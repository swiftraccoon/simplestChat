import type { ChatPreferences, PreferencePatch } from './api-validation';
import { api, ApiOutcomeUnknownError, asyncButton, el } from './ui';
import { group, note, toggleRow } from './settings-sheet';

const CHANGE_KEY = 'simplestchat.preferences.changed';
type Options = {
  getToken: () => string | null;
  getAccountId: () => string | null;
  notify: (message: string) => void;
};
type Listener = (account: string | null, state: ChatPreferences | null) => void;

/** Account-owned state exists independently of room membership. Writes carry only
 * explicit field/set intents; cross-tab hints contain no preferences or credentials.
 * Other devices reconcile visible views every fifteen seconds and on focus. */
export class AccountPreferences {
  private account: string | null = null;
  private state: ChatPreferences | null = null;
  private epoch = 0;
  private operation = 0;
  private saving = false;
  private queued = 0;
  private tail: Promise<void> = Promise.resolve();
  private uncertain = false;
  private failure = '';
  private pending: Promise<void> | null = null;
  private controller = new AbortController();
  private readonly events = new AbortController();
  private readonly listeners = new Set<Listener>();
  private readonly timer: ReturnType<typeof setInterval>;
  private disposed = false;

  constructor(private readonly options: Options) {
    const refresh = (): void => {
      if (!document.hidden) this.refresh().catch(() => {});
    };
    window.addEventListener('focus', refresh, { signal: this.events.signal });
    document.addEventListener('visibilitychange', refresh, { signal: this.events.signal });
    window.addEventListener(
      'storage',
      (event) => {
        if (event.key === CHANGE_KEY || event.key === null) refresh();
      },
      { signal: this.events.signal },
    );
    this.timer = setInterval(refresh, 15_000);
  }

  private synchronize(): void {
    const account = this.options.getAccountId();
    if (account === this.account) return;
    this.controller.abort();
    this.controller = new AbortController();
    this.account = account;
    this.epoch++;
    this.operation++;
    this.state = null;
    this.pending = null;
    this.saving = false;
    this.queued = 0;
    this.tail = Promise.resolve();
    this.uncertain = false;
    this.failure = '';
    this.emit();
  }

  private current(account: string, epoch: number): boolean {
    return (
      !this.disposed &&
      this.account === account &&
      this.options.getAccountId() === account &&
      this.epoch === epoch
    );
  }

  get snapshot(): ChatPreferences | null {
    this.synchronize();
    return this.state ? structuredClone(this.state) : null;
  }

  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  private emit(): void {
    for (const listener of this.listeners)
      listener(this.account, this.state ? structuredClone(this.state) : null);
  }

  refresh(explicit = false): Promise<void> {
    this.synchronize();
    const account = this.account;
    const token = this.options.getToken();
    if (this.disposed || !account || !token || this.saving || (this.uncertain && !explicit))
      return Promise.resolve();
    if (this.pending) return this.pending;
    const epoch = this.epoch;
    const operation = this.operation;
    const task = api
      .accountPreferences(token, this.controller.signal)
      .then((state) => {
        if (!this.current(account, epoch) || operation !== this.operation) return;
        this.state = state;
        this.failure = '';
        this.uncertain = false;
        this.emit();
      })
      .catch((error: unknown) => {
        if (this.current(account, epoch) && operation === this.operation) {
          this.failure = error instanceof Error ? error.message : 'Privacy could not be loaded';
          this.emit();
        }
        throw error;
      })
      .finally(() => {
        if (this.pending === task) this.pending = null;
      });
    this.pending = task;
    return task;
  }

  change(patch: PreferencePatch): Promise<ChatPreferences> {
    this.synchronize();
    const account = this.account;
    const epoch = this.epoch;
    if (!account || this.disposed || this.queued >= 20)
      return Promise.reject(new Error('Please wait for the current preference changes'));
    this.queued++;
    const task = this.tail.then(async () => {
      if (!this.current(account, epoch)) throw new Error('The account changed');
      return this.apply(patch);
    });
    this.tail = task.then(
      () => {},
      () => {},
    );
    return task.finally(() => {
      if (this.current(account, epoch)) this.queued--;
    });
  }

