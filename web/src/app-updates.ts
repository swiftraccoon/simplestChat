type Options = {
  revision: string;
  inCall: () => boolean;
  available: (available: boolean) => void;
  reload: () => void;
};
const revisionPattern = /^[a-f0-9]{40}$/;

/** A public build hint, independent of accounts. Updating always needs a click
 * outside a call; discovering a release cannot reload or retire media. */
export class AppUpdates {
  private readonly events = new AbortController();
  private request: AbortController | null = null;
  private checkedAt = -Infinity;
  private disposed = false;
  private nextRevision: string | null = null;

  constructor(private readonly options: Options) {
    const check = () => {
      this.check().catch(() => {});
    };
    window.addEventListener('focus', check, { signal: this.events.signal });
    document.addEventListener('visibilitychange', check, { signal: this.events.signal });
    check();
  }

  async check(now = Date.now()): Promise<void> {
    if (
      this.disposed ||
      this.request ||
      document.hidden ||
      !revisionPattern.test(this.options.revision) ||
      now - this.checkedAt < 60_000
    )
      return;
    this.checkedAt = now;
    const controller = new AbortController();
    this.request = controller;
    const timeout = setTimeout(() => controller.abort(), 5000);
    let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    let responseBody: ReadableStream<Uint8Array> | null = null;
    try {
      const response = await fetch('/app-version.json', {
        headers: { Accept: 'application/json' },
        cache: 'no-store',
        credentials: 'omit',
        redirect: 'error',
        signal: controller.signal,
      });
      responseBody = response.body;
      if (
        !response.ok ||
        !response.headers.get('Content-Type')?.startsWith('application/json') ||
        !response.body
      )
        return;
      const length = response.headers.get('Content-Length');
      if (length && (!/^\d+$/.test(length) || Number(length) > 512)) return;
      reader = response.body.getReader();
      const decoder = new TextDecoder('utf-8', { fatal: true });
      let bytes = 0;
      let body = '';
      while (!controller.signal.aborted) {
        const chunk = await reader.read();
        if (chunk.done) break;
        bytes += chunk.value.byteLength;
        if (bytes > 512) return;
        body += decoder.decode(chunk.value, { stream: true });
      }
      body += decoder.decode();
      if (this.disposed || this.request !== controller || controller.signal.aborted) return;
      const value: unknown = JSON.parse(body);
      if (
        !value ||
        typeof value !== 'object' ||
        Array.isArray(value) ||
        Object.keys(value).length !== 1 ||
        !('revision' in value) ||
        typeof value.revision !== 'string' ||
        !revisionPattern.test(value.revision)
      )
        return;
      this.nextRevision = value.revision === this.options.revision ? null : value.revision;
      this.options.available(this.nextRevision !== null);
    } catch {
      // A release hint cannot interrupt the current app or become an auth error.
    } finally {
      clearTimeout(timeout);
      controller.abort();
      // Rejecting headers or size must also retire an unread response body.
      // Native cancellation can itself remain pending; it cannot block teardown.
      Promise.resolve()
        .then(() => (reader ? reader.cancel() : responseBody?.cancel()))
        .catch(() => {});
      if (this.request === controller) this.request = null;
    }
  }

  reloadWhenIdle(): boolean {
    if (this.disposed || !this.nextRevision || this.options.inCall()) return false;
    this.options.reload();
    return true;
  }

  dispose(): void {
    this.disposed = true;
    this.events.abort();
    this.request?.abort();
    this.request = null;
  }
}
