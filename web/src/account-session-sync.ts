const MARKER = 'simplestchat-account-change-v1';
const CHANNEL = 'simplestchat-account-change-v1';
const RECENT_MARKERS = 32;
const FOCUS_INTERVAL_MS = 2_000;

interface SessionSyncOptions {
  reconcile: () => Promise<void>;
  invalidate: () => void;
  canCheck: () => boolean;
}

function revision(value: unknown): value is string {
  return typeof value === 'string' && value.length === 32 && /^[a-f0-9]+$/.test(value);
}

/** Same-origin hints only: the server's HttpOnly cookie determines identity.
 * Broadcasts/storage contain one random revision, never account information.
 * Duplicate hints coalesce; a newer hint immediately fences an older request.
 * Browser APIs can be unavailable without preventing sign-in. */
export class AccountSessionSync {
  private channel: BroadcastChannel | null = null;
  private storage: Storage | null = null;
  private readonly seen = new Set<string>();
  private dirty = false;
  private running = false;
  private disposed = false;
  private lastFocusCheck = -Infinity;

  constructor(private readonly options: SessionSyncOptions) {
    try {
      this.storage = window.localStorage;
      const marker = this.storage.getItem(MARKER);
      if (revision(marker)) this.remember(marker);
    } catch {
      this.storage = null;
    }
    try {
      this.channel = new BroadcastChannel(CHANNEL);
      this.channel.onmessage = (event: MessageEvent<unknown>): void => {
        const value = event.data;
        if (typeof value !== 'object' || value === null || Array.isArray(value)) return;
        const fields = Object.keys(value);
        if (
          fields.length !== 2 ||
          !('version' in value) ||
          !('revision' in value) ||
          value.version !== 1
        )
          return;
        this.accept(value.revision);
      };
    } catch {
      this.channel = null;
    }
    window.addEventListener('storage', this.onStorage);
    window.addEventListener('focus', this.checkMissedChanges);
    window.addEventListener('pageshow', this.checkMissedChanges);
    document.addEventListener('visibilitychange', this.onVisibility);
  }

  /** Call after a session-changing response or an uncertain cookie mutation. */
  publish(): void {
    if (this.disposed) return;
    let marker: string;
    try {
      const bytes = crypto.getRandomValues(new Uint8Array(16));
      marker = Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
    } catch {
      return;
    }
    this.remember(marker);
    try {
      this.storage?.setItem(MARKER, marker);
    } catch {
      this.storage = null;
    }
    try {
      this.channel?.postMessage({ version: 1, revision: marker });
    } catch {
      // A frozen/closed channel cannot invalidate a successful account change.
    }
  }

  private remember(marker: string): void {
    this.seen.add(marker);
    if (this.seen.size > RECENT_MARKERS) this.seen.delete(this.seen.values().next().value!);
  }

  private accept(value: unknown): void {
    if (this.disposed || !revision(value) || this.seen.has(value)) return;
    this.remember(value);
    this.request();
  }

  private readonly onStorage = (event: StorageEvent): void => {
    if (event.storageArea === this.storage && event.key === MARKER) this.accept(event.newValue);
  };

  private readonly onVisibility = (): void => {
    if (document.visibilityState === 'visible') this.checkMissedChanges();
  };

  private readonly checkMissedChanges = (): void => {
    if (this.disposed || document.visibilityState !== 'visible') return;
    if (this.storage) {
      try {
        this.accept(this.storage.getItem(MARKER));
        return;
      } catch {
        this.storage = null;
      }
    }
    // Storage-disabled browsers cannot detect missed revisions on wake. Query
    // the server at most once per focus interval, never interrupt a local prompt.
    const now = performance.now();
    if (this.running || now - this.lastFocusCheck < FOCUS_INTERVAL_MS || !this.options.canCheck())
      return;
    this.lastFocusCheck = now;
    this.request();
  };

  private request(): void {
    if (this.disposed) return;
    this.dirty = true;
    try {
      this.options.invalidate();
    } catch {
      this.dirty = false;
      return;
    }
    if (!this.running) {
      this.running = true;
      queueMicrotask(() => {
        this.drain().catch(() => {
          this.running = false;
        });
      });
    }
  }

  private async drain(): Promise<void> {
    try {
      while (this.dirty && !this.disposed) {
        this.dirty = false;
        try {
          await this.options.reconcile();
        } catch {
          // AuthManager owns failure state. Coordination must not reject into UI.
        }
      }
    } finally {
      this.running = false;
    }
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.dirty = false;
    this.seen.clear();
    window.removeEventListener('storage', this.onStorage);
    window.removeEventListener('focus', this.checkMissedChanges);
    window.removeEventListener('pageshow', this.checkMissedChanges);
    document.removeEventListener('visibilitychange', this.onVisibility);
    if (this.channel) {
      this.channel.onmessage = null;
      try {
        this.channel.close();
      } catch {
        /* The channel may already have closed during browser teardown. */
      }
      this.channel = null;
    }
    try {
      this.options.invalidate();
    } catch {
      /* Teardown must not prevent other owners from releasing their resources. */
    }
  }
}
