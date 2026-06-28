import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import toast from "react-hot-toast";
import {
  CxIconAlertCircle,
  CxIconCheck,
  CxIconCheckCircle,
  CxIconCpu,
  CxIconExternalLink,
  CxIconEye,
  CxIconEyeOff,
  CxIconFilter,
  CxIconKey,
  CxIconLayers,
  CxIconLoader,
  CxIconSearch,
  CxIconServer,
  CxIconShield,
  CxIconSparkles,
  CxIconWifi,
  CxIconXCircle,
} from "../icons";

interface Provider {
  id: string;
  name: string;
  enabled: boolean;
  api_key_configured: boolean;
  free_models_count: number;
  total_models_count: number;
}

interface ModelEntry {
  id: string;
  name: string;
  context_window: number | null;
  is_free: boolean;
  badge: string | null;
}

interface Props {
  onNext: () => void;
  onPrev: () => void;
}

const C = {
  bg: "var(--cx-bg)",
  bgSoft: "var(--cx-bg-soft)",
  bgElev: "var(--cx-bg-elev)",
  bgHover: "var(--cx-bg-hover)",
  bgOverlay: "var(--cx-bg-overlay)",
  text: "var(--cx-text)",
  textSoft: "var(--cx-text-soft)",
  textMute: "var(--cx-text-mute)",
  textDim: "var(--cx-text-dim)",
  border: "var(--cx-border)",
  borderSoft: "var(--cx-border-soft)",
  borderElev: "var(--cx-border-elev)",
  accent: "var(--cx-accent)",
  accentHover: "var(--cx-accent-hover)",
  accentSoft: "var(--cx-accent-soft)",
  accentRing: "var(--cx-accent-ring)",
  success: "var(--cx-success)",
  successSoft: "var(--cx-success-soft)",
  warn: "var(--cx-warn)",
  warnSoft: "var(--cx-warn-soft)",
  error: "var(--cx-error)",
  errorSoft: "var(--cx-error-soft)",
} as const;

function contextLabel(ctx: number | null): string {
  if (!ctx) return "";
  if (ctx >= 1000000) return (ctx / 1000000).toFixed(ctx % 1000000 === 0 ? 0 : 1) + "M";
  if (ctx >= 1000) return (ctx / 1000).toFixed(0) + "K";
  return String(ctx);
}

const PROVIDER_DOCS: Record<string, { href: string; label: string }> = {
  kuaifan: { href: "https://kuaifanio.cn", label: "快泛API 官网" },
  openai: { href: "https://platform.openai.com/api-keys", label: "OpenAI 控制台" },
  anthropic: { href: "https://console.anthropic.com/", label: "Anthropic 控制台" },
  google: { href: "https://aistudio.google.com/apikey", label: "Google AI Studio" },
  deepseek: { href: "https://platform.deepseek.com", label: "DeepSeek 控制台（国内）" },
  minimax: { href: "https://platform.minimaxi.com", label: "MiniMax 平台（国内）" },
  volc_ark: { href: "https://console.volcengine.com/ark", label: "火山方舟控制台（国内）" },
  nvidia: { href: "https://build.nvidia.com/", label: "NVIDIA Build" },
  aliyun: { href: "https://dashscope.console.aliyun.com/", label: "阿里云百炼（国内）" },
  zhipu: { href: "https://open.bigmodel.cn/", label: "智谱开放平台（国内）" },
  moonshot: { href: "https://platform.moonshot.cn/", label: "Kimi 开放平台（国内）" },
  grok: { href: "https://console.x.ai/", label: "xAI Console" },
  baidu: { href: "https://console.bce.baidu.com/qianfan/", label: "百度千帆（国内）" },
  xiaomi: { href: "https://platform.xiaomi.com/", label: "小米 MiMo 平台（国内）" },
  tencent: { href: "https://console.cloud.tencent.com/hunyuan", label: "腾讯混元控制台（国内）" },
  xfyun: { href: "https://console.xfyun.cn/", label: "讯飞星火控制台（国内）" },
};

const PROVIDER_GROUP: Record<string, { tag: string; isLocal?: boolean }> = {
  kuaifan: { tag: "内置" },
  openai: { tag: "海外" },
  anthropic: { tag: "海外" },
  google: { tag: "海外" },
  grok: { tag: "海外" },
  nvidia: { tag: "海外" },
  ollama: { tag: "本地", isLocal: true },
  deepseek: { tag: "国内" },
  minimax: { tag: "国内" },
  volc_ark: { tag: "国内" },
  aliyun: { tag: "国内" },
  zhipu: { tag: "国内" },
  moonshot: { tag: "国内" },
  baidu: { tag: "国内" },
  xiaomi: { tag: "国内" },
  tencent: { tag: "国内" },
  xfyun: { tag: "国内" },
};

function providerAccent(id: string): { fg: string; bg: string } {
  const map: Record<string, { fg: string; bg: string }> = {
    kuaifan: { fg: C.accent, bg: C.accentSoft },
    openai: { fg: "#10a37f", bg: "rgba(16,163,127,0.10)" },
    anthropic: { fg: "#cd6336", bg: "rgba(205,99,54,0.10)" },
    google: { fg: "#4285f4", bg: "rgba(66,133,244,0.10)" },
    deepseek: { fg: "#5b6cd9", bg: "rgba(91,108,217,0.10)" },
    minimax: { fg: "#7a4ec5", bg: "rgba(122,78,197,0.10)" },
    volc_ark: { fg: "#e36c2e", bg: "rgba(227,108,46,0.10)" },
    nvidia: { fg: "#76b900", bg: "rgba(118,185,0,0.10)" },
    aliyun: { fg: "#ff6a00", bg: "rgba(255,106,0,0.10)" },
    zhipu: { fg: "#3859ff", bg: "rgba(56,89,255,0.10)" },
    moonshot: { fg: "#1f1f1f", bg: "rgba(31,31,31,0.08)" },
    grok: { fg: "#1f1f1f", bg: "rgba(31,31,31,0.08)" },
    baidu: { fg: "#2932e1", bg: "rgba(41,50,225,0.10)" },
    xiaomi: { fg: "#ff5a00", bg: "rgba(255,90,0,0.10)" },
    tencent: { fg: "#007bff", bg: "rgba(0,123,255,0.10)" },
    xfyun: { fg: "#1ba0e2", bg: "rgba(27,160,226,0.10)" },
    ollama: { fg: C.textSoft, bg: C.bgSoft },
  };
  return map[id] || { fg: C.accent, bg: C.accentSoft };
}

