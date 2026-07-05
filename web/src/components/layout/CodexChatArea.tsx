import { useCallback, useEffect, useRef, useState } from 'react';
import {
  CxIconChevronDown,
  CxIconClose,
  CxIconCpu,
  CxIconFile,
  CxIconFilm,
  CxIconImage,
  CxIconLoader,
  CxIconPaperclip,
  CxIconPlay,
  CxIconPower,
  CxIconSend,
  CxIconSquare,
  CxIconTerminal,
} from "../icons";
import { invoke } from '@tauri-apps/api/core';
import { useThreadStore, type ChatMessage } from '../../stores/threadStore';
import { OpenClawGateway } from '../../services/openclawGateway';

interface ModelOption { provider: string; providerName: string; model: string; modelName: string; }

const PROVIDER_NAMES: Record<string, string> = {
  kuaifan: '快泛API', openai: 'OpenAI', anthropic: 'Claude', google: 'Gemini',
  deepseek: 'DeepSeek', minimax: 'MiniMax', volcengine: '火山', nvidia: 'NVIDIA',
  aliyun: '阿里通义千问', zhipu: '智谱', moonshot: 'Kimi', grok: 'Grok',
  baidu: '百度文心', xiaomi: 'MiMo', tencent: '腾讯混元', xfyun: '科大讯飞',
};
const ALL_PROVIDER_IDS = Object.keys(PROVIDER_NAMES);

