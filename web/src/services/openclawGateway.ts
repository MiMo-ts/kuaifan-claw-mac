/**
 * OpenClaw Gateway WebSocket Client
 *
 * Connects to the OpenClaw gateway via WebSocket JSON-RPC protocol,
 * providing full agent capabilities (skills, tools, memory) instead of
 * raw model proxy.
 *
 * Uses Ed25519 device identity for gateway authentication.
 */

import * as ed from '@noble/ed25519';

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

// ── Device Identity (Ed25519) ──

const DEVICE_STORAGE_KEY = 'clawdbot-device-identity-v1';

function base64UrlEncode(buf: Uint8Array): string {
  let t = '';
  for (const b of buf) t += String.fromCharCode(b);
  return btoa(t).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/g, '');
}

function base64UrlDecode(input: string): Uint8Array {
  const normalized = input.replace(/-/g, '+').replace(/_/g, '/');
  const padded = normalized + '='.repeat((4 - (normalized.length % 4)) % 4);
  const s = atob(padded);
  const bytes = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) bytes[i] = s.charCodeAt(i);
  return bytes;
}

async function sha256Hex(data: Uint8Array): Promise<string> {
  const hash = await crypto.subtle.digest('SHA-256', data.buffer as ArrayBuffer);
  return Array.from(new Uint8Array(hash)).map(b => b.toString(16).padStart(2, '0')).join('');
}

const ED25519_SPKI_PREFIX = new Uint8Array([0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00]);

function publicKeyRawFromSpki(spkiDer: Uint8Array): Uint8Array {
  return spkiDer.slice(ED25519_SPKI_PREFIX.length);
}

async function loadOrCreateDeviceIdentity(): Promise<{
  deviceId: string; publicKey: Uint8Array; privateKeyBytes: Uint8Array;
}> {
  try {
    const stored = localStorage.getItem(DEVICE_STORAGE_KEY);
    if (stored) {
      const { privateKey: privB64, publicKey: pubB64, deviceId } = JSON.parse(stored);
      const privateKeyBytes = base64UrlDecode(privB64);
      const publicKey = base64UrlDecode(pubB64);
      const derivedId = await sha256Hex(publicKey);
      if (derivedId === deviceId) {
        return { deviceId, publicKey, privateKeyBytes };
      }
    }
  } catch { /* regenerate */ }

  const privateKeyBytes = ed.utils.randomSecretKey();
  const publicKey = await ed.getPublicKeyAsync(privateKeyBytes);
  const deviceId = await sha256Hex(publicKey);

  localStorage.setItem(DEVICE_STORAGE_KEY, JSON.stringify({
    version: 1,
    deviceId,
    publicKey: base64UrlEncode(publicKey),
    privateKey: base64UrlEncode(privateKeyBytes),
  }));

  return { deviceId, publicKey, privateKeyBytes };
}

function buildSignaturePayload(params: {
  deviceId: string; clientId: string; clientMode: string;
  role: string; scopes: string[]; signedAtMs: number; token: string; nonce: string;
}): string {
  return [
    'v2', params.deviceId, params.clientId, params.clientMode,
    params.role, params.scopes.join(','), String(params.signedAtMs),
    params.token, params.nonce,
  ].join('|');
}

async function signPayload(privateKeyBytes: Uint8Array, payload: string): Promise<string> {
  const msgBytes = new TextEncoder().encode(payload);
  const sig = await ed.signAsync(msgBytes, privateKeyBytes);
  return base64UrlEncode(sig);
}

// ── Gateway Client ──

export class OpenClawGateway {
  private ws: WebSocket | null = null;
  private opts: GatewayClientOpts;
  private pending = new Map<string, PendingRequest>();
  private nextId = 1;
  private closed = false;
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
    this.readyPromise = new Promise((resolve) => { this.resolveReady = resolve; });
    try {
      const { url, token } = await OpenClawGateway.resolveConnection();
      const finalUrl = this.opts.url ?? url;
      const finalToken = this.opts.token ?? token;
      this.opts.token = finalToken;

      this.ws = new WebSocket(finalUrl);

      this.ws.onopen = () => {};

      this.ws.onmessage = (event) => {
        try {
          const msg = JSON.parse(event.data as string);
          this.handleMessage(msg);
        } catch { /* ignore */ }
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
    const clientId = 'clawdbot-control-ui';
    const clientMode = 'webchat';
    const role = 'operator';
    const scopes = ['operator.admin', 'operator.approvals', 'operator.pairing', 'operator.write'];

    let device: any = undefined;
    if (typeof crypto !== 'undefined' && crypto.subtle) {
      try {
        const identity = await loadOrCreateDeviceIdentity();
        const signedAtMs = Date.now();
        const payload = buildSignaturePayload({
          deviceId: identity.deviceId, clientId, clientMode,
          role, scopes, signedAtMs, token,
          nonce: this.connectNonce ?? '',
        });
        const signature = await signPayload(identity.privateKeyBytes, payload);
        device = {
          id: identity.deviceId,
          publicKey: base64UrlEncode(identity.publicKey),
          signature,
          signedAt: signedAtMs,
          nonce: this.connectNonce ?? undefined,
        };
      } catch (e) { console.warn('[gw] device identity failed:', e); }
    }

    const connectBody: any = {
      type: 'req',
      id,
      method: 'connect',
      params: {
        minProtocol: 3,
        maxProtocol: 3,
        client: {
          id: clientId,
          version: '1.0.0',
          platform: navigator.platform?.startsWith('Win') ? 'win32' :
                    navigator.platform?.startsWith('Mac') ? 'darwin' : 'linux',
          mode: clientMode,
          instanceId: 'manager-' + Math.random().toString(36).slice(2, 8),
        },
        role,
        scopes,
        device,
        caps: [],
        auth: token ? { token } : undefined,
        userAgent: navigator.userAgent,
        locale: navigator.language,
      },
    };

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
    attachments?: Array<{ type: string; image_url?: { url: string }; source?: { type: string; data: string; media_type: string } }>;
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
      attachments: opts.attachments,
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
    for (const [, pending] of this.pending) {
      clearTimeout(pending.timer);
      pending.reject(new Error('Connection closed'));
    }
    this.pending.clear();
    this.ws?.close();
    this.ws = null;
  }
}