  private async apply(patch: PreferencePatch): Promise<ChatPreferences> {
    this.synchronize();
    const account = this.account;
    const token = this.options.getToken();
    if (!account || !token || !this.state || this.disposed)
      throw new Error('Load your account privacy before changing it');
    if (this.saving) throw new Error('Please wait for the current preference change');
    if (this.uncertain) throw new Error('Refresh privacy to check the unconfirmed change first');
    const epoch = this.epoch;
    this.saving = true;
    this.operation++;
    // An older read can finish, but must not absorb an explicit reconciliation
    // after this write. Its operation fence still prevents it from applying.
    this.pending = null;
    this.emit();
    try {
      const state = await api.updatePreferences(token, patch, this.controller.signal);
      if (!this.current(account, epoch)) throw new Error('The account changed');
      this.state = state;
      this.failure = '';
      try {
        localStorage.setItem(CHANGE_KEY, crypto.randomUUID());
      } catch {
        // Focus and bounded polling still reconcile when storage is restricted.
      }
      return structuredClone(state);
    } catch (error) {
      if (this.current(account, epoch)) {
        this.uncertain = error instanceof ApiOutcomeUnknownError;
        // Preserve restrictive intent locally while the server outcome is unknown.
        if (this.uncertain && this.state) {
          if (patch.allowPrivateMessages === false) this.state.allowPrivateMessages = false;
          if (patch.ignore) {
            this.state.ignored = this.state.ignored.filter(
              (person) => person.id !== patch.ignore?.id,
            );
            this.state.ignored.push({ ...patch.ignore });
          }
        }
        this.failure = this.uncertain
          ? 'This change was not confirmed. Refresh privacy before changing it again.'
          : error instanceof Error
            ? error.message
            : 'Privacy could not be saved';
      }
      throw error;
    } finally {
      if (this.current(account, epoch)) {
        this.saving = false;
        this.emit();
      }
    }
  }

  private mount(
    host: HTMLElement,
    current: () => boolean,
    peer?: { id: string; name: string },
  ): () => void {
    const section = el('div');
    host.append(section);
    const render = (): void => {
      if (!current()) return;
      const state = this.snapshot;
      section.replaceChildren();
      const fail = (error: unknown): void => {
        if (current())
          this.options.notify(error instanceof Error ? error.message : 'Privacy unavailable');
      };
      if (this.failure || !state) {
        const status = el('p', this.failure || 'Loading privacy…', 'setting-hint');
        status.setAttribute('role', this.failure ? 'alert' : 'status');
        section.append(status);
      }
      const reload = asyncButton('Refresh privacy', () => this.refresh(true), fail);
      reload.disabled = this.saving;
      if (!state || this.failure) section.append(reload);
      if (!state) return;
      if (peer) {
        const ignored = state.ignored.some((person) => person.id === peer.id);
        const control = asyncButton(
          ignored ? 'Unignore' : 'Ignore this person',
          async () => {
            await this.change(ignored ? { unignore: peer.id } : { ignore: peer });
          },
          fail,
        );
        control.disabled = this.saving || this.uncertain;
        section.append(
          group('Privacy', [control]),
          note(
            'Ignoring stops new private messages from this account. It is separate from muting notifications.',
          ),
        );
      } else {
        const allow = toggleRow(
          'Private messages from others',
          state.allowPrivateMessages,
          (value) => {
            this.change({ allowPrivateMessages: value }).catch(fail);
          },
        );
        allow.input.disabled = this.saving || this.uncertain;
        const people = state.ignored.map((person) => {
          const row = el('div', undefined, 'sheet-row sheet-row-person');
          const restore = asyncButton(
            'Unignore',
            async () => {
              await this.change({ unignore: person.id });
            },
            fail,
            'sheet-button',
          );
          restore.disabled = this.saving || this.uncertain;
          row.append(el('span', person.name, 'sheet-row-label'), restore);
          return row;
        });
        section.append(
          group(null, [allow.row]),
          group(
            'Ignored people',
            people.length
              ? people
              : [note('No one is ignored. Open a conversation in Messages to ignore an account.')],
          ),
          note(
            'These choices follow your account. Guest ignores remain specific to their room session.',
          ),
        );
      }
    };
    const stop = this.subscribe(() => {
      if (!current()) stop();
      else render();
    });
    render();
    this.refresh().catch(() => {});
    return stop;
  }

  mountPrivacy(host: HTMLElement, current: () => boolean): () => void {
    return this.mount(host, current);
  }

  mountConversation(
    host: HTMLElement,
    id: string,
    name: string,
    current: () => boolean,
  ): () => void {
    return this.mount(host, current, { id, name });
  }

  dispose(): void {
    this.disposed = true;
    this.controller.abort();
    this.events.abort();
    clearInterval(this.timer);
    this.listeners.clear();
  }
}
