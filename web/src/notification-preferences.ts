import { api, button, el } from './ui';
import { fieldRow, group, markSaved, note, toggleRow } from './settings-sheet';
import { accountUpdatesLive, onAccountUpdate } from './account-updates';
import {
  notificationAllowed,
  type NotificationPreferences as PreferenceState,
  type NotificationPolicy,
  type ConversationNotificationPolicy,
} from './notification-validation';

interface Options {
  getToken: () => string | null;
  getAccountId: () => string | null;
  notify: (message: string) => void;
}

const timeValue = (minute: number): string =>
  `${String(Math.floor(minute / 60)).padStart(2, '0')}:${String(minute % 60).padStart(2, '0')}`;
const timeMinute = (value: string): number => {
  if (!/^\d\d:\d\d$/.test(value)) return -1;
  const [hour, minute] = value.split(':').map(Number);
  return hour !== undefined && minute !== undefined && hour < 24 && minute < 60
    ? hour * 60 + minute
    : -1;
};

/** One bounded account snapshot gates sounds and foreground notices. The server
 * independently applies this policy before empty-body push delivery. */
export class NotificationPreferences {
  private readonly options: Options;

  private account: string | null;
  private state: PreferenceState | null = null;
  private loadedAt = 0;
  private epoch = 0;
  private request = 0;
  private saving = false;
  private pending: Promise<void> | null = null;
  private controller = new AbortController();
  private readonly listeners = new AbortController();
  private readonly timer: ReturnType<typeof setInterval>;
  private disposed = false;
  private dirty = false;
  private reconciled = false;
  private readFailed = false;
  private tail: Promise<void> = Promise.resolve();
  private queued = 0;
  private readonly views = new Set<() => boolean>();
  private readonly stopUpdates: () => void;

  constructor(options: Options) {
    this.options = options;

    this.account = options.getAccountId();
    const refresh = () => {
      if (!document.hidden) this.refresh().catch(() => {});
    };
    const repair = () => {
      if (
        this.account === this.options.getAccountId() &&
        this.state &&
        this.reconciled &&
        !this.dirty &&
        !this.readFailed &&
        accountUpdatesLive(this.account)
      )
        return;
      refresh();
    };
    window.addEventListener('focus', repair, { signal: this.listeners.signal });
    document.addEventListener('visibilitychange', repair, { signal: this.listeners.signal });
    this.timer = setInterval(() => {
      this.updateViews();
      if (this.readFailed || !this.reconciled || !accountUpdatesLive(this.options.getAccountId()))
        refresh();
    }, 30000);
    this.stopUpdates = onAccountUpdate((update) => {
      if (
        update.account !== this.options.getAccountId() ||
        (update.scope !== 'ready' && update.scope !== 'notifications')
      )
        return;
      if (this.account !== update.account) this.reset();
      this.dirty = true;
      this.reconciled = false;
      refresh();
    });
  }

  reset(): void {
    this.controller.abort();
    this.controller = new AbortController();
    this.account = this.options.getAccountId();
    this.epoch++;
    this.request++;
    this.pending = null;
    this.saving = false;
    this.state = null;
    this.loadedAt = 0;
    this.dirty = false;
    this.reconciled = false;
    this.readFailed = false;
    this.tail = Promise.resolve();
    this.queued = 0;
    this.updateViews();
  }

  private current(account: string, epoch: number): boolean {
    return (
      !this.disposed &&
      account === this.account &&
      account === this.options.getAccountId() &&
      epoch === this.epoch
    );
  }