const VOLC_CUSTOM_EP = "__volc_custom_ep__";

function SectionCard({
  icon: Icon, title, desc, right, children, contentClassName,
}: {
  icon?: any; title: string; desc?: string; right?: React.ReactNode; children: React.ReactNode; contentClassName?: string;
}) {
  return (
    <section
      className="rounded-xl overflow-hidden cx-animate-fade-in"
      style={{ background: C.bgElev, border: "1px solid " + C.borderSoft, boxShadow: "var(--cx-shadow-xs)" }}
    >
      <div
        className="flex items-center justify-between px-5 py-3.5"
        style={{ borderBottom: "1px solid " + C.borderSoft }}
      >
        <div className="flex items-center gap-2.5 min-w-0">
          {Icon && (
            <div
              className="w-7 h-7 rounded-lg flex items-center justify-center shrink-0"
              style={{ background: C.accentSoft, color: C.accent }}
            >
              <Icon className="w-3.5 h-3.5" strokeWidth={2} />
            </div>
          )}
          <div className="min-w-0">
            <h2 className="text-[13.5px] font-semibold leading-tight" style={{ color: C.text }}>
              {title}
            </h2>
            {desc && (
              <p className="text-[11px] mt-0.5 leading-tight" style={{ color: C.textMute }}>
                {desc}
              </p>
            )}
          </div>
        </div>
        {right}
      </div>
      <div className={"p-5 " + (contentClassName || "")}>{children}</div>
    </section>
  );
}

