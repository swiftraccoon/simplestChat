import type { ClientMessage, ServerMessage } from './protocol';
import { decodeServerMessage } from './protocol-validation';

export type MessageHandler = (msg: ServerMessage) => void;

const RECONNECT_DEADLINE_MS = 120_000;

export class SignalingClient {
  private ws: WebSocket | null = null;
  private url: string;
  private onMessage: MessageHandler | null = null;
  private onStatusChange: ((status: 'connected' | 'disconnected' | 'connecting') => void) | null =
    null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private shouldReconnect = true;
  private reconnectAttempt = 0;
  private reconnectDeadline: ReturnType<typeof setTimeout> | null = null;
  private restarting = false;
  private recoveryFailed = false;
  private onReconnectFailed: (() => void) | null = null;
  private onConnectionLost: (() => void) | null = null;
  private onReconnected: (() => void) | null = null;
  private wasConnected = false;
  private currentToken: string | undefined;

  // Pending request/response tracking
  private pendingResolvers: Array<{
    match: (msg: ServerMessage) => boolean;
    resolve: (msg: ServerMessage) => void;
    reject: (err: Error) => void;
  }> = [];

  constructor(url: string) {
    this.url = url;
  }

  setOnMessage(handler: MessageHandler): void {
    this.onMessage = handler;
  }

  setOnStatusChange(handler: (status: 'connected' | 'disconnected' | 'connecting') => void): void {
    this.onStatusChange = handler;
  }

  /** Called when WS reconnects after a disconnect (for session recovery) */
  setOnReconnected(handler: () => void): void {
    this.onReconnected = handler;
  }

  setOnReconnectFailed(handler: () => void): void {
    this.onReconnectFailed = handler;
  }

  setOnConnectionLost(handler: () => void): void {
    this.onConnectionLost = handler;
  }

  /** Explicit user retry keeps the current identity and starts a fresh budget. */
  retryConnection(): void {
    this.restarting = true;
    this.shouldReconnect = true;
    this.startReconnectDeadline();
    this.connect();
  }

  /** Room recovery, explicit leave, or a replacement membership ends restart intent. */
  completeRestartRecovery(): void {
    this.restarting = false;
    this.clearReconnectDeadline();
    // Leaving the room must not leave an unbounded background connection attempt.
    if (this.shouldReconnect && this.wasConnected && !this.connected) this.startReconnectDeadline();
  }

  connect(token?: string): void {
    if (this.ws?.readyState === WebSocket.OPEN || this.ws?.readyState === WebSocket.CONNECTING)
      return;

    this.shouldReconnect = true;
    this.currentToken = token ?? this.currentToken;
    this.recoveryFailed = false;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.onStatusChange?.('connecting');

    // Keep bearer credentials out of the request URL, where proxies and APM
    // products commonly record them. The server selects only `simplestchat`;
    // the auth-prefixed protocol is transport for the handshake credential.
    const protocols = this.currentToken
      ? ['simplestchat', `auth.${this.currentToken}`]
      : ['simplestchat'];
    const socket = new WebSocket(this.url, protocols);
    this.ws = socket;

    socket.onopen = () => {
      if (this.ws !== socket) return;
      console.log('[ws] connected');
      const wasReconnect = this.wasConnected;
      this.wasConnected = true;
      this.reconnectAttempt = 0;
      if (!this.restarting) this.clearReconnectDeadline();
      this.onStatusChange?.('connected');
      if (wasReconnect) {
        this.onReconnected?.();
      }
    };

    socket.onmessage = (event) => {
      if (this.ws !== socket) return;
      let msg: ServerMessage;
      try {
        const data: unknown = event.data;
        if (typeof data !== 'string') throw new Error('Expected a text message');
        const parsed: unknown = JSON.parse(data);
        msg = decodeServerMessage(parsed);
      } catch {
        // Do not resolve a request or expose untrusted payloads in diagnostics.
        console.error('[ws] ignored invalid server message');
        return;
      }

      try {
        if (msg.type === 'serverRestarting') {
          this.restarting = true;
          this.startReconnectDeadline();
          this.rejectAllPending('Server restarting');
        }
        // Check pending resolvers first
        const idx = this.pendingResolvers.findIndex((p) => p.match(msg));
        if (idx !== -1) {
          const pending = this.pendingResolvers.splice(idx, 1)[0]!;
          if (msg.type === 'error') {
            pending.reject(new Error(msg.message));
          } else {
            pending.resolve(msg);
          }
          return;
        }

        this.onMessage?.(msg);
      } catch (e) {
        console.error('[ws] message handler failed:', e);
      }
    };

    socket.onclose = () => {
      // Authentication can replace a socket before its queued close arrives.
      // Only the current connection owns status, requests, and reconnect timers.
      if (this.ws !== socket) return;
      this.ws = null;
      console.log('[ws] disconnected');
      this.onStatusChange?.('disconnected');
      this.rejectAllPending('WebSocket closed');
      this.onConnectionLost?.();
      if (this.shouldReconnect) {
        this.startReconnectDeadline();
        this.scheduleReconnect();
      }
    };

    socket.onerror = (e) => {
      if (this.ws !== socket) return;
      console.error('[ws] error:', e);
    };
  }