function ModelSelector({ onChanged, currentModel, onSelect }: {
  onChanged?: () => void;
  currentModel: string;
  onSelect: (opt: ModelOption) => void;
}) {
  const [models, setModels] = useState<ModelOption[]>([]);
  const [open, setOpen] = useState(false);

  const loadModels = useCallback(async () => {
    const dm = await invoke<{provider?:string;model_name?:string}>('get_default_model').catch(() => null);
    const defaultEntry = dm?.provider && dm?.model_name
      ? { provider: dm.provider, providerName: PROVIDER_NAMES[dm.provider] || dm.provider, model: dm.model_name, modelName: dm.model_name }
      : null;

    const results = await Promise.allSettled(
      ALL_PROVIDER_IDS.map(id =>
        invoke<any[]>('list_models', { providerId: id, apiKey: null })
          .then(ms => ({ id, models: ms })).catch(() => ({ id, models: [] }))
      )
    );
    const list: ModelOption[] = [];
    for (const r of results) {
      if (r.status !== 'fulfilled') continue;
      const { id, models: ms } = r.value;
      const name = PROVIDER_NAMES[id] || id;
      for (const m of (ms || [])) {
        list.push({ provider: id, providerName: name, model: m.id, modelName: m.name || m.id });
      }
    }
    try {
      const ollamaModels: any[] = await invoke('list_models', { providerId: 'ollama', apiKey: null });
      for (const m of ollamaModels) list.push({ provider: 'ollama', providerName: 'Ollama', model: m.id, modelName: m.name || m.id });
    } catch {}
    if (defaultEntry && !list.some(m => m.provider === defaultEntry.provider && m.model === defaultEntry.model)) {
      list.unshift(defaultEntry);
    }
    if (list.length > 0) setModels(list);
  }, []);

  useEffect(() => { loadModels(); }, []);

  const display = currentModel ? currentModel.split('/').pop() || currentModel : '选择模型';

  return (
    <div className="relative">
      <button onClick={() => { setOpen(!open); if (!open) loadModels(); }}
        className="flex items-center gap-1.5 px-2 h-7 rounded text-[12px] font-medium"
        style={{ background: 'var(--cx-bg-soft)', color: 'var(--cx-text-soft)', border: '1px solid var(--cx-border)' }}>
        <CxIconCpu className="w-3 h-3" style={{ color: 'var(--cx-accent)' }} />
        <span className="max-w-[120px] truncate">{display}</span>
        <CxIconChevronDown className="w-3 h-3" />
      </button>
      {open && (
        <div className="absolute top-full left-0 mt-1 z-50 rounded-lg shadow-lg max-h-64 overflow-y-auto min-w-[220px]"
          style={{ background: 'var(--cx-bg-elev)', border: '1px solid var(--cx-border)' }}>
          {models.length === 0 && (
            <div className="px-3 py-4 text-center text-[12px]" style={{ color: 'var(--cx-text-mute)' }}>正在加载模型列表…</div>
          )}
          {models.map((m,i) => (
            <button key={i} onClick={() => { onSelect(m); setOpen(false); }}
              className="w-full text-left px-3 py-2 text-[12px] hover:opacity-80 flex items-center justify-between"
              style={{ color: `${m.provider}/${m.model}`===currentModel ? 'var(--cx-accent)' : 'var(--cx-text-soft)',
                background: `${m.provider}/${m.model}`===currentModel ? 'var(--cx-accent-soft)' : 'transparent' }}>
              <span className="flex-1 truncate">{m.model}</span>
              <span className="text-[10px] ml-2 opacity-50 shrink-0">{m.providerName}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function getFileIcon(name: string) {
  const ext = name.split('.').pop()?.toLowerCase() || '';
  if (['png','jpg','jpeg','gif','webp','svg'].includes(ext)) return 'image';
  if (['mp4','webm','mov','avi'].includes(ext)) return 'video';
  return 'file';
}

function getMimeFromName(name: string): string {
  const ext = name.split('.').pop()?.toLowerCase() || '';
  const map: Record<string,string> = {
    jpg:'image/jpeg', jpeg:'image/jpeg', png:'image/png', gif:'image/gif',
    webp:'image/webp', svg:'image/svg+xml', mp4:'video/mp4', webm:'video/webm',
    mov:'video/quicktime', avi:'video/x-msvideo', pdf:'application/pdf',
    txt:'text/plain', md:'text/markdown', json:'application/json',
  };
  return map[ext] || 'application/octet-stream';
}

interface Attachment { name: string; path: string; dataUrl?: string; type: 'image'|'video'|'file'; }

export default function CodexChatArea({
  title = '新对话', gatewayRunning = false, gatewayBusy = false, gatewayPort = 0,
  onToggleGateway,
}: { title?: string; gatewayRunning?: boolean; gatewayBusy?: boolean; gatewayPort?: number; onToggleGateway?: () => void }) {
  const [input, setInput] = useState('');
  const [busy, setBusy] = useState(false);
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [dragOver, setDragOver] = useState(false);
  const [currentModel, setCurrentModel] = useState('');
  const scrollRef = useRef<HTMLDivElement>(null);
  const mountedRef = useRef(true);
  const abortRef = useRef<{ abort: () => void } | null>(null);
  const gatewayOnline = gatewayRunning;

  useEffect(() => { mountedRef.current = true; return () => { mountedRef.current = false; }; }, []);

  const threadStore = useThreadStore();
  const activeThread = useThreadStore(s => s.threads.find(t => t.id === s.activeThreadId) ?? null);
  const threadId = activeThread?.id ?? threadStore.activeThreadId ?? '';

  useEffect(() => {
    const { threads } = useThreadStore.getState();
    if (threads.length === 0) useThreadStore.getState().createThread();
    else if (!useThreadStore.getState().activeThreadId) useThreadStore.getState().setActiveThread(threads[0].id);
  }, []);

  // Load current model
  useEffect(() => {
    (async () => {
      const dm = await invoke<{provider?:string;model_name?:string}>('get_default_model').catch(() => null);
      if (dm?.model_name) setCurrentModel(`${dm.provider}/${dm.model_name}`);
    })();
  }, []);

  const messages = activeThread?.messages ?? [];

  const storeAppendMessages = (tid: string, msgs: ChatMessage[]) => {
    const store = useThreadStore.getState();
    const t = store.threads.find(x => x.id === tid);
    if (t) store.updateThread(tid, { messages: [...t.messages, ...msgs] });
  };
  const storeUpdateMessage = (tid: string, msgId: string, updates: Partial<ChatMessage>) => {
    if (!msgId) return;
    const store = useThreadStore.getState();
    const t = store.threads.find(x => x.id === tid);
    if (t) store.updateThread(tid, { messages: t.messages.map(m => m.id === msgId ? { ...m, ...updates } : m) });
  };
  const storeReplaceMessages = (tid: string, msgs: ChatMessage[]) => {
    const store = useThreadStore.getState();
    const t = store.threads.find(x => x.id === tid);
    if (t) store.updateThread(tid, { messages: msgs });
  };

  useEffect(() => { scrollRef.current?.scrollTo(0, scrollRef.current.scrollHeight); }, [messages]);

  const handleModelSelect = async (opt: ModelOption) => {
    setCurrentModel(`${opt.provider}/${opt.model}`);
    try { await invoke('set_default_model', { provider: opt.provider, modelName: opt.model }); } catch {}
  };

  const doChat = async (tid: string, aId: string, userMsg: string, attachmentBlocks?: Array<{ type: string; image_url?: { url: string }; source?: { type: string; data: string; media_type: string } }>) => {
    const port = gatewayPort || 18789;
    let accumulated = '';

    // Read gateway token from openclaw.json
    let gwToken = '';
    try {
      const { invoke } = await import('@tauri-apps/api/core');
      const { readTextFile } = await import('@tauri-apps/plugin-fs');
      const { join } = await import('@tauri-apps/api/path');
      const dataDir: string = await invoke('get_data_dir');
      const cfgPath = await join(dataDir, 'openclaw-cn', 'openclaw.json');
      const content = await readTextFile(cfgPath);
      const cfg = JSON.parse(content);
      gwToken = cfg?.gateway?.auth?.token || '';
    } catch { /* ignore */ }

    const gw = new OpenClawGateway({
      token: gwToken,
      onEvent: (event, payload) => {
        if (event !== 'chat') return;
        const state = payload?.state;
        let content = '';
        if (typeof payload?.message === 'string') {
          content = payload.message;
        } else if (payload?.message?.content) {
          content = typeof payload.message.content === 'string'
            ? payload.message.content
            : JSON.stringify(payload.message.content);
        } else if (payload?.message) {
          content = JSON.stringify(payload.message);
        }

        if (state === 'final' || state === 'finished') {
          const finalContent = content || accumulated || '(空)';
          if (mountedRef.current) {
            storeUpdateMessage(tid, aId, { content: finalContent, status: 'done' });
            const store = useThreadStore.getState();
            const t = store.threads.find(x => x.id === tid);
            if (t) store.updateThread(tid, { lastMessage: finalContent.slice(0, 80), ts: Date.now() });
          }
          gw.stop();
        } else if (state === 'error') {
          const errMsg = payload?.errorMessage ?? payload?.error ?? 'Agent error';
          if (mountedRef.current) storeUpdateMessage(tid, aId, { content: `出错：${errMsg}`, status: 'error' });
          gw.stop();
        } else if (content) {
          accumulated += content;
          if (mountedRef.current) storeUpdateMessage(tid, aId, { content: accumulated, status: 'streaming' });
        }
      },
      onConnected: async () => {
        try {
          await gw.sendChat({ sessionKey: `manager-${tid}`, message: userMsg, attachments: attachmentBlocks });
        } catch (e: any) {
          console.error('[chat] sendChat error:', e);
          if (mountedRef.current) storeUpdateMessage(tid, aId, { content: `出错：${e?.message || e}`, status: 'error' });
          gw.stop();
        }
      },
      onError: (err) => {
        console.error('[chat] gateway error:', err);
        if (mountedRef.current) storeUpdateMessage(tid, aId, { content: `网关连接失败：${err.message}`, status: 'error' });
        if (mountedRef.current) setBusy(false);
      },
      onDisconnected: () => {
        if (mountedRef.current) setBusy(false);
      },
    });

    abortRef.current = { abort: () => gw.stop() };
    await gw.start();
  };

  // Attachment handling
  const fileInputRef = useRef<HTMLInputElement>(null);

  const handleFilesSelected = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = e.target.files;
    if (!files) return;
    const newAttachments: Attachment[] = [];
    for (let i = 0; i < files.length; i++) {
      const f = files[i]; if (!f) continue;
      const t = getFileIcon(f.name) as Attachment['type'];
      try {
        const dataUrl = await new Promise<string>((resolve, reject) => {
          const reader = new FileReader();
          reader.onload = () => resolve(reader.result as string);
          reader.onerror = reject;
          reader.readAsDataURL(f);
        });
        newAttachments.push({ name: f.name, path: (f as any).path || f.name, dataUrl, type: t });
      } catch {}
    }
    setAttachments(prev => [...prev, ...newAttachments]);
    if (fileInputRef.current) fileInputRef.current.value = '';
  };
  const removeAttachment = (idx: number) => setAttachments(prev => prev.filter((_, i) => i !== idx));

  // Send
  const handleSend = async () => {
    const text = input.trim();
    if (!text && attachments.length === 0) return;
    if (busy) return;
    const tid = threadId;
    if (!tid) return;

    if (!gatewayOnline) {
      const gwTs = Date.now();
      storeAppendMessages(tid, [
        { id: `u-${gwTs}`, role: 'user', content: text || '[附件]', ts: gwTs },
        { id: `a-${gwTs+1}`, role: 'assistant', content: '请先启动网关', status: 'error', ts: gwTs+1 },
      ]);
      setInput(''); setAttachments([]); return;
    }

    setInput('');
    const savedAttach = [...attachments];
    setAttachments([]);
    const ts = Date.now(); const uId = `u-${ts}`, aId = `a-${ts+1}`;

    const mediaBlocks = savedAttach.map(a => ({
      media_type: a.type as 'image'|'video'|'file',
      mime: a.type === 'video' ? 'video/mp4' : a.type === 'image' ? 'image/png' : getMimeFromName(a.name),
      data: a.dataUrl?.split(',')[1] || '',
      name: a.name,
    }));

    storeAppendMessages(tid, [
      { id: uId, role: 'user', content: text || '[附件]', ts: Date.now(), media: mediaBlocks },
      { id: aId, role: 'assistant', content: '', status: 'streaming', ts: Date.now() },
    ]);

    const t = useThreadStore.getState().threads.find(x => x.id === tid);
    if (t && t.title === '新对话') useThreadStore.getState().updateThread(tid, { title: text.slice(0, 30) || '附件消息' });

    let fullMessage = text;
    const attachmentBlocks: Array<{ type: string; image_url?: { url: string }; source?: { type: string; data: string; media_type: string } }> = [];
    if (savedAttach.length > 0) {
      for (const a of savedAttach) {
        if (a.type === 'image' && a.dataUrl) {
          attachmentBlocks.push({
            type: 'image_url',
            image_url: { url: a.dataUrl },
          });
        } else if (a.type === 'file' || a.type === 'video') {
          attachmentBlocks.push({
            type: 'image_url',
            image_url: { url: `[附件: ${a.name}]` },
          });
        }
      }
    }

    setBusy(true);
    doChat(tid, aId, fullMessage, attachmentBlocks.length > 0 ? attachmentBlocks : undefined);
  };

  // Abort
  const handleAbort = () => {
    if (abortRef.current) { abortRef.current.abort(); abortRef.current = null; }
    setBusy(false);
  };

  // Drag & drop
  const handleDragOver = (e: React.DragEvent) => {
    e.preventDefault(); e.stopPropagation();
    if (e.dataTransfer.types.includes('Files')) setDragOver(true);
  };
  const handleDragLeave = (e: React.DragEvent) => {
    e.preventDefault(); e.stopPropagation();
    if (e.currentTarget === e.target || !e.currentTarget.contains(e.relatedTarget as Node)) setDragOver(false);
  };
  const handleDrop = async (e: React.DragEvent) => {
    e.preventDefault(); e.stopPropagation();
    setDragOver(false);
    const files = e.dataTransfer.files;
    if (!files || files.length === 0) return;
    const newAttachments: Attachment[] = [];
    for (let i = 0; i < files.length; i++) {
      const f = files[i];
      const t = getFileIcon(f.name) as Attachment['type'];
      try {
        const dataUrl = await new Promise<string>((resolve, reject) => {
          const reader = new FileReader();
          reader.onload = () => resolve(reader.result as string);
          reader.onerror = reject;
          reader.readAsDataURL(f);
        });
        newAttachments.push({ name: f.name, path: (f as any).path || f.name, dataUrl, type: t });
      } catch {}
    }
    setAttachments(prev => [...prev, ...newAttachments]);
  };

  const fontFamily = { fontFamily: 'system-ui,-apple-system,"Segoe UI","PingFang SC","Microsoft YaHei",sans-serif' };

  return (
    <section
      className="flex flex-col h-full relative"
      style={{ background: 'var(--cx-bg)' }}
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
    >
      {dragOver && (
        <div
          className="absolute inset-0 z-40 flex items-center justify-center pointer-events-none cx-animate-fade-in"
          style={{
            background: 'rgba(91,127,189,0.10)',
            backdropFilter: 'blur(2px)',
          }}
        >
          <div
            className="text-center px-10 py-7 rounded-2xl"
            style={{
              background: 'var(--cx-bg-elev)',
              border: '1px solid var(--cx-border)',
              boxShadow: 'var(--cx-shadow-lg)',
            }}
          >
            <div
              className="w-12 h-12 mx-auto mb-3 rounded-xl flex items-center justify-center"
              style={{ background: 'var(--cx-accent-soft)' }}
            >
              <CxIconImage className="w-6 h-6" style={{ color: 'var(--cx-accent)' }} strokeWidth={1.75} />
            </div>
            <div className="text-[15px] font-semibold" style={{ color: 'var(--cx-text)' }}>
              松开以添加文件
            </div>
            <div className="text-[12px] mt-1" style={{ color: 'var(--cx-text-mute)' }}>
              支持图片、视频、文档
            </div>
          </div>
        </div>
      )}

      {/* Header */}
      <div
        className="h-11 px-4 flex items-center justify-between shrink-0 backdrop-blur-md"
        style={{
          borderBottom: '1px solid var(--cx-border-soft)',
          background: 'var(--cx-topbar-bg)',
        }}
      >
        <div className="flex items-center gap-2.5">
          <div
            className="w-6 h-6 rounded-md flex items-center justify-center shrink-0"
            style={{ background: 'var(--cx-accent-soft)' }}
          >
            <CxIconTerminal className="w-3.5 h-3.5" style={{ color: 'var(--cx-accent)' }} strokeWidth={2.25} />
          </div>
          <span className="text-[13.5px] font-semibold" style={{ color: 'var(--cx-text)' }}>
            {title}
          </span>
          <ModelSelector currentModel={currentModel} onSelect={handleModelSelect} />
          {gatewayOnline && (
            <span
              className="flex items-center gap-1 text-[11px] font-medium px-1.5 py-0.5 rounded-md ml-1"
              style={{
                background: 'rgba(74,158,92,0.10)',
                color: 'var(--cx-success)',
              }}
            >
              <span
                className="w-1.5 h-1.5 rounded-full cx-animate-blink"
                style={{ background: 'var(--cx-success)' }}
              />
              已连接
            </span>
          )}
        </div>
        <div className="flex items-center gap-1.5">
          {onToggleGateway && (
            <button
              onClick={onToggleGateway}
              disabled={gatewayBusy}
              className="flex items-center gap-1.5 px-2.5 h-7 rounded-md text-[12px] font-medium transition-all duration-150 disabled:opacity-50"
              style={{
                background: gatewayRunning ? 'rgba(74,158,92,0.10)' : 'rgba(200,85,74,0.08)',
                color: gatewayRunning ? 'var(--cx-success)' : 'var(--cx-error)',
                border: `1px solid ${gatewayRunning ? 'rgba(74,158,92,0.22)' : 'rgba(200,85,74,0.18)'}`,
              }}
            >
              {gatewayBusy ? (
                <CxIconLoader className="w-3 h-3 animate-spin" />
              ) : gatewayRunning ? (
                <CxIconPlay className="w-3 h-3" style={{ fill: 'currentColor' }} />
              ) : (
                <CxIconPower className="w-3 h-3" />
              )}
              <span>{gatewayRunning ? '运行中' : '已停止'}</span>
            </button>
          )}
        </div>
      </div>

      {/* Messages */}
      <div ref={scrollRef} className="flex-1 overflow-y-auto cx-scroll-slim px-4 py-5 space-y-4">
        {Array.isArray(messages) && messages.map((m) => {
          if (!m) return null;
          const isUser = m.role === 'user';
          return (
            <div
              key={m.id}
              className={`flex ${isUser ? 'justify-end' : 'justify-start'} cx-animate-fade-in`}
            >
              <div className={`max-w-[85%] ${isUser ? 'order-1' : ''}`}>
                <div className={`flex items-center gap-2 mb-1 ${isUser ? 'justify-end' : ''}`}>
                  <span
                    className="text-[10.5px] font-semibold uppercase tracking-[0.06em]"
                    style={{
                      color: isUser ? 'var(--cx-accent)' : 'var(--cx-text-mute)',
                      ...fontFamily,
                    }}
                  >
                    {isUser ? 'You' : 'Assistant'}
                  </span>
                </div>
                <div
                  className="whitespace-pre-wrap leading-relaxed rounded-2xl px-4 py-2.5"
                  style={{
                    fontSize: '14.5px',
                    fontWeight: 400,
                    ...fontFamily,
                    background: isUser ? 'var(--cx-accent-soft)' : 'var(--cx-bg-elev)',
                    color: 'var(--cx-text)',
                    border: isUser
                      ? '1px solid rgba(91,127,189,0.22)'
                      : '1px solid var(--cx-border-soft)',
                    borderTopRightRadius: isUser ? '4px' : undefined,
                    borderTopLeftRadius: !isUser ? '4px' : undefined,
                    boxShadow: isUser ? 'none' : 'var(--cx-shadow-xs)',
                  }}
                >
                  {m.media && m.media.length > 0 && (
                    <div className="mb-2 flex flex-wrap gap-2">
                      {m.media.map((med, i) => {
                        if (!med) return null;
                        return med.media_type === 'image' ? (
                          <img
                            key={i}
                            src={`data:${med.mime};base64,${med.data}`}
                            className="max-w-[200px] max-h-[200px] rounded-lg object-cover"
                            alt=""
                          />
                        ) : med.media_type === 'video' ? (
                          <video
                            key={i}
                            controls
                            className="max-w-[200px] max-h-[200px] rounded-lg"
                          >
                            <source src={`data:${med.mime};base64,${med.data}`} type={med.mime} />
                          </video>
                        ) : med.media_type === 'file' ? (
                          <div
                            key={i}
                            className="flex items-center gap-2 px-3 py-2 rounded-lg text-[13px]"
                            style={{
                              background: 'var(--cx-bg-soft)',
                              border: '1px solid var(--cx-border-soft)',
                            }}
                          >
                            <CxIconFile className="w-4 h-4 shrink-0" style={{ color: 'var(--cx-text-mute)' }} />
                            <span className="truncate max-w-[160px]" style={{ color: 'var(--cx-text-soft)' }}>
                              {med.name || '文件'}
                            </span>
                          </div>
                        ) : null;
                      })}
                    </div>
                  )}
                  {m.status === 'streaming' && !m.content ? (
                    <span className="inline-flex items-center gap-2 opacity-60">
                      <CxIconLoader className="w-4 h-4 animate-spin" />
                      思考中…
                    </span>
                  ) : (
                    m.content
                  )}
                  {m.status === 'streaming' && m.content && (
                    <span
                      className="inline-block w-1.5 h-4 ml-0.5 align-text-bottom animate-pulse"
                      style={{ background: 'var(--cx-accent)' }}
                    />
                  )}
                </div>
              </div>
            </div>
          );
        })}

        {messages.length === 0 && !gatewayOnline && (
          <div className="h-full flex flex-col items-center justify-center text-center pt-8 px-4">
            <div
              className="w-14 h-14 rounded-2xl flex items-center justify-center mb-4"
              style={{
                background: 'linear-gradient(135deg, var(--cx-accent-soft), rgba(91,127,189,0.04))',
                border: '1px solid var(--cx-border-soft)',
                boxShadow: 'var(--cx-shadow-sm)',
              }}
            >
              <CxIconTerminal className="w-6 h-6" style={{ color: 'var(--cx-accent)' }} strokeWidth={1.75} />
            </div>
            <div className="text-[15px] font-semibold mb-1" style={{ color: 'var(--cx-text)' }}>
              网关尚未启动
            </div>
            <div
              className="text-[12.5px] mb-5 max-w-[360px] leading-relaxed"
              style={{ color: 'var(--cx-text-mute)' }}
            >
              启动 OpenClaw 网关后即可与 AI 模型对话，支持多平台接入与插件管理。
            </div>
            {onToggleGateway && (
              <button
                onClick={onToggleGateway}
                disabled={gatewayBusy}
                className="inline-flex items-center gap-2 px-5 h-9 rounded-lg text-[13px] font-medium transition-all duration-150"
                style={{
                  background: gatewayBusy
                    ? 'var(--cx-bg-hover)'
                    : 'linear-gradient(180deg, var(--cx-accent) 0%, var(--cx-accent-hover) 100%)',
                  color: gatewayBusy ? 'var(--cx-text-mute)' : '#fff',
                  boxShadow: gatewayBusy ? 'none' : '0 1px 2px rgba(91,127,189,0.25), inset 0 1px 0 rgba(255,255,255,0.18)',
                }}
              >
                {gatewayBusy ? (
                  <>
                    <CxIconLoader className="w-4 h-4 animate-spin" /> 启动中…
                  </>
                ) : (
                  <>
                    <CxIconPlay className="w-4 h-4" style={{ fill: 'currentColor' }} /> 一键启动网关
                  </>
                )}
              </button>
            )}
          </div>
        )}
      </div>

      {/* Attachments preview */}
      {attachments.length > 0 && (
        <div className="px-4 pb-1 flex gap-2 flex-wrap">
          {attachments.map((a, i) => (
            <div
              key={i}
              className="relative group flex items-center gap-1.5 pl-1.5 pr-2.5 py-1.5 rounded-lg text-[12px] transition-all duration-150"
              style={{
                background: 'var(--cx-bg-elev)',
                border: '1px solid var(--cx-border-soft)',
                boxShadow: 'var(--cx-shadow-xs)',
                ...fontFamily,
              }}
            >
              {a.type === 'image' && a.dataUrl ? (
                <img src={a.dataUrl} alt={a.name} className="w-7 h-7 rounded object-cover" />
              ) : a.type === 'video' ? (
                <CxIconFilm className="w-3.5 h-3.5 ml-1" style={{ color: 'var(--cx-text-mute)' }} />
              ) : (
                <CxIconFile className="w-3.5 h-3.5 ml-1" style={{ color: 'var(--cx-text-mute)' }} />
              )}
              <span className="max-w-[110px] truncate" style={{ color: 'var(--cx-text-soft)' }}>
                {a.name}
              </span>
              <button
                onClick={() => removeAttachment(i)}
                className="w-4 h-4 rounded-full flex items-center justify-center opacity-0 group-hover:opacity-100 transition-opacity"
                style={{
                  background: 'var(--cx-bg-soft)',
                  color: 'var(--cx-text-mute)',
                  border: '1px solid var(--cx-border-soft)',
                }}
                aria-label="移除附件"
              >
                <CxIconClose className="w-2.5 h-2.5" />
              </button>
            </div>
          ))}
        </div>
      )}

      {/* Input */}
      <div className="px-4 pb-3 pt-1 shrink-0">
        <div
          className="flex flex-col rounded-xl transition-shadow duration-200"
          style={{
            background: 'var(--cx-bg-soft)',
            border: '1px solid var(--cx-border)',
            boxShadow: 'var(--cx-shadow-xs)',
          }}
        >
          <div className="flex items-end gap-2 px-3 pt-2.5">
            <button
              onClick={() => fileInputRef.current?.click()}
              className="p-1.5 rounded-md shrink-0 transition-all duration-150"
              style={{ color: 'var(--cx-text-mute)' }}
              onMouseEnter={(e) => {
                e.currentTarget.style.color = 'var(--cx-accent)';
                e.currentTarget.style.background = 'var(--cx-bg-hover)';
              }}
              onMouseLeave={(e) => {
                e.currentTarget.style.color = 'var(--cx-text-mute)';
                e.currentTarget.style.background = 'transparent';
              }}
              title="上传文件"
              aria-label="上传文件"
            >
              <CxIconPaperclip className="w-4 h-4" />
            </button>
            <input
              ref={fileInputRef}
              type="file"
              multiple
              accept="*/*"
              onChange={handleFilesSelected}
              className="hidden"
            />
            <textarea
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && !e.shiftKey) {
                  e.preventDefault();
                  handleSend();
                }
              }}
              placeholder={gatewayOnline ? '输入消息…' : '请先启动网关'}
              rows={1}
              className="flex-1 bg-transparent outline-none resize-none py-1.5 max-h-32"
              style={{
                fontSize: '14.5px',
                fontWeight: 400,
                color: 'var(--cx-text)',
                ...fontFamily,
              }}
            />
            {busy ? (
              <button
                onClick={handleAbort}
                className="px-3 h-8 rounded-md text-[12px] flex items-center gap-1 shrink-0 font-medium transition-all duration-150"
                style={{
                  background: 'rgba(200,85,74,0.10)',
                  color: 'var(--cx-error)',
                  border: '1px solid rgba(200,85,74,0.22)',
                }}
                title="停止"
              >
                <CxIconSquare className="w-3 h-3" style={{ fill: 'currentColor' }} />
              </button>
            ) : (
              <button
                onClick={handleSend}
                disabled={(!input.trim() && attachments.length === 0) || !gatewayOnline}
                className="px-2.5 h-8 rounded-md text-[12px] flex items-center gap-1 shrink-0 font-medium transition-all duration-150 disabled:opacity-40"
                style={{
                  background:
                    !input.trim() && attachments.length === 0
                      ? 'var(--cx-bg-hover)'
                      : 'var(--cx-accent)',
                  color:
                    !input.trim() && attachments.length === 0
                      ? 'var(--cx-text-mute)'
                      : '#fff',
                }}
                title="发送 (Enter)"
              >
                <CxIconSend className="w-3.5 h-3.5" />
              </button>
            )}
          </div>
          <div className="px-3 pb-2 pt-0.5 flex items-center gap-3">
            <span className="text-[10.5px]" style={{ color: 'var(--cx-text-dim)' }}>
              <kbd
                className="font-mono px-1 py-0.5 rounded text-[9.5px]"
                style={{
                  background: 'var(--cx-bg-elev)',
                  border: '1px solid var(--cx-border-soft)',
                }}
              >
                Enter
              </kbd>{' '}
              发送 · Shift+Enter 换行 · 上传/拖拽文件
            </span>
          </div>
        </div>
      </div>
    </section>
  );
}
