export type AccountUpdate = {
  account: string;
  scope: 'ready' | 'inbox' | 'contacts' | 'preferences' | 'notifications';
  peerId?: string;
};
const listeners = new Set<(update: AccountUpdate) => void>();
const connected = new Map<string, object>();

export function onAccountUpdate(listener: (update: AccountUpdate) => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function accountUpdatesLive(account: string | null): boolean {
  return account !== null && connected.has(account);
}

type WireUpdate =
  | { type: 'ready' }
  | { type: 'change'; scope: Exclude<AccountUpdate['scope'], 'ready'>; peerId?: string };
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** Small, bounded SSE decoder. Comments are heartbeats; partial events never apply. */
export class AccountEventDecoder {
  private readonly deliver: (update: WireUpdate) => void;

  private line = '';
  private data: string[] = [];
  private size = 0;
  private carriageReturn = false;

  constructor(deliver: (update: WireUpdate) => void) {
    this.deliver = deliver;
  }

  push(chunk: string): void {
    for (const char of chunk) {
      if (char === '\n' && this.carriageReturn) {
        this.carriageReturn = false;
        continue;
      }
      this.carriageReturn = char === '\r';
      if (char !== '\r' && char !== '\n') {
        this.line += char;
        if (this.line.length + this.size > 8192) throw new Error('Account event too large');
        continue;
      }
      const line = this.line;
      this.line = '';
      if (line === '') {
        if (this.data.length) this.event(this.data.join('\n'));
        this.data = [];
        this.size = 0;
      } else if (line.startsWith('data:')) {
        const data = line.slice(5).replace(/^ /, '');
        this.size += data.length + 1;
        if (this.size > 8192) throw new Error('Account event too large');
        this.data.push(data);
      }
    }
  }

  private event(data: string): void {
    const value: unknown = JSON.parse(data);
    if (value === null || typeof value !== 'object' || Array.isArray(value))
      throw new Error('Invalid account event');
    const record = value as Record<string, unknown>;
    if (record['type'] === 'ready') {
      this.deliver({ type: 'ready' });
      return;
    }
    if (
      record['type'] !== 'change' ||
      !['inbox', 'contacts', 'preferences', 'notifications'].includes(String(record['scope'])) ||
      (record['peerId'] !== undefined &&
        (typeof record['peerId'] !== 'string' || !uuid.test(record['peerId'])))
    )
      throw new Error('Invalid account event');
    this.deliver({
      type: 'change',
      scope: record['scope'] as Exclude<AccountUpdate['scope'], 'ready'>,
      ...(typeof record['peerId'] === 'string' ? { peerId: record['peerId'] } : {}),
    });
  }
}

type Options = { getToken: () => string | null; getAccountId: () => string | null };

/** Invalidation only: HTTP remains authoritative, including after every reconnect.
 * Credentials stay in the request header. A new identity retires every old callback. */
export class AccountUpdates {
  private readonly options: Options;

  private account: string | null = null;
  private token: string | null = null;
  private controller: AbortController | null = null;
  private retry: ReturnType<typeof setTimeout> | null = null;
  private readonly events = new AbortController();
  private readonly timer: ReturnType<typeof setInterval>;
  private attempt = 0;
  private denied = false;
  private disposed = false;

  constructor(options: Options) {
    this.options = options;

    const sync = () => this.synchronize();
    window.addEventListener('focus', sync, { signal: this.events.signal });
    window.addEventListener(
      'online',
      () => {
        // A ready stream can retain a stalled read after connectivity returns.
        // Pending headers and server-directed retries keep their existing owner.
        if (this.account && connected.get(this.account) === this) this.stop();
        this.synchronize();
      },
      { signal: this.events.signal },
    );
    document.addEventListener('visibilitychange', sync, { signal: this.events.signal });
    this.timer = setInterval(sync, 5000);
    this.synchronize();
  }

  synchronize(): void {
    if (this.disposed) return;
    const account = this.options.getAccountId();
    const token = this.options.getToken();
    if (account !== this.account || token !== this.token) {
      this.stop();
      this.account = account;
      this.token = token;
      this.denied = false;
      this.attempt = 0;
    }
    if (document.hidden) this.stop();
    else if (account && token && !this.controller && !this.retry && !this.denied)
      this.connect(account, token).catch(() => {});
  }

  private stop(): void {
    this.controller?.abort();
    this.controller = null;
    if (this.account && connected.get(this.account) === this) connected.delete(this.account);
    if (this.retry !== null) clearTimeout(this.retry);
    this.retry = null;
  }

  private async connect(account: string, token: string): Promise<void> {
    const controller = new AbortController();
    this.controller = controller;
    const current = () =>
      !this.disposed &&
      !document.hidden &&
      !controller.signal.aborted &&
      this.controller === controller &&
      this.options.getAccountId() === account &&
      this.options.getToken() === token;
    let delay = 0;
    let watchdog = setTimeout(() => controller.abort(), 10_000);
    let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    try {
      const response = await fetch('/api/auth/updates', {
        headers: { Authorization: `Bearer ${token}`, Accept: 'text/event-stream' },
        cache: 'no-store',
        redirect: 'error',
        credentials: 'omit',
        signal: controller.signal,
      });
      if (!current()) {
        await response.body?.cancel();
        return;
      }
      if (response.status === 401 || response.status === 403) {
        this.denied = true;
        await response.body?.cancel();
        return;
      }
      if (response.status === 429) {
        const header = response.headers.get('Retry-After');
        const seconds = header && /^\d+$/.test(header) ? Number(header) : NaN;
        const until = header ? Date.parse(header) - Date.now() : NaN;
        delay = Math.min(
          300_000,
          Math.max(
            1000,
            Number.isFinite(seconds) ? seconds * 1000 : Number.isFinite(until) ? until : 60_000,
          ),
        );
      }
      if (
        !response.ok ||
        !response.headers.get('Content-Type')?.startsWith('text/event-stream') ||
        !response.body
      ) {
        await response.body?.cancel();
        throw new Error('Account stream unavailable');
      }
      let ready = false;
      const readyDeadline = Date.now() + 10_000;
      const decoder = new AccountEventDecoder((update) => {
        if (!current()) return;
        if (update.type === 'ready') {
          if (ready) throw new Error('Repeated account stream ready');
          ready = true;
          this.attempt = 0;
          connected.set(account, this);
        } else if (!ready) throw new Error('Account stream not ready');
        const event: AccountUpdate =
          update.type === 'ready'
            ? { account, scope: 'ready' }
            : { account, scope: update.scope, ...(update.peerId ? { peerId: update.peerId } : {}) };
        for (const listener of listeners) {
          try {
            listener(event);
          } catch {
            /* An individual view cannot interrupt resynchronization. */
          }
        }
      });
      const utf8 = new TextDecoder('utf-8', { fatal: true });
      reader = response.body.getReader();
      while (current()) {
        clearTimeout(watchdog);
        // Heartbeats cannot keep a stream without its initial ready alive forever.
        watchdog = setTimeout(
          () => controller.abort(),
          ready ? 45_000 : Math.max(0, readyDeadline - Date.now()),
        );
        const chunk = await reader.read();
        if (!current() || chunk.done) break;
        decoder.push(utf8.decode(chunk.value, { stream: true }));
      }
    } catch {
      /* A bounded reconnect always performs a fresh authoritative read. */
    } finally {
      clearTimeout(watchdog);
      reader?.cancel().catch(() => {});
      if (this.controller === controller) {
        this.controller = null;
        if (connected.get(account) === this) connected.delete(account);
        if (
          !this.disposed &&
          !document.hidden &&
          !this.denied &&
          account === this.options.getAccountId() &&
          token === this.options.getToken()
        ) {
          const backoff = Math.min(60_000, 1000 * 2 ** Math.min(this.attempt++, 6));
          this.retry = setTimeout(
            () => {
              this.retry = null;
              this.synchronize();
            },
            delay || Math.min(60_000, Math.round(backoff * (0.8 + Math.random() * 0.4))),
          );
        }
      }
    }
  }

  dispose(): void {
    this.disposed = true;
    this.stop();
    this.events.abort();
    clearInterval(this.timer);
  }
}