  disconnect(): void {
    this.shouldReconnect = false;
    this.wasConnected = false;
    this.reconnectAttempt = 0;
    this.currentToken = undefined;
    this.restarting = false;
    this.recoveryFailed = false;
    this.clearReconnectDeadline();
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    const socket = this.ws;
    this.ws = null;
    socket?.close();
    this.rejectAllPending('Disconnected');
    if (socket) this.onStatusChange?.('disconnected');
  }

  send(msg: ClientMessage): void {
    if (this.ws?.readyState !== WebSocket.OPEN) {
      console.error('[ws] not connected, cannot send:', msg.type);
      return;
    }
    this.ws.send(JSON.stringify(msg));
  }

  /** Send a message and wait for a specific response type */
  request<T extends ServerMessage>(
    msg: ClientMessage,
    responseType: T['type'],
    timeoutMs = 5000,
  ): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      const pending = {
        match: (m: ServerMessage) => m.type === responseType || m.type === 'error',
        resolve: (m: ServerMessage) => {
          clearTimeout(timer);
          resolve(m as T);
        },
        reject: (err: Error) => {
          clearTimeout(timer);
          reject(err);
        },
      };
      const timer = setTimeout(() => {
        const idx = this.pendingResolvers.indexOf(pending);
        if (idx !== -1) this.pendingResolvers.splice(idx, 1);
        pending.reject(new Error(`Timeout waiting for ${responseType}`));
      }, timeoutMs);
      this.pendingResolvers.push(pending);

      this.send(msg);
    });
  }

  get connected(): boolean {
    return this.ws?.readyState === WebSocket.OPEN;
  }

  get reconnectExhausted(): boolean {
    return this.recoveryFailed;
  }

  /** Update JWT token for next connection/reconnection */
  setToken(token: string | undefined): void {
    this.currentToken = token;
  }

  private scheduleReconnect(): void {
    // Equal jitter spreads recovering clients without immediate reconnect loops.
    // The exponential ceiling is 2s, 4s, 8s, 16s, then 30s; every delay uses
    // half to all of that ceiling and one overall deadline bounds the outage.
    const ceiling = Math.min(2000 * Math.pow(2, this.reconnectAttempt), 30000);
    const delay = Math.floor(ceiling / 2 + (Math.random() * ceiling) / 2);
    this.reconnectAttempt++;
    console.log(`[ws] reconnecting in ${delay}ms (attempt ${this.reconnectAttempt})`);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      console.log('[ws] attempting reconnect...');
      this.connect(this.currentToken);
    }, delay);
  }

  private clearReconnectDeadline(): void {
    if (this.reconnectDeadline) clearTimeout(this.reconnectDeadline);
    this.reconnectDeadline = null;
  }

  private startReconnectDeadline(): void {
    if (this.reconnectDeadline !== null) return;
    this.reconnectDeadline = setTimeout(() => {
      this.reconnectDeadline = null;
      this.shouldReconnect = false;
      this.restarting = false;
      this.recoveryFailed = true;
      if (this.reconnectTimer) clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
      const socket = this.ws;
      this.ws = null;
      socket?.close();
      this.rejectAllPending('Reconnection timed out');
      this.onStatusChange?.('disconnected');
      this.onReconnectFailed?.();
    }, RECONNECT_DEADLINE_MS);
  }

  private rejectAllPending(reason: string): void {
    const pending = this.pendingResolvers.splice(0);
    for (const p of pending) {
      p.reject(new Error(reason));
    }
  }
}