  refresh(): Promise<void> {
    if (this.account !== this.options.getAccountId()) this.reset();
    const account = this.account;
    const token = this.options.getToken();
    if (this.disposed || !account || !token || this.saving) return Promise.resolve();
    if (this.pending) return this.pending;
    this.dirty = false;
    const epoch = this.epoch;
    const request = ++this.request;
    const task = api
      .notificationPreferences(token, this.controller.signal)
      .then((state) => {
        if (this.current(account, epoch) && request === this.request) {
          this.state = state;
          this.readFailed = false;
          this.loadedAt = Date.now();
          this.reconciled = !this.dirty;
          this.updateViews();
        }
      })
      .catch(() => {
        // Retain this account's last policy on transient failures; never borrow
        // another account's policy or enable alerts before the first response.
        if (this.current(account, epoch) && request === this.request) this.readFailed = true;
      })
      .finally(() => {
        if (this.pending === task) {
          this.pending = null;
          if (this.dirty) this.refresh().catch(() => {});
        }
      });
    this.pending = task;
    return task;
  }

  allows(kind: 'private' | 'mention' | 'room', peerId?: string): boolean {
    if (this.account !== this.options.getAccountId()) this.reset();
    if (!this.account) return true;
    return (
      this.state !== null &&
      !this.saving &&
      this.queued === 0 &&
      this.reconciled &&
      !this.dirty &&
      (Date.now() - this.loadedAt < 90000 || accountUpdatesLive(this.account)) &&
      notificationAllowed(this.state, kind, peerId)
    );
  }

  private updateViews(): void {
    for (const update of this.views) if (!update()) this.views.delete(update);
  }

  private save(
    policy: Partial<NotificationPolicy> | ConversationNotificationPolicy,
    peerId?: string,
  ): Promise<boolean> {
    const account = this.account;
    const epoch = this.epoch;
    if (!account || !this.current(account, epoch)) return Promise.resolve(false);
    if (this.queued >= 20) {
      this.options.notify('Please wait for the current notification changes');
      return Promise.resolve(false);
    }
    this.queued++;
    const task = this.tail.then(() => {
      if (!this.current(account, epoch)) return false;
      return this.apply(policy, peerId);
    });
    this.tail = task.then(
      () => {},
      () => {},
    );
    return task.finally(() => {
      if (this.current(account, epoch)) this.queued--;
    });
  }

  private async apply(
    policy: Partial<NotificationPolicy> | ConversationNotificationPolicy,
    peerId?: string,
  ): Promise<boolean> {
    const account = this.account;
    const token = this.options.getToken();
    const epoch = this.epoch;
    if (!account || !token || this.saving || !this.current(account, epoch)) return false;
    this.saving = true;
    ++this.request;
    this.pending = null;
    try {
      const state = peerId
        ? await api.saveConversationNotifications(
            token,
            peerId,
            policy as ConversationNotificationPolicy,
            this.controller.signal,
          )
        : await api.patchNotificationPreferences(
            token,
            policy as Partial<NotificationPolicy>,
            this.controller.signal,
          );
      if (!this.current(account, epoch)) return false;
      this.state = state;
      this.readFailed = false;
      this.loadedAt = Date.now();
      this.reconciled = !this.dirty;
      this.updateViews();
      return true;
    } catch (error) {
      if (this.current(account, epoch)) {
        this.dirty = true;
        this.reconciled = false;
        this.options.notify(
          error instanceof Error ? error.message : 'Notification preferences could not be saved',
        );
      }
      return false;
    } finally {
      if (this.current(account, epoch)) {
        this.saving = false;
        if (this.dirty) this.refresh().catch(() => {});
      }
    }
  }

