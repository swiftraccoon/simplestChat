import type { ChatAttachment } from './protocol';
import { decodeAttachment } from './protocol-validation';
import { button, el } from './ui';
import './attachments.css';

export const MAX_ATTACHMENT_BYTES = 5 * 1024 * 1024;
const MAX_FILES = 4;
const imageTypes = new Set(['image/png', 'image/jpeg', 'image/webp']);
// Across all open histories, retain at most eight downloaded/previewed blobs.
const blobOwners = new Map<string, () => void>();
function ownBlob(url: string, release: () => void): void {
  blobOwners.set(url, release);
  while (blobOwners.size > 8) {
    const oldest = blobOwners.entries().next().value;
    if (!oldest) break;
    blobOwners.delete(oldest[0]);
    oldest[1]();
  }
}
function releaseBlob(url: string): void {
  blobOwners.delete(url);
  URL.revokeObjectURL(url);
}

/** Keep filenames out of URLs and preserve Unicode in the binary upload header. */
export function encodedFilename(name: string): string {
  const bytes = new TextEncoder().encode(name);
  if (!bytes.length || bytes.length > 255) throw new Error('File names must be 1–255 bytes');
  return btoa(String.fromCharCode(...bytes))
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/, '');
}

function sizeLabel(size: number): string {
  return size >= 1024 * 1024
    ? `${(size / (1024 * 1024)).toFixed(1)} MB`
    : `${Math.max(1, Math.ceil(size / 1024))} KB`;
}

/** A bounded, cancellable binary request with real upload progress. */
export function uploadAttachment(
  file: File,
  token: string,
  signal: AbortSignal,
  progress: (percent: number) => void,
): Promise<ChatAttachment> {
  if (!file.size || file.size > MAX_ATTACHMENT_BYTES)
    return Promise.reject(new Error('Choose a nonempty file up to 5 MB'));
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    const abort = (): void => xhr.abort();
    const finish = (): void => signal.removeEventListener('abort', abort);
    xhr.open('POST', '/api/auth/attachments');
    xhr.timeout = 60_000;
    xhr.setRequestHeader('Authorization', `Bearer ${token}`);
    xhr.setRequestHeader('Content-Type', file.type || 'application/octet-stream');
    xhr.setRequestHeader('X-File-Name', encodedFilename(file.name));
    xhr.upload.onprogress = (event): void => {
      if (event.lengthComputable)
        progress(Math.min(99, Math.round((event.loaded / event.total) * 100)));
    };
    xhr.onload = (): void => {
      finish();
      try {
        if (xhr.status < 200 || xhr.status >= 300) {
          const message =
            xhr.status === 413
              ? 'Choose a file up to 5 MB'
              : xhr.status === 401
                ? 'Sign in again to attach files'
                : xhr.status === 429
                  ? 'Attachment storage or upload limit reached; try again later'
                  : 'Upload failed; retry or choose another file';
          throw new Error(message);
        }
        const value: unknown = JSON.parse(xhr.responseText);
        resolve(decodeAttachment(value));
      } catch (error) {
        reject(error instanceof Error ? error : new Error('Invalid upload response'));
      }
    };
    xhr.onerror = (): void => {
      finish();
      reject(new Error('Upload interrupted; retry when connected'));
    };
    xhr.ontimeout = (): void => {
      finish();
      reject(new Error('Upload timed out; retry when connected'));
    };
    xhr.onabort = (): void => {
      finish();
      reject(new DOMException('Upload cancelled', 'AbortError'));
    };
    if (signal.aborted) {
      reject(new DOMException('Upload cancelled', 'AbortError'));
      return;
    }
    signal.addEventListener('abort', abort, { once: true });
    xhr.send(file);
  });
}

type ComposerOptions = {
  host: HTMLElement;
  input: HTMLTextAreaElement;
  getToken: () => string | null;
  current: () => boolean;
  changed?: () => void;
  notify: (message: string) => void;
};
type PendingFile = {
  file: File;
  controller: AbortController;
  metadata: ChatAttachment | null;
  state: 'uploading' | 'ready' | 'failed';
  progress: number;
  error: string;
};

/** Pending files belong to this composer only; resets fence all late completions. */
export class AttachmentComposer {
  private readonly files: PendingFile[] = [];
  private readonly tray = el('div', undefined, 'attachment-tray');
  private readonly picker = el('input');
  private readonly attach: HTMLButtonElement;
  private disposed = false;

