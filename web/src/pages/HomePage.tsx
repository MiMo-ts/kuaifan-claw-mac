import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import toast from 'react-hot-toast';
import {
  CxIconDownload,
  CxIconLoader,
} from "../components/icons";
import { useAppStore } from "../stores/appStore";
import { checkForUpdate, downloadAndInstallUpdate, UpdateProgress } from "../utils/updater";
import ModuleCardsModal from "../components/ModuleCardsModal";
import ModelConfigModal from "../components/ModelConfigModal";
import CodexChatArea from "../components/layout/CodexChatArea";

interface GatewayStatus {
  running: boolean;
  version?: string;
  port: number;
  uptime_seconds: number;
  memory_mb: number;
  instances_running?: number;
}

export default function HomePage() {
  const { gatewayRunning, setGatewayRunning } = useAppStore();
  const [hydrated, setHydrated] = useState(false);
  const [gatewayBusy, setGatewayBusy] = useState(false);
  const [gatewayStatus, setGatewayStatus] = useState<GatewayStatus | null>(null);

  // Update
  const [updateAvailable, setUpdateAvailable] = useState(false);
  const [updateVersion, setUpdateVersion] = useState("");
  const [, setUpdateNotes] = useState("");
  const [updateProgress, setUpdateProgress] = useState<UpdateProgress | null>(null);
  const [isUpdating, setIsUpdating] = useState(false);

  // Modals
  const [modelConfigOpen, setModelConfigOpen] = useState(false);
  const [moduleCardsOpen, setModuleCardsOpen] = useState(false);

  const gatewayRunningRef = useRef(gatewayRunning);
  useEffect(() => { gatewayRunningRef.current = gatewayRunning; }, [gatewayRunning]);

  // Hydration
  useEffect(() => {
    if (useAppStore.persist.hasHydrated()) { setHydrated(true); return; }
    const unsub = useAppStore.persist.onFinishHydration(() => setHydrated(true));
    return unsub;
  }, []);

  // Load initial data
  useEffect(() => { if (!hydrated) return; loadInitial(); }, [hydrated]);

  const loadInitial = async () => {
    try {
      const [status] = await Promise.all([
        invoke<GatewayStatus>("get_gateway_status"),
        invoke<{ provider?: string; model_name?: string }>("get_default_model").catch(() => null),
      ]);
      setGatewayStatus(status);
      setGatewayRunning(status.running);
      const KEY = 'openclaw-module-center-shown';
      if (!status.running && !localStorage.getItem(KEY)) {
        localStorage.setItem(KEY, '1');
        setTimeout(() => setModuleCardsOpen(true), 500);
      }
    } catch {
      const KEY = 'openclaw-module-center-shown';
      if (!localStorage.getItem(KEY)) {
        localStorage.setItem(KEY, '1');
        setTimeout(() => setModuleCardsOpen(true), 500);
      }
    }
  };

  // Poll gateway status (always run, even when busy)
  useEffect(() => {
    if (!hydrated) return;
    const poll = async () => {
      try {
        const status = await invoke<GatewayStatus>("get_gateway_status");
        setGatewayStatus(status);
        setGatewayRunning(status.running);
      } catch { /* ignore */ }
    };
    // Faster polling when busy (every 1s), normal when idle (every 5s)
    const interval = gatewayBusy ? 1000 : 5000;
    const id = window.setInterval(poll, interval);
    const onVis = () => { if (document.visibilityState === "visible") void poll(); };
    document.addEventListener("visibilitychange", onVis);
    // Immediate poll on busy state change
    if (gatewayBusy) void poll();
    return () => { window.clearInterval(id); document.removeEventListener("visibilitychange", onVis); };
  }, [hydrated, gatewayBusy, setGatewayRunning]);

  // Module cards event listener
  useEffect(() => {
    const handler = () => setModuleCardsOpen(true);
    window.addEventListener("openModuleCards", handler);
    return () => window.removeEventListener("openModuleCards", handler);
  }, []);

  // Check for updates
  useEffect(() => {
    if (!hydrated) return;
    const doCheck = async () => {
      try {
        const info = await checkForUpdate();
        if (info.available) {
          setUpdateAvailable(true);
          setUpdateVersion(info.version || "");
          setUpdateNotes(info.body || "");
        }
      } catch { /* ignore */ }
    };
    const t = window.setTimeout(doCheck, 3000);
    return () => window.clearTimeout(t);
  }, [hydrated]);

  const handleToggleGateway = useCallback(async () => {
    if (gatewayBusy) return;
    const isRunning = gatewayRunningRef.current;
    setGatewayBusy(true);
    const toastId = toast.loading(isRunning ? "正在停止网关..." : "正在启动网关...", {
      style: { background: "var(--cx-bg-overlay)", color: "var(--cx-text)", border: "1px solid var(--cx-border)" },
    });
    try {
      if (isRunning) {
        await invoke("stop_gateway");
        setGatewayRunning(false);
        setGatewayStatus(null);
        toast.success("网关已停止", { id: toastId });
      } else {
        await invoke("start_gateway");
        toast.success("网关已启动", { id: toastId });
        // Poll multiple times to ensure status is accurate
        for (let i = 0; i < 3; i++) {
          await new Promise(r => setTimeout(r, 500));
          try {
            const status = await invoke<GatewayStatus>("get_gateway_status");
            setGatewayStatus(status);
            setGatewayRunning(status.running);
            if (status.running) break;
          } catch { /* ignore */ }
        }
      }
    } catch (e) {
      toast.error(`操作失败: ${e instanceof Error ? e.message : String(e)}`, { id: toastId });
    } finally {
      setGatewayBusy(false);
    }
  }, [gatewayBusy, setGatewayRunning]);

  const handleUpdate = async () => {
    if (isUpdating) return;
    setIsUpdating(true);
    try { await downloadAndInstallUpdate((p) => setUpdateProgress(p)); }
    catch (e) { toast.error(`更新失败: ${e}`); setIsUpdating(false); setUpdateProgress(null); }
  };

  if (!hydrated) {
    return (
      <div className="flex items-center justify-center h-full" style={{ background: "var(--cx-bg)" }}>
        <CxIconLoader className="cx-animate-spin w-5 h-5" style={{ color: "var(--cx-accent)" }} />
      </div>
    );
  }

  const isOnline = gatewayStatus?.running;

  return (
    <div className="flex flex-col h-full" style={{ background: "var(--cx-bg)" }}>
      {/* Top bar */}
      <div className="h-11 px-5 flex items-center justify-between shrink-0 gap-3 backdrop-blur-md"
        style={{ borderBottom: "1px solid var(--cx-border-soft)", background: "var(--cx-topbar-bg)" }}>
        <div className="flex items-center gap-3">
          <span className="text-[13px] font-medium" style={{ color: "var(--cx-text)" }}>快泛 Claw</span>
          <span className="cx-badge"
            style={isOnline ? { background: "var(--cx-success-soft)", color: "var(--cx-success)" } : { background: "var(--cx-error-soft)", color: "var(--cx-error)" }}>
            <span className="inline-block w-2 h-2 rounded-full" style={{ background: isOnline ? "var(--cx-success)" : "var(--cx-error)" }} />
            {isOnline ? `运行中:${gatewayStatus?.port}` : "未启动"}
          </span>
          {updateAvailable && !isUpdating && (
            <button onClick={handleUpdate} className="cx-btn cx-btn-primary" style={{ padding: "2px 10px", fontSize: 11 }}>
              <CxIconDownload className="w-3 h-3" />更新 v{updateVersion}
            </button>
          )}
        </div>
      </div>

      {/* Update progress */}
      {isUpdating && updateProgress && (
        <div className="px-4 py-2" style={{ background: "var(--cx-bg-soft)", borderBottom: "1px solid var(--cx-border-soft)" }}>
          <div className="text-[12px]" style={{ color: "var(--cx-text-soft)" }}>更新中:{updateProgress.percentage?.toFixed(0)}%</div>
          <div className="mt-1 h-1 rounded-full" style={{ background: "var(--cx-border-soft)" }}>
            <div className="h-full rounded-full transition-all" style={{ width: `${updateProgress.percentage ?? 0}%`, background: "var(--cx-accent)" }} />
          </div>
        </div>
      )}

      {/* Main content */}
      <div className="flex-1 min-h-0">
        <CodexChatArea
          title="新对话"
          gatewayRunning={isOnline ?? false}
          gatewayBusy={gatewayBusy}
          gatewayPort={gatewayStatus?.port ?? 0}
          onToggleGateway={handleToggleGateway}
        />
      </div>

      {/* Modals */}
      {moduleCardsOpen && <ModuleCardsModal onClose={() => setModuleCardsOpen(false)} />}
      {modelConfigOpen && <ModelConfigModal onClose={() => { setModelConfigOpen(false); loadInitial(); }} />}
    </div>
  );
}