  /** Each row owns only its explicit field intent. Snapshot updates reconcile
   * other fields while queued local choices keep their visible value. */
  mountAccount(container: HTMLElement, current: () => boolean): void {
    if (this.account !== this.options.getAccountId()) this.reset();
    const section = el('div', undefined, 'sheet-group notification-preferences');
    const status = el('p', 'Loading notification rules…', 'sheet-status');
    status.setAttribute('role', 'status');
    section.append(el('div', 'Alert me about', 'sheet-group-title'), status);
    container.append(section);
    const account = this.options.getAccountId();
    const epoch = this.epoch;
    const active = () => current() && Boolean(account && this.current(account, epoch));
    type Key = keyof NotificationPolicy;
    const pending = new Map<Key, { version: number; value: NotificationPolicy[Key] }>();
    let version = 0;
    let loaded = false;
    let failure = '';
    let quiet = {
      startMinute: 1320,
      endMinute: 420,
      timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone,
    };
    const persist = (row: HTMLElement, change: Partial<NotificationPolicy>): void => {
      if (!active()) return;
      const revision = ++version;
      const keys = Object.keys(change) as Key[];
      for (const key of keys)
        pending.set(key, { version: revision, value: change[key] as NotificationPolicy[Key] });
      failure = '';
      render();
      this.save(change)
        .then((saved) => {
          if (!active()) return;
          for (const key of keys) if (pending.get(key)?.version === revision) pending.delete(key);
          failure = saved
            ? ''
            : 'Change was not confirmed. Check the current rules before trying again.';
          if (saved && keys.every((key) => !pending.has(key))) markSaved(row);
          render();
        })
        .catch(() => {
          if (!active()) return;
          for (const key of keys) if (pending.get(key)?.version === revision) pending.delete(key);
          failure = 'Notification rules could not be saved. Try again.';
          render();
        });
    };
    const privateMessages = toggleRow('Private messages', false, (value) =>
      persist(privateMessages.row, { privateMessages: value }),
    );
    const mentions = toggleRow('Mentions', false, (value) =>
      persist(mentions.row, { mentions: value }),
    );
    const start = el('input');
    start.type = 'time';
    const end = el('input');
    end.type = 'time';
    const zone = el('input');
    zone.type = 'text';
    zone.maxLength = 64;
    zone.setAttribute('placeholder', 'America/New_York');
    const times = group(null, [
      fieldRow('From', start),
      fieldRow('Until', end),
      fieldRow('Time zone', zone),
    ]);
    times.classList.add('notification-quiet-times');
    const quietToggle = toggleRow('Quiet hours every day', false, (value) =>
      persist(quietToggle.row, { quietHours: value ? { ...quiet } : null }),
    );
    const applyTimes = (row: HTMLElement): void => {
      if (!active()) return;
      const startMinute = timeMinute(start.value),
        endMinute = timeMinute(end.value),
        timeZone = zone.value.trim();
      if (startMinute < 0 || endMinute < 0 || startMinute === endMinute) {
        failure = 'Choose different valid start and end times.';
        status.textContent = failure;
        return;
      }
      try {
        new Intl.DateTimeFormat('en', { timeZone });
      } catch {
        failure = 'Choose an IANA time zone, such as America/New_York.';
        status.textContent = failure;
        return;
      }
      failure = '';
      quiet = { startMinute, endMinute, timeZone };
      if (quietToggle.input.checked) persist(row, { quietHours: { ...quiet } });
    };
    for (const [input, row] of [
      [start, times.children[0]],
      [end, times.children[1]],
      [zone, times.children[2]],
    ] as const)
      input.addEventListener('change', () => applyTimes(row as HTMLElement));
    section.append(
      privateMessages.row,
      mentions.row,
      quietToggle.row,
      times,
      note('Applies on every device. Alerts suppressed while quiet are not replayed later.'),
    );
    const render = (): boolean => {
      if (!active()) return false;
      const state = this.state;
      for (const control of [
        privateMessages.input,
        mentions.input,
        quietToggle.input,
        start,
        end,
        zone,
      ])
        control.disabled = state === null;
      if (!state) {
        status.textContent = loaded
          ? 'Notification rules could not be loaded. Reopen Settings to try again.'
          : 'Loading notification rules…';
        return true;
      }
      privateMessages.input.checked = pending.has('privateMessages')
        ? pending.get('privateMessages')?.value === true
        : state.privateMessages;
      mentions.input.checked = pending.has('mentions')
        ? pending.get('mentions')?.value === true
        : state.mentions;
      const configured = pending.has('quietHours')
        ? (pending.get('quietHours')?.value as NotificationPolicy['quietHours'])
        : state.quietHours;
      quietToggle.input.checked = configured !== null;
      times.hidden = configured === null;
      if (configured) quiet = { ...configured };
      if (document.activeElement !== start) start.value = timeValue(quiet.startMinute);
      if (document.activeElement !== end) end.value = timeValue(quiet.endMinute);
      if (document.activeElement !== zone) zone.value = quiet.timeZone;
      status.textContent = pending.size ? 'Saving notification rules…' : failure;
      return true;
    };
    this.updateViews();
    this.views.add(render);
    render();
    this.refresh()
      .finally(() => {
        loaded = true;
        render();
      })
      .catch(() => {});
  }

