import { api, button, el, field } from './ui';
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

  constructor(private readonly options: Options) {
    this.account = options.getAccountId();
    const refresh = () => {
      if (!document.hidden) this.refresh().catch(() => {});
    };
    window.addEventListener('focus', refresh, { signal: this.listeners.signal });
    document.addEventListener('visibilitychange', refresh, { signal: this.listeners.signal });
    this.timer = setInterval(refresh, 30000);
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
    const epoch = this.epoch;
    const request = ++this.request;
    const task = api
      .notificationPreferences(token, this.controller.signal)
      .then((state) => {
        if (this.current(account, epoch) && request === this.request) {
          this.state = state;
          this.loadedAt = Date.now();
        }
      })
      .catch(() => {
        // Retain this account's last policy on transient failures; never borrow
        // another account's policy or enable alerts before the first response.
      })
      .finally(() => {
        if (this.pending === task) this.pending = null;
      });
    this.pending = task;
    return task;
  }

  allows(kind: 'private' | 'mention' | 'room', peerId?: string): boolean {
    if (this.account !== this.options.getAccountId()) this.reset();
    if (!this.account) return true;
    return (
      this.state !== null &&
      Date.now() - this.loadedAt < 90000 &&
      notificationAllowed(this.state, kind, peerId)
    );
  }

  private async save(
    policy: NotificationPolicy | ConversationNotificationPolicy,
    peerId?: string,
  ): Promise<boolean> {
    const account = this.account;
    const token = this.options.getToken();
    const epoch = this.epoch;
    if (!account || !token || this.saving || !this.current(account, epoch)) return false;
    this.saving = true;
    ++this.request;
    try {
      const state = peerId
        ? await api.saveConversationNotifications(
            token,
            peerId,
            policy as ConversationNotificationPolicy,
            this.controller.signal,
          )
        : await api.saveNotificationPreferences(
            token,
            policy as NotificationPolicy,
            this.controller.signal,
          );
      if (!this.current(account, epoch)) return false;
      this.state = state;
      this.loadedAt = Date.now();
      return true;
    } catch (error) {
      if (this.current(account, epoch))
        this.options.notify(
          error instanceof Error ? error.message : 'Notification preferences could not be saved',
        );
      return false;
    } finally {
      if (this.current(account, epoch)) this.saving = false;
    }
  }

  mountAccount(container: HTMLElement, current: () => boolean): void {
    if (this.account !== this.options.getAccountId()) this.reset();
    const section = el('section', undefined, 'notification-preferences');
    section.append(el('h3', 'Notification rules'));
    const status = el('p', 'Loading notification rules…', 'setting-hint');
    status.setAttribute('role', 'status');
    section.append(status);
    container.append(section);
    const account = this.options.getAccountId();
    const epoch = this.epoch;
    const active = () => current() && Boolean(account && this.current(account, epoch));
    this.refresh()
      .then(() => {
        if (!active()) return;
        const state = this.state;
        if (!state) {
          status.textContent = 'Rules could not be loaded. Reopen Account to try again.';
          return;
        }
        const privateMessages = el('input');
        privateMessages.type = 'checkbox';
        privateMessages.checked = state.privateMessages;
        const mentions = el('input');
        mentions.type = 'checkbox';
        mentions.checked = state.mentions;
        const quiet = el('input');
        quiet.type = 'checkbox';
        quiet.checked = state.quietHours !== null;
        const start = el('input');
        start.type = 'time';
        start.value = timeValue(state.quietHours?.startMinute ?? 1320);
        const end = el('input');
        end.type = 'time';
        end.value = timeValue(state.quietHours?.endMinute ?? 420);
        const zone = el('input');
        zone.type = 'text';
        zone.maxLength = 64;
        zone.value = state.quietHours?.timeZone ?? Intl.DateTimeFormat().resolvedOptions().timeZone;
        zone.setAttribute('placeholder', 'America/New_York');
        const times = el('div', undefined, 'notification-quiet-times');
        times.append(field('From', start), field('Until', end), field('Time zone', zone));
        const toggleQuiet = () => {
          times.hidden = !quiet.checked;
        };
        quiet.addEventListener('change', toggleQuiet);
        toggleQuiet();
        const save = button('Save notification rules', () => {
          if (!active()) return;
          const startMinute = timeMinute(start.value);
          const endMinute = timeMinute(end.value);
          const timeZone = zone.value.trim();
          if (quiet.checked) {
            if (startMinute < 0 || endMinute < 0 || startMinute === endMinute) {
              status.textContent = 'Choose different valid start and end times.';
              return;
            }
            try {
              new Intl.DateTimeFormat('en', { timeZone });
            } catch {
              status.textContent = 'Choose an IANA time zone, such as America/New_York.';
              return;
            }
          }
          save.disabled = true;
          this.save({
            privateMessages: privateMessages.checked,
            mentions: mentions.checked,
            quietHours: quiet.checked ? { startMinute, endMinute, timeZone } : null,
          })
            .then((saved) => {
              if (active()) {
                save.disabled = false;
                status.textContent = saved
                  ? 'Notification rules saved for your account.'
                  : 'Rules were not saved. Try again.';
              }
            })
            .catch(() => {
              if (active()) {
                save.disabled = false;
                status.textContent = 'Rules could not be saved. Try again.';
              }
            });
        });
        status.textContent =
          'Applies across devices to alert sounds, desktop notifications, and background PM notifications. Messages and unread counts are unchanged.';
        section.append(
          field('Private-message alerts', privateMessages),
          field('Mention alerts', mentions),
          field('Quiet hours every day', quiet),
          times,
          el(
            'p',
            'Times use this time zone, including daylight-saving changes. Alerts suppressed while quiet are not replayed later.',
            'setting-hint',
          ),
          save,
        );
      })
      .catch(() => {
        if (active()) status.textContent = 'Notification rules could not be loaded.';
      });
  }

  mountConversation(container: HTMLElement, peerId: string, current: () => boolean): void {
    if (this.account !== this.options.getAccountId()) this.reset();
    const section = el('details', undefined, 'notification-conversation');
    section.append(el('summary', 'Notifications'));
    const status = el('p', 'Loading…', 'setting-hint');
    status.setAttribute('role', 'status');
    section.append(status);
    container.append(section);
    const account = this.options.getAccountId();
    const epoch = this.epoch;
    const active = () => current() && Boolean(account && this.current(account, epoch));
    this.refresh()
      .then(() => {
        if (!active()) return;
        if (!this.state) {
          status.textContent =
            'Notification rules could not be loaded. Reopen this conversation to try again.';
          return;
        }
        const controls = el('div', undefined, 'notification-conversation-actions');
        const render = () => {
          if (!active()) return;
          const preference = this.state?.conversations.find((entry) => entry.peerId === peerId);
          const snoozed = Boolean(
            preference?.snoozedUntil && Date.parse(preference.snoozedUntil) > Date.now(),
          );
          status.textContent = preference?.muted
            ? 'Muted. Messages still arrive.'
            : snoozed && preference?.snoozedUntil
              ? `Snoozed until ${new Date(preference.snoozedUntil).toLocaleString()}. Messages still arrive.`
              : 'Uses your account notification rules.';
          const update = (policy: ConversationNotificationPolicy) => {
            if (!active()) return;
            const buttons = controls.querySelectorAll('button');
            buttons.forEach((item) => {
              item.disabled = true;
            });
            this.save(policy, peerId)
              .then(() => {
                if (active()) render();
              })
              .catch(() => {
                if (active()) render();
              });
          };
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
        };
        render();
        section.append(controls);
      })
      .catch(() => {
        if (active()) status.textContent = 'Notification rules could not be loaded.';
      });
  }

  dispose(): void {
    this.disposed = true;
    this.controller.abort();
    this.listeners.abort();
    clearInterval(this.timer);
    this.state = null;
  }
}