  constructor(private readonly options: ComposerOptions) {
    this.tray.setAttribute('aria-label', 'Attachments');
    this.picker.type = 'file';
    this.picker.multiple = true;
    this.picker.hidden = true;
    this.picker.setAttribute('aria-label', 'Choose attachments');
    this.attach = button(
      '＋',
      () => {
        if (!this.options.getToken()) {
          this.options.notify('Sign in to attach files');
          return;
        }
        if (!this.options.current() || this.options.input.disabled) return;
        this.picker.click();
      },
      'attachment-add',
    );
    this.attach.title = 'Attach files (up to 5 MB each)';
    this.attach.setAttribute('aria-label', 'Attach files');
    this.picker.addEventListener('change', this.pick);
    options.input.addEventListener('paste', this.paste);
    options.host.addEventListener('dragover', this.drag);
    options.host.addEventListener('drop', this.drop);
    options.host.append(this.attach, this.picker, this.tray);
    this.render();
  }

  get hasFiles(): boolean {
    return this.files.length > 0;
  }

  /** Sending is allowed only after every selected file is acknowledged. */
  ready(): ChatAttachment[] {
    if (this.files.some((file) => file.state !== 'ready'))
      throw new Error('Wait for uploads, or retry or remove failed attachments');
    return this.files.flatMap((file) => (file.metadata ? [file.metadata] : []));
  }

  consume(): void {
    this.files.splice(0);
    this.render();
  }

  reset(): void {
    for (const file of this.files.splice(0)) {
      file.controller.abort();
      if (file.metadata) this.removePending(file.metadata.id);
    }
    this.picker.value = '';
    this.render();
  }

  dispose(): void {
    this.reset();
    this.disposed = true;
    this.picker.removeEventListener('change', this.pick);
    this.options.input.removeEventListener('paste', this.paste);
    this.options.host.removeEventListener('dragover', this.drag);
    this.options.host.removeEventListener('drop', this.drop);
    this.tray.remove();
    this.picker.remove();
    this.attach.remove();
  }

  private readonly pick = (): void => {
    this.add(Array.from(this.picker.files ?? []));
    this.picker.value = '';
  };
  private readonly paste = (event: ClipboardEvent): void => {
    const files = Array.from(event.clipboardData?.files ?? []);
    if (!files.length) return;
    event.preventDefault();
    this.add(files);
  };
  private readonly drag = (event: DragEvent): void => {
    if (event.dataTransfer?.types.includes('Files')) event.preventDefault();
  };
  private readonly drop = (event: DragEvent): void => {
    const files = Array.from(event.dataTransfer?.files ?? []);
    if (!files.length) return;
    event.preventDefault();
    this.add(files);
  };

  private add(files: File[]): void {
    if (this.disposed || !this.options.current() || this.options.input.disabled) return;
    if (!this.options.getToken()) {
      this.options.notify('Sign in to attach files');
      return;
    }
    if (files.length + this.files.length > MAX_FILES) {
      this.options.notify('Attach up to four files per message');
      return;
    }
    try {
      for (const file of files) {
        encodedFilename(file.name);
        if (!file.size || file.size > MAX_ATTACHMENT_BYTES)
          throw new Error('Choose nonempty files up to 5 MB each');
      }
    } catch (error) {
      this.options.notify(error instanceof Error ? error.message : 'Invalid file');
      return;
    }
    for (const file of files) {
      const pending: PendingFile = {
        file,
        controller: new AbortController(),
        metadata: null,
        state: 'uploading',
        progress: 0,
        error: '',
      };
      this.files.push(pending);
      this.upload(pending);
    }
    this.render();
  }

  private upload(file: PendingFile): void {
    const token = this.options.getToken();
    if (!token || this.disposed || !this.options.current()) return;
    file.controller = new AbortController();
    file.state = 'uploading';
    file.progress = 0;
    file.error = '';
    const current = (): boolean =>
      !this.disposed && this.files.includes(file) && this.options.current();
    uploadAttachment(file.file, token, file.controller.signal, (percent) => {
      if (!current()) return;
      file.progress = percent;
      this.render();
    })
      .then((metadata) => {
        if (!current()) {
          this.removePending(metadata.id, token);
          return;
        }
        file.metadata = metadata;
        file.state = 'ready';
        file.progress = 100;
        this.render();
      })
      .catch((error: unknown) => {
        if (!current() || file.controller.signal.aborted) return;
        file.state = 'failed';
        file.error = error instanceof Error ? error.message : 'Upload failed';
        this.render();
      });
  }

  private removePending(id: string, token = this.options.getToken()): void {
    if (!token) return;
    fetch(`/api/auth/attachments/${encodeURIComponent(id)}`, {
      method: 'DELETE',
      headers: { Authorization: `Bearer ${token}` },
      signal: AbortSignal.timeout(15_000),
    }).catch(() => {
      /* Abandoned uploads also expire on the server. */
    });
  }