function StatusBadge({ tone, children }: { tone: "success" | "warn" | "error" | "info" | "neutral"; children: React.ReactNode }) {
  const map = {
    success: { bg: C.successSoft, fg: C.success },
    warn: { bg: C.warnSoft, fg: C.warn },
    error: { bg: C.errorSoft, fg: C.error },
    info: { bg: C.accentSoft, fg: C.accent },
    neutral: { bg: C.bgSoft, fg: C.textMute },
  };
  const c = map[tone] || map.neutral;
  return (
    <span
      className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10.5px] font-medium whitespace-nowrap"
      style={{ background: c.bg, color: c.fg }}
    >
      {children}
    </span>
  );
}
function ProviderRail({
  providers, selectedId, configuredMap, onSelect, loading,
}: {
  providers: Provider[]; selectedId: string; configuredMap: Record<string, boolean>; onSelect: (id: string) => void; loading: boolean;
}) {
  const [keyword, setKeyword] = useState("");
  const filtered = useMemo(() => {
    const k = keyword.trim().toLowerCase();
    if (!k) return providers;
    return providers.filter(
      (p: Provider) => p.name.toLowerCase().includes(k) || p.id.toLowerCase().includes(k),
    );
  }, [providers, keyword]);

  const configuredCount = Object.values(configuredMap).filter(Boolean).length;

  const groups = useMemo(() => {
    const buckets: Record<string, Provider[]> = { "内置": [], "海外": [], "国内": [], "本地": [] };
    filtered.forEach((p) => {
      const g = PROVIDER_GROUP[p.id]?.tag || "海外";
      if (!buckets[g]) buckets[g] = [];
      buckets[g].push(p);
    });
    return buckets;
  }, [filtered]);

  const groupOrder: Array<{ key: string; label: string }> = [
    { key: "内置", label: "内置" },
    { key: "海外", label: "海外" },
    { key: "国内", label: "国内" },
    { key: "本地", label: "本地" },
  ];

  return (
    <aside
      className="w-[264px] shrink-0 rounded-xl overflow-hidden flex flex-col sticky top-0 max-h-[calc(100vh-9rem)]"
      style={{ background: C.bgElev, border: "1px solid " + C.borderSoft, boxShadow: "var(--cx-shadow-xs)" }}
    >
      <div className="px-4 pt-3.5 pb-3" style={{ borderBottom: "1px solid " + C.borderSoft }}>
        <div className="flex items-center justify-between mb-2.5">
          <div className="flex items-center gap-2">
            <div
              className="w-6 h-6 rounded-md flex items-center justify-center"
              style={{ background: C.accentSoft, color: C.accent }}
            >
              <CxIconLayers className="w-3.5 h-3.5" strokeWidth={2} />
            </div>
            <h3 className="text-[12.5px] font-semibold tracking-tight" style={{ color: C.text }}>模型供应商</h3>
          </div>
          <span className="text-[10.5px] px-1.5 py-0.5 rounded-full font-medium" style={{ background: C.bgSoft, color: C.textMute }}>
            {providers.length}
          </span>
        </div>

        <div className="relative flex items-center" style={{ background: C.bgSoft, border: "1px solid " + C.borderSoft, borderRadius: "8px" }}>
          <CxIconSearch className="w-3.5 h-3.5 absolute left-2.5" style={{ color: C.textDim }} />
          <input
            type="text"
            value={keyword}
            onChange={(e) => setKeyword(e.target.value)}
            placeholder="搜索供应商"
            className="w-full pl-8 pr-2.5 py-1.5 text-[12px] rounded-lg outline-none"
            style={{ background: "transparent", color: C.text }}
          />
        </div>

        <div className="mt-2.5 flex items-center gap-1.5 text-[10.5px]" style={{ color: C.textMute }}>
          <span className="inline-block w-1.5 h-1.5 rounded-full" style={{ background: C.success, boxShadow: "0 0 0 3px " + C.successSoft }} />
          <span>已配置 {configuredCount} 家</span>
          <span style={{ color: C.textDim }}>·</span>
          <span>共 {providers.length} 家可选</span>
        </div>
      </div>

      <div className="flex-1 overflow-y-auto cx-scroll-slim py-2 min-h-[320px]">
        {loading ? (
          <div className="p-4 space-y-2">
          {Array.from({ length: 8 }).map((_, i) => (
            <div key={i} className="cx-shimmer h-11 rounded-lg" style={{ background: C.bgSoft }} />
          ))}
          </div>
        ) : filtered.length === 0 ? (
          <div className="px-4 py-10 text-center" style={{ color: C.textMute }}>
            <CxIconFilter className="w-5 h-5 mx-auto mb-1.5" style={{ color: C.textDim }} />
            <p className="text-[11.5px]">没有匹配的供应商</p>
          </div>
        ) : (
          groupOrder.map((g) => {
            const list = groups[g.key];
            if (!list || list.length === 0) return null;
            return (
              <div key={g.key} className="mb-1">
                <div className="px-4 pt-2 pb-1.5 text-[10px] font-semibold uppercase tracking-wider" style={{ color: C.textDim }}>
                  {g.label}
                </div>
                {list.map((p) => {
                  const active = selectedId === p.id;
                  const configured = configuredMap[p.id];
                  const isLocal = PROVIDER_GROUP[p.id]?.isLocal;
                  const accent = providerAccent(p.id);
                  return (
                    <button
                      key={p.id}
                      type="button"
                      onClick={() => onSelect(p.id)}
                      className="text-left px-3 py-2 mx-2 rounded-lg flex items-center gap-2.5 transition-colors duration-150 relative"
                      style={{ width: "calc(100% - 16px)", background: active ? C.accentSoft : "transparent", color: active ? C.text : C.textSoft }}
                      onMouseEnter={(e) => { if (!active) e.currentTarget.style.background = C.bgHover; }}
                      onMouseLeave={(e) => { if (!active) e.currentTarget.style.background = "transparent"; }}
                    >
                      <span
                        className="absolute left-0 top-1/2 -translate-y-1/2 rounded-full"
                        style={{ width: 2.5, height: active ? 18 : 0, background: C.accent, transition: "height 0.18s var(--cx-ease-out)" }}
                      />
                      <div
                        className="w-7 h-7 rounded-md flex items-center justify-center shrink-0 text-[11px] font-semibold tracking-tight"
                        style={{ background: active ? C.bgElev : (isLocal ? C.bgSoft : accent.bg), color: active ? C.accent : accent.fg, boxShadow: "inset 0 0 0 1px " + (active ? C.accent : (isLocal ? C.borderSoft : "transparent")) }}
                      >
                        {isLocal ? <CxIconServer className="w-3.5 h-3.5" strokeWidth={2} /> : p.name.charAt(0)}
                      </div>
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center gap-1.5 min-w-0">
                          <span className="text-[12.5px] font-medium truncate" style={{ color: active ? C.text : C.textSoft }}>
                            {p.name}
                          </span>
                        </div>
                        <div className="flex items-center gap-1 mt-0.5 text-[10.5px]" style={{ color: C.textMute }}>
                          {isLocal ? (
                            <CxIconServer className="w-2.5 h-2.5" />
                          ) : configured ? (
                            <span className="inline-block w-1.5 h-1.5 rounded-full" style={{ background: C.success }} />
                          ) : (
                            <span className="inline-block w-1.5 h-1.5 rounded-full" style={{ background: C.borderElev }} />
                          )}
                          <span>
                            {isLocal ? "本地服务" : configured ? "已配置 Key" : "未配置 Key"}
                          </span>
                        </div>
                      </div>
                    </button>
                  );
                })}
              </div>
            );
          })
        )}
      </div>
    </aside>
  );
}
function ModelCard({
  m, active, onClick, accentTone,
}: {
  m: ModelEntry; active: boolean; onClick: () => void; accentTone?: "info" | "warn";
}) {
  const ctx = contextLabel(m.context_window);
  const tone = accentTone || "info";
  const accentBg = tone === "warn" ? C.warnSoft : C.accentSoft;
  const accentFg = tone === "warn" ? C.warn : C.accent;
  const accentBorder = tone === "warn" ? C.warn : C.accent;
  const accentShadow = tone === "warn" ? "0 1px 3px rgba(196,136,60,0.18)" : "0 1px 3px rgba(91,127,189,0.18)";
  return (
    <button
      type="button"
      onClick={onClick}
      className="relative w-full text-left rounded-lg transition-all duration-200 group"
      style={{ background: C.bgElev, border: "1px solid " + (active ? accentBorder : C.borderSoft), boxShadow: active ? accentShadow : "var(--cx-shadow-xs)", padding: "12px 14px" }}
      onMouseEnter={(e) => { if (!active) { e.currentTarget.style.borderColor = C.borderElev; e.currentTarget.style.boxShadow = "0 1px 2px rgba(44,36,22,0.06)"; } }}
      onMouseLeave={(e) => { if (!active) { e.currentTarget.style.borderColor = C.borderSoft; e.currentTarget.style.boxShadow = "var(--cx-shadow-xs)"; } }}
    >
      {active && (
        <span className="absolute top-2.5 right-2.5 w-4 h-4 rounded-full flex items-center justify-center" style={{ background: accentFg }}>
          <CxIconCheck className="w-2.5 h-2.5 text-white" strokeWidth={3} />
        </span>
      )}
      <div className="flex items-center gap-2 mb-1.5 pr-6">
        <span className="text-[13px] font-semibold leading-tight truncate tracking-tight" style={{ color: C.text }}>
          {m.name}
        </span>
        {m.is_free && <StatusBadge tone="success">免费</StatusBadge>}
        {m.badge && (!m.is_free || m.badge !== "免费") && (
          <StatusBadge tone="neutral">{m.badge}</StatusBadge>
        )}
      </div>
      <div className="flex items-center gap-3 flex-wrap">
        <code
          className="text-[11px] font-mono truncate max-w-full"
          style={{ color: active ? accentFg : C.textMute, background: active ? accentBg : "transparent", padding: active ? "1px 6px" : "0", borderRadius: "4px" }}
        >
          {m.id}
        </code>
        {ctx && (
          <span className="inline-flex items-center gap-1 text-[10.5px]" style={{ color: C.textDim }}>
            <CxIconLayers className="w-3 h-3" />
            上下文 {ctx}
          </span>
        )}
      </div>
    </button>
  );
}
export default function ModelConfig({ onNext, onPrev }: Props) {
  const [providers, setProviders] = useState<Provider[]>([]);
  const [loading, setLoading] = useState(true);
  const [selectedProvider, setSelectedProvider] = useState<string>("kuaifan");
  const [apiKey, setApiKey] = useState("");
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ success: boolean; message: string } | null>(null);
  const [selectedModel, setSelectedModel] = useState("");
  const [setDefault, setSetDefault] = useState(false);

  const [models, setModels] = useState<ModelEntry[]>([]);
  const [modelsLoading, setModelsLoading] = useState(false);
  const [modelsError, setModelsError] = useState<string | null>(null);
  const [volcCustomEpId, setVolcCustomEpId] = useState("");
  const [hasStoredKey, setHasStoredKey] = useState(false);
  const [providerReady, setProviderReady] = useState(false);

  const [currentDefault, setCurrentDefault] = useState<{ provider: string; model: string } | null>(null);

  const [proxyUrl, setProxyUrl] = useState("");
  const [proxyUsername, setProxyUsername] = useState("");
  const [proxyPassword, setProxyPassword] = useState("");

  const [showKey, setShowKey] = useState<boolean>(false);
  const [proxyOpen, setProxyOpen] = useState<boolean>(false);

  useEffect(() => {
    loadProviders();
  }, []);

  useEffect(() => {
    if (!providerReady) return;
    loadStoredKeyAndModels();
  }, [providerReady, selectedProvider]);

  const loadStoredKeyAndModels = async () => {
    setHasStoredKey(false);
    setVolcCustomEpId("");
    setModelsLoading(true);
    setModelsError(null);
    setModels([]);
    setSelectedModel("");
    setTestResult(null);

    try {
      const [cfg, defaultModel] = await Promise.all([
        invoke<{ api_key?: string; proxy_url?: string; proxy_username?: string; proxy_password?: string }>("get_provider_config", { providerId: selectedProvider }),
        invoke<{ provider?: string; model_name?: string }>("get_default_model", {}),
      ]);
      const stored = cfg?.api_key || "";
      setApiKey(stored);
      setHasStoredKey(stored.length > 0);
      setProxyUrl(cfg?.proxy_url || "");
      setProxyUsername(cfg?.proxy_username || "");
      setProxyPassword(cfg?.proxy_password || "");
      setProxyOpen(Boolean(cfg?.proxy_url));

      const dm = defaultModel?.provider && defaultModel?.model_name
        ? { provider: defaultModel.provider, model: defaultModel.model_name }
        : null;
      setCurrentDefault(dm);

      if (dm && dm.provider === selectedProvider) {
        setSetDefault(true);
      } else {
        setSetDefault(false);
      }

      setModelsLoading(false);
    } catch {
      setApiKey("");
      setHasStoredKey(false);
      setCurrentDefault(null);
      setModelsLoading(false);
    }
  };

  useEffect(() => {
    if (!providerReady) return;
    loadModels();
  }, [selectedProvider, apiKey, providerReady, currentDefault]);

  const loadProviders = async () => {
    setLoading(true);
    try {
      const result = await invoke<Provider[]>("list_providers");
      setProviders(result);
      setProviderReady(true);
    } catch (e) {
      console.error("Load providers error:", e);
    }
    setLoading(false);
  };

  const loadModels = async () => {
    setModelsLoading(true);
    setModelsError(null);
    setModels([]);
    const prevSelected = selectedModel;
    const prevVolcEp = volcCustomEpId;

    try {
      const result = await invoke<ModelEntry[]>("list_models", {
        providerId: selectedProvider,
        apiKey: apiKey || null,
      });
      setModels(result);

      let nextId = "";
      let nextVolc = "";

      if (prevSelected && result.some((m) => m.id === prevSelected)) {
        nextId = prevSelected;
        if (prevSelected === VOLC_CUSTOM_EP) nextVolc = prevVolcEp;
      } else if (currentDefault && currentDefault.provider === selectedProvider) {
        const match = result.find((m) => m.id === currentDefault.model);
        if (match) {
          nextId = match.id;
        } else if (selectedProvider === "volc_ark" && currentDefault.model.startsWith("ep-")) {
          nextId = VOLC_CUSTOM_EP;
          nextVolc = currentDefault.model;
        }
      }

      setSelectedModel(nextId);
      setVolcCustomEpId(nextVolc);
    } catch (e) {
      setModelsError(String(e));
      setSelectedModel("");
      setVolcCustomEpId("");
    } finally {
      setModelsLoading(false);
    }
  };

  const resolvedModelName =
    selectedProvider === "volc_ark" && selectedModel === VOLC_CUSTOM_EP
      ? volcCustomEpId.trim()
      : selectedModel;

  const handleTest = async () => {
    setTesting(true);
    setTestResult(null);
    try {
      const result = await invoke<{ success: boolean; message: string }>("test_model_connection", {
        provider: selectedProvider,
        modelName: resolvedModelName,
        apiKey,
        proxyUrl: proxyUrl || null,
        proxyUsername: proxyUsername || null,
        proxyPassword: proxyPassword || null,
      });
      setTestResult(result);
    } catch (e) {
      setTestResult({ success: false, message: String(e) });
    }
    setTesting(false);
  };

  const handleSave = async () => {
    try {
      const name = resolvedModelName.trim();
      if (setDefault && !name) {
        toast.error(
          "无法写入全局默认模型：当前未选中具体模型。请先点击下方列表中的某一模型，再保存（填写 API Key 后列表会刷新，需重新点选模型）。",
          { duration: 7000 },
        );
        return false;
      }
      // 一次写入：把供应商配置（api_key + proxy）和 default_model 合并到同一个
      // set_default_model 调用中。后端会原子地 upsert 两个块并 sync_all。
      // 当 setDefault=false 时，apiKey/proxy 仍需保存 → 退化为只写供应商块。
      if (setDefault) {
        await invoke("set_default_model", {
          provider: selectedProvider,
          modelName: name,
          apiKey: apiKey || null,
          proxyUrl: proxyUrl || null,
          proxyUsername: proxyUsername || null,
          proxyPassword: proxyPassword || null,
        });
      } else {
        await invoke("save_provider_config", {
          providerId: selectedProvider,
          apiKey,
          proxyUrl: proxyUrl || null,
          proxyUsername: proxyUsername || null,
          proxyPassword: proxyPassword || null,
        });
      }
      toast.success(
        setDefault
          ? "已保存：供应商配置 + 全局默认模型已原子写入配置，请启动或重启网关后生效。"
          : "供应商配置已保存",
        { duration: 4000 },
      );
      await loadProviders();
      return true;
    } catch (e) {
      const msg = String(e);
      console.error("Save error:", e);
      toast.error(msg, { duration: 6000 });
      return false;
    }
  };

  const currentProvider = providers.find((p) => p.id === selectedProvider);
  const isOllama = selectedProvider === "ollama";
  const isKuaifan = selectedProvider === "kuaifan";
  const isVolcArk = selectedProvider === "volc_ark";

  const configuredMap = useMemo<Record<string, boolean>>(() => {
    const m: Record<string, boolean> = {};
    providers.forEach((p: Provider) => { m[p.id] = p.api_key_configured; });
    return m;
  }, [providers]);

  const freeCount = models.filter((m: ModelEntry) => m.is_free).length;
  const doc = PROVIDER_DOCS[selectedProvider];
  const showProxySettings =
    selectedProvider === "openai" ||
    selectedProvider === "google" ||
    selectedProvider === "grok";

  const currentIsDefault =
    (currentDefault && currentDefault.provider === selectedProvider) &&
    (currentDefault.model === selectedModel ||
      (isVolcArk && selectedModel === VOLC_CUSTOM_EP && currentDefault.model === volcCustomEpId.trim()));
  const heroAccent = currentProvider ? providerAccent(currentProvider.id) : providerAccent("kuaifan");

  return (
    <div className="flex flex-col h-full" style={{ background: C.bg }}>
      <header
        className="sticky top-0 z-20 backdrop-blur-md"
        style={{ background: "rgba(250, 248, 245, 0.78)", borderBottom: "1px solid " + C.borderSoft }}
      >
        <div className="max-w-[1320px] mx-auto px-6 h-14 flex items-center gap-4">
          <button
            onClick={onPrev}
            className="w-8 h-8 rounded-lg flex items-center justify-center transition-colors duration-150"
            style={{ color: C.textMute }}
            onMouseEnter={(e) => { e.currentTarget.style.background = C.bgHover; e.currentTarget.style.color = C.text; }}
            onMouseLeave={(e) => { e.currentTarget.style.background = "transparent"; e.currentTarget.style.color = C.textMute; }}
            title="返回上一步"
          >
            <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
              <path d="M10 3L5 8l5 5" />
            </svg>
          </button>

          <div className="flex items-center gap-2.5 flex-1 min-w-0">
            <div
              className="w-7 h-7 rounded-lg flex items-center justify-center shrink-0"
              style={{ background: C.accentSoft, boxShadow: "inset 0 0 0 1px " + C.accent + "26" }}
            >
              <CxIconCpu className="w-3.5 h-3.5" style={{ color: C.accent }} strokeWidth={2} />
            </div>
            <div className="min-w-0">
              <h1 className="text-[14px] font-semibold leading-tight truncate" style={{ color: C.text }}>
                大模型配置
              </h1>
              <p className="text-[11px] mt-0.5 leading-tight truncate" style={{ color: C.textMute }}>
                配置 AI 模型供应商与 API Key · 选中后即可作为全局默认
              </p>
            </div>
          </div>

          <div className="hidden md:flex items-center gap-2">
            <StatusBadge tone="neutral">
              <CxIconLayers className="w-3 h-3" />
              {providers.length} 家供应商
            </StatusBadge>
            <StatusBadge tone="success">
              <span className="w-1.5 h-1.5 rounded-full inline-block" style={{ background: C.success }} />
              {Object.values(configuredMap).filter(Boolean).length} 家已配置
            </StatusBadge>
          </div>
        </div>
      </header>

      <div className="flex-1 min-h-0 overflow-y-auto cx-scroll-slim">
        <div className="max-w-[1320px] mx-auto px-6 py-6">
          <div className="flex gap-5 items-start">
            <ProviderRail
              providers={providers}
              selectedId={selectedProvider}
              configuredMap={configuredMap}
              onSelect={setSelectedProvider}
              loading={loading}
            />

            <div className="flex-1 min-w-0 space-y-5">
              {/* HERO */}
              <section
                className="rounded-xl px-5 py-4 flex items-start gap-4 cx-animate-fade-in"
                style={{ background: C.bgElev, border: "1px solid " + C.borderSoft, boxShadow: "var(--cx-shadow-xs)" }}
              >
                <div
                  className={"w-12 h-12 rounded-xl flex items-center justify-center shrink-0 text-[18px] font-semibold tracking-tight " + (isOllama ? "" : "")}
                  style={{ background: isOllama ? C.bgSoft : heroAccent.bg, color: isOllama ? C.textSoft : heroAccent.fg, boxShadow: "inset 0 0 0 1px " + (isOllama ? C.border : "transparent") }}
                >
                  {isOllama ? <CxIconServer className="w-5 h-5" strokeWidth={2} /> : (currentProvider ? currentProvider.name : "?").charAt(0)}
                </div>

                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2 mb-1 flex-wrap">
                    <h2 className="text-[15px] font-semibold leading-tight tracking-tight" style={{ color: C.text }}>
                      {currentProvider ? currentProvider.name : selectedProvider}
                    </h2>
                    {hasStoredKey ? (
                      <StatusBadge tone="success">
                        <CxIconCheckCircle className="w-3 h-3" />
                        API Key 已保存
                      </StatusBadge>
                    ) : (
                      <StatusBadge tone="warn">
                        <CxIconAlertCircle className="w-3 h-3" />
                        待配置 API Key
                      </StatusBadge>
                    )}
                    {currentIsDefault && (
                      <StatusBadge tone="info">
                        <CxIconSparkles className="w-3 h-3" />
                        当前全局默认
                      </StatusBadge>
                    )}
                  </div>
                  <p className="text-[12px]" style={{ color: C.textMute }}>
                    {isKuaifan && "快泛API · 实时定价页拉取 · 部分模型免费"}
                    {isOllama && "本地模型服务 · 需先启动 Ollama 并拉取模型"}
                    {isVolcArk && "火山方舟 · 接入点 ID（ep-xxxx）需自行在控制台创建"}
                    {!isKuaifan && !isOllama && !isVolcArk && (doc ? doc.label : "")}
                    {doc && (
                      <>
                        {" · "}
                        <a
                          href={doc.href}
                          onClick={(e) => {
                            e.preventDefault();
                            invoke("open_url", { url: doc.href }).catch(() => undefined);
                          }}
                          className="underline-offset-2 hover:underline cursor-pointer font-medium"
                          style={{ color: C.accent }}
                        >
                          打开控制台
                        </a>
                      </>
                    )}
                  </p>
                </div>

                {doc && (
                  <button
                    type="button"
                    onClick={() => invoke("open_url", { url: doc.href }).catch(() => undefined)}
                    className="cx-btn cx-btn-secondary text-[12px]"
                    title={"打开 " + doc.label}
                  >
                    <CxIconExternalLink className="w-3.5 h-3.5" />
                    控制台
                  </button>
                )}
              </section>

              <SectionCard
                icon={CxIconLayers}
                title={isKuaifan ? "可选模型（实时定价）" : "模型库"}
                desc={
                  isKuaifan
                    ? "从 kuaifanio.cn/pricing 实时拉取 · 免费档以角标标注"
                    : "点击下方卡片选中模型 · 列表 ID 即调用时使用的 model 字段"
                }
                right={
                  !modelsLoading && !modelsError && models.length > 0 && (
                    <div className="flex items-center gap-1.5">
                      {isKuaifan && freeCount > 0 && (
                        <StatusBadge tone="success">{freeCount} 个免费</StatusBadge>
                      )}
                      <StatusBadge tone="neutral">共 {models.length} 个</StatusBadge>
                    </div>
                  )
                }
              >
                {modelsLoading && (
                  <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                  {Array.from({ length: 4 }).map((_, i) => (
                    <div key={i} className="cx-shimmer h-[72px] rounded-lg" />
                  ))}
                  </div>
                )}

                {modelsError && !modelsLoading && (
                  <div
                    className="flex items-start gap-2.5 text-[12.5px] px-3.5 py-3 rounded-lg"
                    style={{ background: C.errorSoft, color: C.error, border: "1px solid " + C.error + "33" }}
                  >
                    <CxIconXCircle className="w-4 h-4 mt-0.5 shrink-0" />
                    <div className="flex-1">
                      <div className="font-medium">模型列表加载失败</div>
                      <div className="mt-0.5 opacity-80 text-[11px]">{modelsError}</div>
                    </div>
                  </div>
                )}

                {!modelsLoading && !modelsError && models.length === 0 && (
                  <div className="text-center py-8" style={{ color: C.textMute }}>
                    <CxIconLayers className="w-7 h-7 mx-auto mb-2" style={{ color: C.textDim }} />
                    <p className="text-[12.5px]">{isOllama ? "未检测到本地模型 · 请先执行 ollama pull" : "暂无可用模型"}</p>
                    <p className="text-[11px] mt-1" style={{ color: C.textDim }}>
                      {isKuaifan ? "请检查网络后点击刷新重试" : "请检查 API Key 是否有效"}
                    </p>
                  </div>
                )}

                {!modelsLoading && !modelsError && models.length > 0 && (
                  <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                  {models.map((m) => (
                    <ModelCard
                      key={m.id}
                      m={m}
                      active={selectedModel === m.id}
                      onClick={async () => {
                        setSelectedModel(m.id);
                        if (m.id !== VOLC_CUSTOM_EP) setVolcCustomEpId("");
                        // 选中即设为默认：与当前表单的 apiKey / proxy 一起原子写入。
                        // 已勾选「设为全局默认」时同 handleSave；未勾选时也走一次写盘，
                        // 避免用户后续忘记点保存按钮时配置丢失。
                        try {
                          await invoke("set_default_model", {
                            provider: selectedProvider,
                            modelName: m.id,
                            apiKey: apiKey || null,
                            proxyUrl: proxyUrl || null,
                            proxyUsername: proxyUsername || null,
                            proxyPassword: proxyPassword || null,
                          });
                          setSetDefault(true);
                          toast.success(
                            `已选中并设为默认：${selectedProvider} / ${m.id}`,
                            { duration: 2500 },
                          );
                        } catch (e) {
                          console.error("set_default_model (on click) failed:", e);
                        }
                      }}
                    />
                  ))}
                  </div>
                )}

                {isVolcArk && selectedModel === VOLC_CUSTOM_EP && (
                  <div className="mt-4 pt-4" style={{ borderTop: "1px dashed " + C.borderSoft }}>
                    <label className="block text-[12px] font-medium mb-1.5" style={{ color: C.textSoft }}>
                      推理接入点 ID
                    </label>
                    <input
                      type="text"
                      value={volcCustomEpId}
                      onChange={(e) => setVolcCustomEpId(e.target.value)}
                      placeholder="例如 ep-20250101-xxxxx"
                      className="w-full px-3 py-2 rounded-lg text-[12.5px] font-mono outline-none"
                      style={{ background: C.bgSoft, border: "1px solid " + C.warn, color: C.text }}
                      onFocus={(e) => { e.currentTarget.style.boxShadow = "0 0 0 3px " + C.warnSoft; }}
                      onBlur={(e) => { e.currentTarget.style.boxShadow = "none"; }}
                    />
                    <p className="text-[11px] mt-1.5" style={{ color: C.textMute }}>
                      粘贴控制台为推理接入点分配的 <strong>ep-xxxx</strong> 完整字符串。
                    </p>
                  </div>
                )}

                {selectedProvider === "minimax" && (
                  <div
                    className="mt-4 px-3.5 py-2.5 rounded-lg text-[11.5px] leading-relaxed"
                    style={{ background: C.bgSoft, color: C.textSoft, border: "1px solid " + C.borderSoft }}
                  >
                    文本对话为 <strong>M2.1 / M2.5 / M2.7</strong> 等系列；「海螺」多为视频等多模态产品，请在{" "}
                    <a
                      href="https://platform.minimaxi.com/"
                      onClick={(e) => { e.preventDefault(); invoke("open_url", { url: "https://platform.minimaxi.com/" }).catch(() => undefined); }}
                      className="underline"
                      style={{ color: C.accent }}
                    >
                      MiniMax 开放平台
                    </a>{" "}
                    核对当前可用的 <code className="px-1 rounded" style={{ background: C.bgElev }}>model</code> 字段。
                    <br />
                    若 <strong>M2.5（标准）</strong> 对话失败、换 <strong>M2.5 高速</strong> 正常，多为账号侧产品线/线路差异。
                  </div>
                )}

                {isVolcArk && (
                  <div
                    className="mt-4 px-3.5 py-2.5 rounded-lg text-[11.5px] leading-relaxed space-y-1.5"
                    style={{ background: C.warnSoft, color: C.textSoft, border: "1px solid " + C.warn + "40" }}
                  >
                    <p>
                      <strong style={{ color: C.warn }}>鉴权</strong>：请使用火山方舟控制台「API Key 管理」里创建的{" "}
                      <strong>Ark API Key</strong>（长串密钥），<strong>不是</strong>火山引擎账号的 AK/SK。
                    </p>
                    <p>
                      <strong style={{ color: C.warn }}>模型名</strong>：对话接口 <code className="px-1 rounded" style={{ background: "rgba(255,255,255,0.5)" }}>/api/v3/chat/completions</code>，
                      <code className="px-1 rounded" style={{ background: "rgba(255,255,255,0.5)" }}>model</code> 一般填控制台分配的 <strong>接入点 ID（ep-xxxx）</strong>。
                    </p>
                  </div>
                )}
              </SectionCard>

              <SectionCard
                icon={CxIconKey}
                title={isOllama ? "本地服务" : "API Key"}
                desc={isOllama ? "Ollama 直接连接本地模型 · 无需 API Key" : "保存在本地配置文件 · 仅用于调用对应供应商接口"}
                right={
                  !isOllama && hasStoredKey ? (
                    <StatusBadge tone="success">
                      <CxIconCheckCircle className="w-3 h-3" />
                      已保存
                    </StatusBadge>
                  ) : null
                }
              >
                {isOllama ? (
                  <div
                    className="px-3.5 py-3 rounded-lg flex items-center gap-2.5 text-[12.5px]"
                    style={{ background: C.bgSoft, color: C.textSoft, border: "1px dashed " + C.border }}
                  >
                    <CxIconServer className="w-4 h-4" style={{ color: C.accent }} />
                    Ollama 直接连接 localhost:11434 · 无需配置 API Key
                  </div>
                ) : (
                  <div className="relative">
                    <input
                      type={showKey ? "text" : "password"}
                      value={apiKey}
                      onChange={(e) => setApiKey(e.target.value)}
                      placeholder={hasStoredKey ? "已保存 · 输入新值可替换" : "粘贴或输入供应商提供的 API Key"}
                      className="w-full px-3 py-2.5 pr-12 rounded-lg text-[12.5px] font-mono outline-none transition-shadow"
                      style={{ background: C.bgSoft, border: "1px solid " + C.border, color: C.text }}
                      onFocus={(e) => { e.currentTarget.style.borderColor = C.accent; e.currentTarget.style.boxShadow = "0 0 0 3px " + C.accentRing; }}
                      onBlur={(e) => { e.currentTarget.style.borderColor = C.border; e.currentTarget.style.boxShadow = "none"; }}
                    />
                    <button
                      type="button"
                      onClick={() => setShowKey((s) => !s)}
                      className="absolute right-2 top-1/2 -translate-y-1/2 w-7 h-7 rounded-md flex items-center justify-center transition-colors"
                      style={{ color: C.textMute }}
                      onMouseEnter={(e) => { e.currentTarget.style.background = C.bgHover; e.currentTarget.style.color = C.text; }}
                      onMouseLeave={(e) => { e.currentTarget.style.background = "transparent"; e.currentTarget.style.color = C.textMute; }}
                      title={showKey ? "隐藏" : "显示"}
                    >
                      {showKey ? <CxIconEyeOff className="w-3.5 h-3.5" /> : <CxIconEye className="w-3.5 h-3.5" />}
                    </button>
                  </div>
                )}

                {showProxySettings && (
                  <div className="mt-4 pt-4" style={{ borderTop: "1px dashed " + C.borderSoft }}>
                    <button
                      type="button"
                      onClick={() => setProxyOpen((o) => !o)}
                      className="flex items-center gap-1.5 text-[12px] font-medium cursor-pointer"
                      style={{ color: C.textSoft }}
                    >
                      <svg
                        width="10"
                        height="10"
                        viewBox="0 0 10 10"
                        fill="none"
                        stroke="currentColor"
                        strokeWidth="1.6"
                        strokeLinecap="round"
                        strokeLinejoin="round"
                        style={{ transform: proxyOpen ? "rotate(90deg)" : "rotate(0deg)", transition: "transform 0.18s var(--cx-ease-out)" }}
                      >
                        <path d="M3 1.5l3.5 3.5L3 8.5" />
                      </svg>
                      <CxIconShield className="w-3.5 h-3.5" />
                      代理服务设置（可选）
                    </button>
                    {proxyOpen && (
                      <div className="mt-3 space-y-2.5">
                        <input
                          type="text"
                          value={proxyUrl}
                          onChange={(e) => setProxyUrl(e.target.value)}
                          placeholder="代理地址，例如 http://127.0.0.1:7890"
                          className="w-full px-3 py-2 rounded-lg text-[12px] outline-none"
                          style={{ background: C.bgSoft, border: "1px solid " + C.border, color: C.text }}
                        />
                        <div className="grid grid-cols-2 gap-2.5">
                          <input
                            type="text"
                            value={proxyUsername}
                            onChange={(e) => setProxyUsername(e.target.value)}
                            placeholder="代理用户名"
                            className="w-full px-3 py-2 rounded-lg text-[12px] outline-none"
                            style={{ background: C.bgSoft, border: "1px solid " + C.border, color: C.text }}
                          />
                          <input
                            type="password"
                            value={proxyPassword}
                            onChange={(e) => setProxyPassword(e.target.value)}
                            placeholder="代理密码"
                            className="w-full px-3 py-2 rounded-lg text-[12px] outline-none"
                            style={{ background: C.bgSoft, border: "1px solid " + C.border, color: C.text }}
                          />
                        </div>
                        <p className="text-[11px]" style={{ color: C.textMute }}>
                          若使用代理服务，请填写代理地址及账号密码以提高连接稳定性。
                        </p>
                      </div>
                    )}
                  </div>
                )}
              </SectionCard>

              {testResult && (
                <div
                  className="flex items-start gap-2.5 text-[12.5px] px-3.5 py-2.5 rounded-lg cx-animate-fade-in"
                  style={{
                    background: testResult.success ? C.successSoft : C.errorSoft,
                    color: testResult.success ? C.success : C.error,
                    border: "1px solid " + (testResult.success ? C.success : C.error) + "33",
                  }}
                >
                  {testResult.success ? (
                    <CxIconCheckCircle className="w-4 h-4 mt-0.5 shrink-0" />
                  ) : (
                    <CxIconXCircle className="w-4 h-4 mt-0.5 shrink-0" />
                  )}
                  <div className="flex-1">
                    <div className="font-medium">{testResult.success ? "连接成功" : "连接失败"}</div>
                    <div className="mt-0.5 opacity-90 text-[11.5px]">{testResult.message}</div>
                  </div>
                </div>
              )}

            </div>
          </div>
        </div>
      </div>

      <div
        className="sticky bottom-0 z-10 backdrop-blur-md"
        style={{ background: "rgba(250, 248, 245, 0.86)", borderTop: "1px solid " + C.borderSoft }}
      >
        <div className="max-w-[1320px] mx-auto px-6 h-14 flex items-center gap-3">
          <label
            className="flex items-center gap-2 select-none"
            style={{
              opacity:
                !selectedModel ||
                (isVolcArk && selectedModel === VOLC_CUSTOM_EP && !volcCustomEpId.trim())
                  ? 0.4
                  : 1,
              cursor:
                !selectedModel ||
                (isVolcArk && selectedModel === VOLC_CUSTOM_EP && !volcCustomEpId.trim())
                  ? "not-allowed"
                  : "pointer",
            }}
          >
            <span className="relative inline-flex items-center" style={{ width: 32, height: 18 }}>
              <input
                type="checkbox"
                checked={setDefault}
                onChange={(e) => setSetDefault(e.target.checked)}
                disabled={
                  !selectedModel ||
                  (isVolcArk && selectedModel === VOLC_CUSTOM_EP && !volcCustomEpId.trim())
                }
                className="peer sr-only"
              />
              <span
                className="absolute inset-0 rounded-full transition-colors duration-200"
                style={{ background: setDefault ? C.accent : C.border }}
                onClick={() => {
                  if (
                    !selectedModel ||
                    (isVolcArk && selectedModel === VOLC_CUSTOM_EP && !volcCustomEpId.trim())
                  )
                    return;
                  setSetDefault((s) => !s);
                }}
              />
              <span
                className="absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform duration-200"
                style={{
                  transform: setDefault ? "translateX(16px)" : "translateX(2px)",
                  boxShadow: "0 1px 2px rgba(0,0,0,0.2)",
                }}
              />
            </span>
            <span className="text-[12.5px]" style={{ color: C.textSoft }}>设为全局默认模型</span>
          </label>

          <div className="flex-1" />

          {!isOllama && (
            <button
              type="button"
              onClick={handleTest}
              disabled={
                !apiKey ||
                !selectedModel ||
                testing ||
                (isVolcArk && selectedModel === VOLC_CUSTOM_EP && !volcCustomEpId.trim())
              }
              className="cx-btn cx-btn-secondary"
            >
              {testing ? <CxIconLoader className="w-3.5 h-3.5 animate-spin" /> : <CxIconWifi className="w-3.5 h-3.5" />}
              {testing ? "测试中…" : "测试连接"}
            </button>
          )}

          {isOllama && (
            <button
              type="button"
              onClick={loadModels}
              disabled={modelsLoading}
              className="cx-btn cx-btn-secondary"
            >
              {modelsLoading ? <CxIconLoader className="w-3.5 h-3.5 animate-spin" /> : <CxIconSparkles className="w-3.5 h-3.5" />}
              刷新模型列表
            </button>
          )}

          <button
            type="button"
            onClick={async () => {
              const ok = await handleSave();
              if (ok) onNext();
            }}
            disabled={selectedProvider !== "ollama" && !apiKey && !hasStoredKey}
            className="cx-btn cx-btn-primary"
          >
            保存配置
          </button>
        </div>
      </div>
    </div>
  );
}