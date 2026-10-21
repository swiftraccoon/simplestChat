import { createScope } from '../disposable';
import type { ChatEntry } from '../protocol';
import type { RoomClient } from '../room';
import type { ChatStore } from './store';
import type { SocialChatOptions } from './context';
import type { Preferences } from './preferences';

interface NotificationContext {
  readonly options: Pick<SocialChatOptions, 'getRoom' | 'allowsNotification'>;
  readonly preferences: Preferences;
  readonly activation: number;
  readonly activeRoom: RoomClient | null;
  readonly viewerKey: string;
  readonly store: ChatStore;
  readonly disposed: boolean;
  contextCurrent: (activation: number, room: RoomClient, viewer: string) => boolean;
  switchConversation: (id: string, name?: string) => void;
}

/** Owns optional sound output and bounded OS notices for the current chat identity. */
export class ChatNotifications {
  private readonly context: NotificationContext;

  private audio: AudioContext | null = null;
  private readonly desktopNotices = new Set<Notification>();
  private readonly events = createScope();

  constructor(context: NotificationContext) {
    this.context = context;

    document.addEventListener('pointerdown', () => this.resumeSoundFromGesture(), {
      signal: this.events.signal,
    });
    document.addEventListener('keydown', () => this.resumeSoundFromGesture(), {
      signal: this.events.signal,
    });
  }

  reset(): void {
    this.closeDesktopNotices();
    this.audio?.close().catch(() => {});
    this.audio = null;
  }

  dispose(): void {
    this.events.dispose();
    this.reset();
  }

  /** An opted-in desktop notice for a mention or private message while the tab is out of sight. */
  notifyDesktop(message: ChatEntry): void {
    if (!this.allowsNotification(message)) return;
    if (!this.context.preferences.notifications || typeof Notification === 'undefined') return;
    if (Notification.permission !== 'granted') return;
    if (!document.hidden && document.hasFocus?.() !== false) return;
    const nickname = this.context.options.getRoom()?.nickname;
    const direct = Boolean(message.recipientId);
    const mentioned = Boolean(
      nickname && message.content.toLowerCase().includes(`@${nickname.toLowerCase()}`),
    );
    if (!direct && !mentioned) return;
    try {
      const activation = this.context.activation;
      const room = this.context.activeRoom;
      const viewer = this.context.viewerKey;
      const preview = this.context.preferences.notificationPreviews;
      const notice = new Notification(
        preview
          ? direct
            ? `${message.participantName} sent you a private message`
            : `${message.participantName} mentioned you`
          : direct
            ? 'New private message'
            : 'You were mentioned',
        {
          body: preview ? message.content.slice(0, 160) : 'Open SimplestChat to read it.',
          tag: message.messageId,
        },
      );
      this.desktopNotices.add(notice);
      // Bound resources even if the OS does not dispatch notification close events.
      if (this.desktopNotices.size > 20) {
        const oldest = this.desktopNotices.values().next().value;
        oldest?.close();
        if (oldest) this.desktopNotices.delete(oldest);
      }
      notice.onclose = () => this.desktopNotices.delete(notice);
      notice.onclick = () => {
        notice.close();
        this.desktopNotices.delete(notice);
        if (!room || !this.context.contextCurrent(activation, room, viewer)) return;
        globalThis.focus?.();
        if (direct) this.context.switchConversation(message.participantId, message.participantName);
        else if (this.context.store.active !== 'public') this.context.switchConversation('public');
        document.querySelector<HTMLButtonElement>('[data-tab="chat"]')?.click();
      };
    } catch {
      /* Some browsers (Android Chrome) only notify through a service worker. */
    }
  }

  allowsNotification(message: ChatEntry): boolean {
    const nickname = this.context.options.getRoom()?.nickname;
    const kind = message.recipientId
      ? 'private'
      : nickname && message.content.toLowerCase().includes(`@${nickname.toLowerCase()}`)
        ? 'mention'
        : 'room';
    return (
      this.context.options.allowsNotification?.(
        kind,
        kind === 'private' ? message.participantId : undefined,
      ) ?? true
    );
  }

  closeDesktopNotices(): void {
    for (const notice of this.desktopNotices) notice.close();
    this.desktopNotices.clear();
  }

  /** Asks during the click that enabled notices; resolves whether they may be shown. */
  notificationPermission(wanted: boolean): Promise<boolean> {
    if (!wanted || typeof Notification === 'undefined') return Promise.resolve(false);
    if (Notification.permission !== 'default')
      return Promise.resolve(Notification.permission === 'granted');
    return Notification.requestPermission().then(
      (answer) => answer === 'granted',
      () => false,
    );
  }

  resumeSoundFromGesture(enabled = this.context.preferences.sounds): void {
    if (this.context.disposed) return;
    if (!enabled) return;
    try {
      if (!this.audio || this.audio.state === 'closed') this.audio = new AudioContext();
      if (this.audio.state === 'suspended')
        this.audio.resume().catch(() => {
          // Notification sounds are optional; denied output must never block chat.
        });
    } catch {
      /* Unsupported or denied audio must never block chat. */
    }
  }

  playSound(frequency: number): void {
    if (!this.audio || this.audio.state !== 'running') return;
    try {
      const oscillator = this.audio.createOscillator();
      const gain = this.audio.createGain();
      oscillator.frequency.value = frequency;
      gain.gain.setValueAtTime(0.06, this.audio.currentTime);
      gain.gain.exponentialRampToValueAtTime(0.001, this.audio.currentTime + 0.12);
      oscillator.connect(gain).connect(this.audio.destination);
      oscillator.start();
      oscillator.stop(this.audio.currentTime + 0.13);
      oscillator.onended = () => {
        oscillator.disconnect();
        gain.disconnect();
      };
    } catch {
      /* Output devices may disappear while the room is open. */
    }
  }
}