  private render(): void {
    this.tray.hidden = !this.files.length;
    this.tray.replaceChildren(
      ...this.files.map((file) => {
        const row = el('div', undefined, 'attachment-pending');
        row.append(
          el('span', `${file.file.name} · ${sizeLabel(file.file.size)}`, 'attachment-name'),
        );
        const status = el(
          'span',
          file.state === 'ready'
            ? 'Ready'
            : file.state === 'failed'
              ? file.error
              : `${file.progress}%`,
          'attachment-status',
        );
        status.setAttribute('role', 'status');
        row.append(status);
        if (file.state === 'failed')
          row.append(
            button('Retry', () => {
              if (this.options.input.disabled) return;
              this.upload(file);
              this.render();
            }),
          );
        const remove = button('×', () => {
          if (this.options.input.disabled) return;
          const index = this.files.indexOf(file);
          if (index < 0) return;
          this.files.splice(index, 1);
          file.controller.abort();
          if (file.metadata) this.removePending(file.metadata.id);
          this.render();
        });
        remove.setAttribute('aria-label', `Remove ${file.file.name}`);
        row.append(remove);
        return row;
      }),
    );
    this.options.changed?.();
  }
}

type ReadOptions = { current: () => boolean; authorization: (id: string) => Promise<string> };

/** Fetch only after a gesture. Credentials stay in headers; every row owns its blobs. */
export function renderAttachments(
  host: HTMLElement,
  attachments: ChatAttachment[],
  options: ReadOptions,
): () => void {
  const controller = new AbortController();
  const urls = new Set<string>();
  const timers = new Set<ReturnType<typeof setTimeout>>();
  const container = el('div', undefined, 'message-attachments');
  const current = (): boolean => !controller.signal.aborted && options.current();
  for (const attachment of attachments.slice(0, MAX_FILES)) {
    const row = el('div', undefined, 'message-attachment');
    const status = el('span', '', 'attachment-status');
    status.setAttribute('role', 'status');
    const preview = el('img');
    preview.hidden = true;
    preview.alt = attachment.name;
    let imageUrl: string | null = null;
    let busy = false;
    const open = async (inline: boolean): Promise<void> => {
      if (busy || !current()) return;
      if (inline && imageUrl) {
        preview.hidden = !preview.hidden;
        return;
      }
      busy = true;
      status.textContent = 'Loading…';
      try {
        const authorization = await options.authorization(attachment.id);
        if (!current()) return;
        const response = await fetch(`/api/auth/attachments/${encodeURIComponent(attachment.id)}`, {
          headers: { Authorization: authorization },
          signal: AbortSignal.any([controller.signal, AbortSignal.timeout(30_000)]),
        });
        if (!response.ok)
          throw new Error(
            response.status === 404 || response.status === 410
              ? 'Attachment expired or removed'
              : 'Attachment unavailable; try again',
          );
        const type = response.headers.get('Content-Type')?.split(';')[0]?.trim() ?? '';
        if (inline && (!imageTypes.has(type) || type !== attachment.contentType))
          throw new Error('Image preview unavailable');
        const reader = response.body?.getReader();
        if (!reader) throw new Error('Attachment unavailable');
        const chunks: Uint8Array<ArrayBuffer>[] = [];
        let size = 0;
        try {
          for (;;) {
            const part = await reader.read();
            if (part.done) break;
            size += part.value.byteLength;
            if (size > MAX_ATTACHMENT_BYTES || size > attachment.size)
              throw new Error('Invalid attachment size');
            chunks.push(new Uint8Array(part.value));
          }
        } finally {
          await reader.cancel();
        }
        if (size !== attachment.size) throw new Error('Incomplete attachment; try again');
        if (!current()) return;
        const url = URL.createObjectURL(
          new Blob(chunks, { type: inline ? type : 'application/octet-stream' }),
        );
        urls.add(url);
        ownBlob(url, () => {
          urls.delete(url);
          releaseBlob(url);
          if (imageUrl === url) {
            imageUrl = null;
            preview.removeAttribute('src');
            preview.hidden = true;
          }
        });
        if (inline) {
          imageUrl = url;
          preview.src = url;
          preview.hidden = false;
        } else {
          const link = el('a');
          link.href = url;
          link.download = attachment.name;
          row.append(link);
          link.click();
          link.remove();
          const timer = setTimeout(() => {
            releaseBlob(url);
            urls.delete(url);
            timers.delete(timer);
          }, 30_000);
          timers.add(timer);
        }
        status.textContent = '';
      } catch (error) {
        if (current())
          status.textContent = error instanceof Error ? error.message : 'Attachment unavailable';
      } finally {
        busy = false;
      }
    };
    row.append(el('span', `${attachment.name} · ${sizeLabel(attachment.size)}`, 'attachment-name'));
    if (imageTypes.has(attachment.contentType))
      row.append(
        button('Preview', () => {
          open(true).catch(() => {});
        }),
      );
    row.append(
      button('Download', () => {
        open(false).catch(() => {});
      }),
      status,
      preview,
    );
    container.append(row);
  }
  host.append(container);
  return (): void => {
    controller.abort();
    for (const timer of timers) clearTimeout(timer);
    for (const url of urls) releaseBlob(url);
    urls.clear();
    timers.clear();
    container.remove();
  };
}