  mountConversation(container: HTMLElement, peerId: string, current: () => boolean): void {
    if (this.account !== this.options.getAccountId()) this.reset();
    const section = el('details', undefined, 'notification-conversation');
    const status = el('p', 'Loading…', 'setting-hint');
    status.setAttribute('role', 'status');
    const controls = el('div', undefined, 'notification-conversation-actions');
    section.append(el('summary', 'Notifications'), status, controls);
    container.append(section);
    const account = this.options.getAccountId();
    const epoch = this.epoch;
    const active = () => current() && Boolean(account && this.current(account, epoch));
    let pending = false,
      loaded = false,
      failure = '';
    const update = (policy: ConversationNotificationPolicy): void => {
      if (!active() || pending) return;
      pending = true;
      failure = '';
      render();
      this.save(policy, peerId)
        .then((saved) => {
          if (active())
            failure = saved
              ? ''
              : 'Change was not confirmed. Check the current rules before trying again.';
        })
        .catch(() => {
          if (active()) failure = 'Notification rules could not be saved. Try again.';
        })
        .finally(() => {
          pending = false;
          render();
        });
    };
    const render = (): boolean => {
      if (!active()) return false;
      if (!this.state) {
        status.textContent = loaded
          ? 'Notification rules could not be loaded. Reopen this conversation to try again.'
          : 'Loading…';
        return true;
      }
      const preference = this.state.conversations.find((entry) => entry.peerId === peerId);
      const snoozed = Boolean(
        preference?.snoozedUntil && Date.parse(preference.snoozedUntil) > Date.now(),
      );
      status.textContent = pending
        ? 'Saving notification rule…'
        : failure ||
          (preference?.muted
            ? 'Muted. Messages still arrive.'
            : snoozed && preference?.snoozedUntil
              ? `Snoozed until ${new Date(preference.snoozedUntil).toLocaleString()}. Messages still arrive.`
              : 'Uses your account notification rules.');
      controls.replaceChildren(
        button(preference?.muted ? 'Unmute' : 'Mute', () =>
          update({ muted: !preference?.muted, snoozedUntil: null }),
        ),
        button('Snooze 1 hour', () =>
          update({ muted: false, snoozedUntil: new Date(Date.now() + 3600000).toISOString() }),
        ),
        button('Snooze 8 hours', () =>
          update({ muted: false, snoozedUntil: new Date(Date.now() + 28800000).toISOString() }),
        ),
      );
      if (snoozed)
        controls.append(
          button('Resume alerts', () => update({ muted: false, snoozedUntil: null })),
        );
      controls.querySelectorAll('button').forEach((control) => {
        control.disabled = pending;
      });
      return true;
    };
    this.updateViews();
    this.views.add(render);
    render();
    this.refresh()
      .finally(() => {
        loaded = true;
        render();
      })
      .catch(() => {});
  }

  dispose(): void {
    this.disposed = true;
    this.controller.abort();
    this.listeners.abort();
    this.stopUpdates();
    clearInterval(this.timer);
    this.state = null;
    this.views.clear();
  }
}
