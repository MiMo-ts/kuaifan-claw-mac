/**
 * OpenClaw Gateway WebSocket Client
 *
 * Connects to the OpenClaw gateway via WebSocket JSON-RPC protocol,
 * providing full agent capabilities (skills, tools, memory) instead of
 * raw model proxy.
 */

interface GatewayClientOpts {
  url?: string;
  token?: string;
  onEvent?: (event: string, payload: any) => void;
  onConnected?: () => void;
  onDisconnected?: (reason?: string) => void;
  onError?: (err: Error) => void;
}

interface PendingRequest {
  resolve: (value: any) => void;
  reject: (err: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

export class OpenClawGateway {
  private ws: WebSocket | null = null;
  private opts: GatewayClientOpts;
  private pending = new Map<string, PendingRequest>();
  private nextId = 1;
  private closed = false;
  private hello: any = null;
  private resolveReady: (() => void) | null = null;
  private readyPromise: Promise<void>;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private backoffMs = 1000;
  private connectNonce: string | null = null;

  constructor(opts: GatewayClientOpts = {}) {
    this.opts = opts;
    this.readyPromise = new Promise((resolve) => { this.resolveReady = resolve; });
  }

  /** Resolve the gateway URL and token from openclaw.json config */
  static async resolveConnection(port?: number): Promise<{ url: string; token: string }> {
    const { invoke } = await import('@tauri-apps/api/core');
    try {
      const dataDir: string = await invoke('get_data_dir');
      const { readTextFile } = await import('@tauri-apps/plugin-fs');
      const { join } = await import('@tauri-apps/api/path');
      const cfgPath = await join(dataDir, 'openclaw-cn', 'openclaw.json');
      const content = await readTextFile(cfgPath);
      const cfg = JSON.parse(content);
      const gwPort = port || cfg?.gateway?.port || 18789;
      const gwToken = cfg?.gateway?.auth?.token || '';
      return { url: `ws://127.0.0.1:${gwPort}`, token: gwToken };
    } catch {
      return { url: `ws://127.0.0.1:${port || 18789}`, token: '' };
    }
  }

  async start() {
    if (this.closed) return;
    // Reset ready promise for new connection
    this.readyPromise = new Promise((resolve) => { this.resolveReady = resolve; });
    try {
      const { url, token } = await OpenClawGateway.resolveConnection();
      const finalUrl = this.opts.url ?? url;
      const finalToken = this.opts.token ?? token;

      this.ws = new WebSocket(finalUrl);
      // Note: browser WebSocket doesn't support custom headers.
      // Token is sent via the connect RPC auth field instead.

      this.ws.onopen = () => {
        // Wait for hello event from gateway
      };

      this.ws.onmessage = (event) => {
        try {
          const msg = JSON.parse(event.data as string);
          this.handleMessage(msg);
        } catch {
          // ignore parse errors
        }
      };

      this.ws.onclose = (e) => {
        this.opts.onDisconnected?.(e.reason || undefined);
        if (!this.closed) this.scheduleReconnect();
      };

      this.ws.onerror = () => {
        this.opts.onError?.(new Error('WebSocket connection error'));
      };
    } catch (err) {
      this.opts.onError?.(err instanceof Error ? err : new Error(String(err)));
    }
  }

  private scheduleReconnect() {
    if (this.closed) return;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    this.backoffMs = Math.min(this.backoffMs * 1.5, 30000);
    this.reconnectTimer = setTimeout(() => {
      this.backoffMs = 1000;
      this.start();
    }, this.backoffMs);
  }

  private handleMessage(msg: any) {
    // Gateway sends connect.challenge event first (not 'hello')
    if (msg.type === 'event' && msg.event === 'connect.challenge') {
      this.connectNonce = msg.payload?.nonce ?? null;
      this.sendConnect();
      return;
    }

    if (msg.type === 'res') {
      const pending = this.pending.get(msg.id);
      if (!pending) return;
      this.pending.delete(msg.id);
      clearTimeout(pending.timer);
      if (msg.ok !== false) {
        pending.resolve(msg.payload ?? msg.result ?? null);
      } else {
        pending.reject(new Error(msg.error?.message ?? msg.error ?? 'Request failed'));
      }
      return;
    }

    if (msg.type === 'event') {
      this.opts.onEvent?.(msg.event, msg.payload);
    }
  }

  private async sendConnect() {
    const id = String(this.nextId++);
    const token = this.opts.token ?? '';
    const connectBody: any = {
      type: 'req',
      id,
      method: 'connect',
      params: {
        minProtocol: 3,
        maxProtocol: 3,
        client: {
          id: 'clawdbot-control-ui',
          version: '1.0.0',
          platform: navigator.platform?.startsWith('Win') ? 'win32' :
                    navigator.platform?.startsWith('Mac') ? 'darwin' : 'linux',
          mode: 'webchat',
          instanceId: 'manager-' + Math.random().toString(36).slice(2, 8),
        },
        role: 'operator',
        scopes: ['operator.admin', 'operator.approvals', 'operator.pairing', 'operator.write'],
        caps: [],
        auth: token ? { token } : undefined,
        userAgent: navigator.userAgent,
        locale: navigator.language,
      },
    };
    if (this.connectNonce) {
      connectBody.params.device = { nonce: this.connectNonce };
    }

    return new Promise<void>((resolve) => {
      const pending: PendingRequest = {
        resolve: (payload: any) => {
          this.backoffMs = 1000;
          this.resolveReady?.();
          this.opts.onConnected?.();
          resolve();
        },
        reject: (err: Error) => {
          this.opts.onError?.(err);
          resolve();
        },
        timer: setTimeout(() => {
          this.pending.delete(id);
          resolve();
        }, 10000),
      };
      this.pending.set(id, pending);
      this.ws?.send(JSON.stringify(connectBody));
    });
  }

  async ready(): Promise<void> {
    return this.readyPromise;
  }

  /** Send a chat message - non-blocking, responses come as 'chat' events */
  async sendChat(opts: {
    sessionKey?: string;
    message: string;
    thinking?: string | number;
    deliver?: boolean;
    timeoutMs?: number;
  }): Promise<{ runId: string }> {
    await this.readyPromise;
    const runId = `run-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    const result = await this.request('chat.send', {
      sessionKey: opts.sessionKey ?? 'main',
      message: opts.message,
      thinking: opts.thinking,
      deliver: opts.deliver,
      timeoutMs: opts.timeoutMs,
      idempotencyKey: runId,
    });
    return { runId: result?.runId ?? runId };
  }

  /** Abort a running chat */
  async abortChat(sessionKey: string, runId: string): Promise<any> {
    return this.request('chat.abort', { sessionKey, runId });
  }

  /** Load chat history */
  async loadHistory(sessionKey: string, limit = 100): Promise<any> {
    return this.request('chat.history', { sessionKey, limit });
  }

  /** Generic JSON-RPC request */
  async request(method: string, params?: any): Promise<any> {
    await this.readyPromise;
    const id = String(this.nextId++);
    const body: any = { type: 'req', id, method };
    if (params !== undefined) body.params = params;

    return new Promise((resolve, reject) => {
      const pending: PendingRequest = {
        resolve,
        reject,
        timer: setTimeout(() => {
          this.pending.delete(id);
          reject(new Error(`Request timed out: ${method}`));
        }, 30000),
      };
      this.pending.set(id, pending);
      try {
        this.ws?.send(JSON.stringify(body));
      } catch (e) {
        this.pending.delete(id);
        clearTimeout(pending.timer);
        reject(e);
      }
    });
  }

  /** Load gateway status */
  async getStatus(): Promise<any> {
    return this.request('status');
  }

  async stop() {
    this.closed = true;
    if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
    // Reject all pending
    for (const [, pending] of this.pending) {
      clearTimeout(pending.timer);
      pending.reject(new Error('Connection closed'));
    }
    this.pending.clear();
    this.ws?.close();
    this.ws = null;
  }
}
