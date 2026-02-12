import type { ClientMessage, ServerMessage } from './protocol';

export type MessageHandler = (msg: ServerMessage) => void;

export class SignalingClient {
  private ws: WebSocket | null = null;
  private url: string;
  private onMessage: MessageHandler | null = null;
  private onStatusChange: ((status: 'connected' | 'disconnected' | 'connecting') => void) | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private shouldReconnect = true;
  private reconnectAttempt = 0;
  private onReconnected: (() => void) | null = null;
  private wasConnected = false;

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

  connect(): void {
    if (this.ws?.readyState === WebSocket.OPEN) return;

    this.shouldReconnect = true;
    this.onStatusChange?.('connecting');

    this.ws = new WebSocket(this.url);

    this.ws.onopen = () => {
      console.log('[ws] connected');
      const wasReconnect = this.wasConnected;
      this.wasConnected = true;
      this.reconnectAttempt = 0;
      this.onStatusChange?.('connected');
      if (wasReconnect) {
        this.onReconnected?.();
      }
    };

    this.ws.onmessage = (event) => {
      try {
        const msg: ServerMessage = JSON.parse(event.data as string);

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
        console.error('[ws] failed to parse message:', e);
      }
    };

    this.ws.onclose = () => {
      console.log('[ws] disconnected');
      this.onStatusChange?.('disconnected');
      this.rejectAllPending('WebSocket closed');
      if (this.shouldReconnect) {
        this.scheduleReconnect();
      }
    };

    this.ws.onerror = (e) => {
      console.error('[ws] error:', e);
    };
  }

  disconnect(): void {
    this.shouldReconnect = false;
    this.wasConnected = false;
    this.reconnectAttempt = 0;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.ws?.close();
    this.ws = null;
    this.rejectAllPending('Disconnected');
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
      const timer = setTimeout(() => {
        const idx = this.pendingResolvers.findIndex((p) => p.resolve === (resolve as any)); // eslint-disable-line @typescript-eslint/no-explicit-any
        if (idx !== -1) this.pendingResolvers.splice(idx, 1);
        reject(new Error(`Timeout waiting for ${responseType}`));
      }, timeoutMs);

      this.pendingResolvers.push({
        match: (m: ServerMessage) => m.type === responseType || m.type === 'error',
        resolve: (m: ServerMessage) => {
          clearTimeout(timer);
          resolve(m as T);
        },
        reject: (err: Error) => {
          clearTimeout(timer);
          reject(err);
        },
      });

      this.send(msg);
    });
  }

  get connected(): boolean {
    return this.ws?.readyState === WebSocket.OPEN;
  }

  private scheduleReconnect(): void {
    // Exponential backoff: 2s, 4s, 8s, 16s, max 30s
    const delay = Math.min(2000 * Math.pow(2, this.reconnectAttempt), 30000);
    this.reconnectAttempt++;
    console.log(`[ws] reconnecting in ${delay}ms (attempt ${this.reconnectAttempt})`);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      console.log('[ws] attempting reconnect...');
      this.connect();
    }, delay);
  }

  private rejectAllPending(reason: string): void {
    const pending = this.pendingResolvers.splice(0);
    for (const p of pending) {
      p.reject(new Error(reason));
    }
  }
}
