import type { ChatEntry } from './protocol';
import { AttachmentComposer } from './attachments';
import { SavedPmDrafts } from './chat-store';
import {
  PM_SEND_LOCK,
  PM_SEND_STORAGE_KEY,
  PmSendAttempts,
  type PmSendAttempt,
} from './pm-send-attempts';
import { asyncButton, button, el, field, modal } from './ui';

type Options = {
  account: string;
  peer: string;
  current: () => boolean;
  getToken: () => string | null;
  signal: AbortSignal;
  send: (content: string, id: string, signal: AbortSignal, files: string[]) => Promise<ChatEntry>;
  confirmed: () => Promise<void>;
  fail: (error: unknown) => void;
};
const listeners = new Set<() => void>();
const changed = (): void => {
  for (const listener of listeners) listener();
};

/** A submitted payload belongs to its durable receipt, separately from the next draft. */
export function mountPmComposer(host: HTMLElement, options: Options): () => void {
  // Accessing localStorage itself can throw in restricted browser contexts.
  const storage = {
    getItem: (key: string) => localStorage.getItem(key),
    setItem: (key: string, value: string) => localStorage.setItem(key, value),
  };
  const drafts = new SavedPmDrafts(storage);
  const attempts = new PmSendAttempts(storage);
  const active = (): boolean => !options.signal.aborted && options.current();
  const composer = el('textarea');
  composer.rows = 3;
  composer.maxLength = 2000;
  composer.placeholder = 'Write a private message…';
  composer.value = drafts.get(options.account, options.peer);
  const saveDraft = (): void => {
    if (options.current()) drafts.save(options.account, options.peer, composer.value);
  };
  composer.addEventListener('input', saveDraft);
  const recovery = el('div', undefined, 'pm-unconfirmed');
  const attachmentHost = el('div', undefined, 'history-attachment-composer');
  const attachments = new AttachmentComposer({
    host: attachmentHost,
    input: composer,
    current: active,
    getToken: () => (options.current() ? options.getToken() : null),
    notify: (message) => options.fail(new Error(message)),
  });
  let sending = false;
  let confirmation: ReturnType<typeof modal> | null = null;

  const locked = async (work: (signal: AbortSignal) => Promise<void>): Promise<void> => {
    if (!('locks' in navigator) || !navigator.locks)
      throw new Error('This browser cannot safely save sends for recovery. Use another browser.');
    const signal = AbortSignal.any([options.signal, AbortSignal.timeout(15_000)]);
    await navigator.locks.request(PM_SEND_LOCK, { signal }, async () => {
      signal.throwIfAborted();
      if (active()) await work(signal);
    });
  };

  const deliver = async (attempt: PmSendAttempt, signal: AbortSignal): Promise<void> => {
    const receipt = await options.send(
      attempt.content,
      attempt.id,
      signal,
      attempt.attachments.map((file) => file.id),
    );
    if (
      receipt.clientMessageId !== attempt.id ||
      receipt.participantId !== options.account ||
      receipt.recipientId !== options.peer
    )
      throw new Error('The original send was not confirmed. Check saved messages before retrying.');
    if (!options.current()) return;
    if (!attempts.clear(options.account, options.peer, attempt.id))
      throw new Error('The message was confirmed, but its recovery item could not be cleared.');
    changed();
  };

  const submit = async (retryId?: string): Promise<void> => {
    if (sending || !active()) return;
    const content = composer.value.trim();
    if (!retryId && !content && !attachments.hasFiles) return;
    const files = retryId ? [] : attachments.ready();
    sending = true;
    composer.disabled = true;
    refresh();
    let confirmed = false;
    try {
      await locked(async (signal) => {
        let state = attempts.read(options.account, options.peer);
        if (!retryId) {
          if (state.kind !== 'empty')
            throw new Error('Resolve the unconfirmed send before sending another message.');
          state = attempts.begin(options.account, options.peer, {
            id: crypto.randomUUID(),
            content,
            attachments: files,
          });
          if (state.kind === 'unavailable' || state.kind === 'full')
            throw new Error(
              'This send could not be saved for recovery. Restore browser storage before sending.',
            );
        }
        if (state.kind !== 'pending' || (retryId && state.attempt.id !== retryId))
          throw new Error('The original send cannot be recovered. Review saved messages first.');
        if (!retryId) {
          // Detach submitted uploads before close can delete pending composer files.
          // These synchronous writes finish before the network request can begin.
          attachments.consume();
          composer.value = '';
          saveDraft();
          changed();
        }
        await deliver(state.attempt, signal);
        confirmed = true;
      });
      if (confirmed && active()) await options.confirmed();
    } finally {
      sending = false;
      composer.disabled = false;
      refresh();
    }
  };
  const send = asyncButton('Send', () => submit(), options.fail);
  send.className = 'btn-secondary history-send';

  function refresh(): void {
    if (!active()) return;
    const state = attempts.read(options.account, options.peer);
    recovery.replaceChildren();
    recovery.hidden = state.kind === 'empty';
    send.disabled = sending || state.kind !== 'empty';
    if (state.kind === 'empty') return;
    if (!('attempt' in state)) {
      recovery.append(
        el('p', 'Saved sends cannot be read. Restore browser storage before sending.'),
      );
      return;
    }
    const attempt = state.attempt;
    const status = el('p', 'Unconfirmed send');
    status.setAttribute('role', 'status');
    recovery.append(status, el('p', attempt.content));
    for (const file of attempt.attachments) recovery.append(el('p', file.name));
    if (state.kind === 'expired')
      recovery.append(
        el('p', 'The 24-hour retry window ended. Review saved messages before dismissing.'),
      );
    const retry = asyncButton('Retry unconfirmed send', () => submit(attempt.id), options.fail);
    retry.disabled = sending || state.kind !== 'pending';
    const dismiss = button('Dismiss unconfirmed send', () => {
      if (sending || !active()) return;
      confirmation?.close();
      const view = modal('Dismiss unconfirmed send?');
      confirmation = view;
      view.dialog.setAttribute('data-account-dialog', 'true');
      view.body.append(
        el(
          'p',
          'This message may already have been delivered. Review saved messages first. Dismissing does not recall it.',
        ),
        asyncButton(
          'Dismiss',
          async () => {
            await locked(async () => {
              if (!attempts.clear(options.account, options.peer, attempt.id))
                throw new Error('The recovery item changed or could not be cleared.');
              changed();
            });
            view.close();
          },
          options.fail,
        ),
      );
    });
    dismiss.disabled = sending;
    recovery.append(retry, dismiss);
  }
  composer.addEventListener('keydown', (event) => {
    if (
      event.key === 'Enter' &&
      !event.isComposing &&
      (event.ctrlKey ||
        event.metaKey ||
        (!event.shiftKey && !window.matchMedia('(any-pointer: coarse)').matches))
    ) {
      event.preventDefault();
      send.click();
    }
  });
  const storageChanged = (event: StorageEvent): void => {
    if (event.key === PM_SEND_STORAGE_KEY || event.key === null) refresh();
  };
  listeners.add(refresh);
  window.addEventListener('storage', storageChanged);
  attachmentHost.append(send);
  host.append(recovery, field('Private message', composer), attachmentHost);
  refresh();
  return (): void => {
    saveDraft();
    confirmation?.close();
    listeners.delete(refresh);
    window.removeEventListener('storage', storageChanged);
    attachments.dispose();
  };
}
