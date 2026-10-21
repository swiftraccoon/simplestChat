import type { RoomClient } from '../room';
import { TIMESTAMP_FORMATS } from '../protocol';
import type {
  ChatEntry,
  ChatPreferences,
  ChatStyle,
  PreferencePatch,
  TimestampFormat,
} from '../protocol';
import { asyncButton, el } from '../ui';
import {
  actionRow,
  group,
  markSaved,
  note,
  selectRow,
  toggleRow,
  type ScreenContext,
  type SheetSection,
} from '../settings-sheet';
import { openRoomHistory } from '../chat-history';
import { CHAT_PALETTE, chatColor } from '../avatar-colors';
import { isRecord } from '../validation';
import type { SocialChatOptions } from './context';
import type { ChatStore } from './store';
import { AUTOMATIC_LOOK, LOOKS, validLook, formatChatTime } from './format';

/** Account-synced chat choices plus this browser's explicit notification consent and saved look. */
export type Preferences = ChatPreferences & {
  /** Desktop notices for mentions and private messages while the tab is out of sight. */
  notifications: boolean;
  /** Explicitly reveals message content and sender on the operating system’s lock screen. */
  notificationPreviews: boolean;
  /** The last look chosen here; a guest's next join asks for it. */
  look: ChatStyle | null;
};

interface PreferencesContext {
  readonly options: Pick<
    SocialChatOptions,
    | 'getRoom'
    | 'getViewerKey'
    | 'getToken'
    | 'accountPreferences'
    | 'notify'
    | 'mountConversationNotifications'
    | 'mountAccountNotifications'
  >;
  readonly store: ChatStore;
  readonly activation: number;
  readonly viewerKey: string;
  readonly activeRoom: RoomClient | null;
  readonly disposed: boolean;
  contextCurrent(activation: number, room: RoomClient, viewer: string): boolean;
  render(force?: boolean): void;
  switchConversation(id: string, name?: string): void;
  startReply(message: ChatEntry): void;
  notificationPermission(wanted: boolean): Promise<boolean>;
  closeDesktopNotices(): void;
  resumeSoundFromGesture(enabled?: boolean): void;
  playSound(frequency: number): void;
}

function defaultPreferences(): Preferences {
  return {
    allowPrivateMessages: true,
    sounds: false,
    notifications: false,
    notificationPreviews: false,
    timestamps: 'time',
    look: null,
    largeText: false,
    ignored: [],
  };
}

/**
 * Owns browser/account preferences, room-scoped ignores and their settings sections.
 * The host starts subscriptions after all collaborating controllers exist. Every
 * asynchronous apply belongs to the current chat activation, viewer and room;
 * reset retires pending applies and dispose removes the account subscription.
 */
export class ChatPreferencesController {
  private readonly context: PreferencesContext;

  preferences: Preferences = defaultPreferences();
  temporaryIgnored = new Map<string, string>();
  preferenceOperation = 0;
  preferenceBusy = false;
  accountPreferenceSnapshot = '';
  private stopAccountPreferences: (() => void) | undefined;
  private subscribed = false;
  private disposed = false;

  constructor(context: PreferencesContext) {
    this.context = context;
  }

  /** Start once the host has installed render, composer and notification owners. */
  subscribe(): void {
    if (this.disposed || this.subscribed) return;
    this.subscribed = true;
    this.stopAccountPreferences = this.context.options.accountPreferences?.subscribe(
      (account, saved) => {
        if (
          !this.disposed &&
          !this.context.disposed &&
          saved &&
          account === this.context.viewerKey &&
          account === this.context.options.getViewerKey()
        )
          this.applyAccountPreferences(saved);
      },
    );
  }

  /** Retire pending updates and forget room/viewer state before the host changes identity. */
  reset(): void {
    this.preferenceOperation++;
    this.preferenceBusy = false;
    this.accountPreferenceSnapshot = '';
    this.temporaryIgnored.clear();
    this.preferences = defaultPreferences();
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.stopAccountPreferences?.();
    this.stopAccountPreferences = undefined;
  }
  isIgnored(id: string): boolean {
    return (
      this.preferences.ignored.some((entry) => entry.id === id) || this.temporaryIgnored.has(id)
    );
  }

  async toggleIgnore(id: string, name: string, authenticated: boolean): Promise<void> {
    const account = this.context.options.accountPreferences;
    if (
      account &&
      this.context.options.getToken?.() &&
      (authenticated || this.preferences.ignored.some((person) => person.id === id))
    ) {
      await account.change(this.isIgnored(id) ? { unignore: id } : { ignore: { id, name } });
      return;
    }
    const room = this.context.options.getRoom();
    if (!room?.localParticipantId) throw new Error('Join a room first');
    if (id === room.localParticipantId) throw new Error('You cannot ignore yourself');
    const activation = this.context.activation,
      viewer = this.context.viewerKey;
    const next = structuredClone(this.preferences);
    const guests = new Map(this.temporaryIgnored);
    if (this.isIgnored(id)) {
      next.ignored = next.ignored.filter((entry) => entry.id !== id);
      guests.delete(id);
    } else {
      if (next.ignored.length + guests.size >= 100)
        throw new Error('Ignore list is full (100 people)');
      if (authenticated) next.ignored.push({ id, name: name.slice(0, 128) });
      else guests.set(id, name.slice(0, 128));
    }
    const operation = this.beginPreferenceUpdate();
    try {
      await this.pushPreferences(room, next, guests);
      if (
        !this.context.contextCurrent(activation, room, viewer) ||
        operation !== this.preferenceOperation
      )
        return;
      this.preferences = next;
      this.temporaryIgnored = guests;
      this.savePreferences(viewer);
      this.context.render();
    } finally {
      if (operation === this.preferenceOperation) this.preferenceBusy = false;
    }
  }

  private beginPreferenceUpdate(): number {
    if (this.preferenceBusy) throw new Error('Please wait for the current chat preference update');
    this.preferenceBusy = true;
    return ++this.preferenceOperation;
  }

  loadPreferences(): void {
    this.preferences = defaultPreferences();
    try {
      const value: unknown = JSON.parse(localStorage.getItem(this.preferenceKey()) ?? 'null');
      if (!isRecord(value)) return;
      this.preferences.allowPrivateMessages = value['allowPrivateMessages'] !== false;
      this.preferences.sounds = value['sounds'] === true;
      this.preferences.notifications = value['notifications'] === true;
      this.preferences.notificationPreviews = value['notificationPreviews'] === true;
      this.preferences.largeText = value['largeText'] === true;
      const timestamp = TIMESTAMP_FORMATS.find((format) => format === value['timestamps']);
      if (timestamp) this.preferences.timestamps = timestamp;
      this.preferences.look = validLook(value['look']);
      const ignored = new Map<string, { id: string; name: string }>();
      const entries = value['ignored'];
      if (Array.isArray(entries)) {
        const ignoredEntries: readonly unknown[] = entries;
        for (const entry of ignoredEntries) {
          if (!isRecord(entry)) continue;
          const id = entry['id'];
          const name = entry['name'];
          if (
            typeof id !== 'string' ||
            !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id) ||
            id === this.context.store.localId ||
            typeof name !== 'string'
          )
            continue;
          ignored.set(id, { id, name: name.slice(0, 128) });
          if (ignored.size >= 100) break;
        }
      }
      this.preferences.ignored = [...ignored.values()];
    } catch {
      /* Preferences are optional. */
    }
  }

  /** Local to this viewer: applies at once and needs no server. */
  setTimestampFormat(format: TimestampFormat): void {
    if (!TIMESTAMP_FORMATS.includes(format)) return;
    this.preferences = { ...this.preferences, timestamps: format };
    this.savePreferences(this.context.viewerKey, { timestamps: format });
    this.context.render();
  }

  /** The look saved for whoever joins next, before the room has confirmed anything. */
  savedLook(): ChatStyle | null {
    try {
      const value: unknown = JSON.parse(
        localStorage.getItem(this.preferenceKey(this.context.options.getViewerKey())) ?? 'null',
      );
      return isRecord(value) ? validLook(value['look']) : null;
    } catch {
      return null;
    }
  }

  private preferenceKey(viewer = this.context.viewerKey): string {
    return `simplestchat.chat.v1.${viewer}`;
  }

  private savePreferences(viewer = this.context.viewerKey, patch?: PreferencePatch): void {
    if (viewer !== this.context.viewerKey || viewer !== this.context.options.getViewerKey()) return;
    this.storePreferences(viewer);
    if (patch && this.context.options.getToken?.())
      this.context.options.accountPreferences?.change(patch).catch((error: unknown) => {
        if (!this.context.disposed && viewer === this.context.options.getViewerKey())
          this.context.options.notify(
            error instanceof Error ? error.message : 'Preferences could not be saved',
          );
      });
  }

  private storePreferences(viewer: string): void {
    try {
      localStorage.setItem(this.preferenceKey(viewer), JSON.stringify(this.preferences));
    } catch {
      /* Browser-local consent and guest preferences remain usable. */
    }
  }

  accountToken(viewer: string): string | null {
    return viewer === 'guest' ? null : (this.context.options.getToken?.() ?? null);
  }

  async loadAccountPreferences(current: () => boolean): Promise<void> {
    const account = this.context.options.accountPreferences;
    if (!account) throw new Error('Account privacy is unavailable');
    await account.refresh();
    const saved = account.snapshot;
    if (current() && saved) this.applyAccountPreferences(saved);
  }

  private applyAccountPreferences(saved: ChatPreferences): void {
    const snapshot = JSON.stringify(saved);
    if (snapshot === this.accountPreferenceSnapshot) return;
    this.accountPreferenceSnapshot = snapshot;
    this.preferences = {
      ...this.preferences,
      allowPrivateMessages: saved.allowPrivateMessages,
      sounds: saved.sounds,
      largeText: saved.largeText,
      timestamps: saved.timestamps,
      ignored: saved.ignored.map((person) => ({ ...person })),
    };
    this.storePreferences(this.context.viewerKey);
    this.context.render();
    const room = this.context.options.getRoom();
    if (room && room === this.context.activeRoom)
      this.pushPreferences(room, this.preferences, this.temporaryIgnored).catch(() => {});
  }

  async pushPreferences(
    room: RoomClient,
    preferences: Preferences,
    temporary: Map<string, string>,
  ): Promise<void> {
    await room.requestSocial('setChatPreferences', {
      allowPrivateMessages: preferences.allowPrivateMessages,
      ignoredParticipantIds: this.context.options.getToken?.()
        ? [...temporary.keys()]
        : [...preferences.ignored.map((entry) => entry.id), ...temporary.keys()],
    });
  }

  /** The settings sheet's chat sections; every control applies as it changes. */
  sections(): SheetSection[] {
    const available = (): boolean => Boolean(this.context.options.getRoom()?.localParticipantId);
    return [
      {
        id: 'chat-look',
        segment: 'you',
        label: 'Chat look',
        status: () => this.lookStatus(),
        available,
        screen: () => ({ title: 'Chat look', body: (context) => this.lookScreen(context) }),
      },
      {
        id: 'notifications',
        segment: 'you',
        label: 'Notifications',
        status: () => this.notificationStatus(),
        available,
        screen: () => ({
          title: 'Notifications',
          body: (context) => this.notificationScreen(context),
        }),
      },
      {
        id: 'privacy',
        segment: 'you',
        label: 'Privacy',
        status: () => this.privacyStatus(),
        available,
        screen: () => ({ title: 'Privacy', body: (context) => this.privacyScreen(context) }),
      },
    ];
  }

  private lookStatus(): string {
    const look = this.context.options.getRoom()?.chatStyle ?? AUTOMATIC_LOOK;
    const style = LOOKS.find((entry) => entry.kind === look.style)?.label ?? 'Stripe';
    const color = look.color
      ? look.color.charAt(0).toUpperCase() + look.color.slice(1)
      : 'Automatic';
    return `${color} · ${style}`;
  }

  private notificationStatus(): string {
    const parts = [
      this.preferences.sounds ? 'Sounds' : null,
      this.preferences.notifications ? 'Alerts' : null,
    ].filter(Boolean);
    return parts.length ? parts.join(' · ') : 'Off';
  }

  private privacyStatus(): string {
    const ignored = this.preferences.ignored.length + this.temporaryIgnored.size;
    const messages = this.preferences.allowPrivateMessages ? 'PMs on' : 'PMs off';
    return ignored ? `${messages} · ${ignored} ignored` : messages;
  }

  /** A screen's feedback stops when the sheet leaves it; its work stops when the chat context changes. */
  private screenContext(
    context: ScreenContext,
  ): { room: RoomClient; current: () => boolean; live: () => boolean; viewer: string } | null {
    const room = this.context.options.getRoom();
    if (!room?.localParticipantId) return null;
    const activation = this.context.activation;
    const viewer = this.context.viewerKey;
    const live = (): boolean => this.context.contextCurrent(activation, room, viewer);
    return { room, viewer, live, current: () => context.current() && live() };
  }

  private lookScreen(context: ScreenContext): HTMLElement {
    const scope = this.screenContext(context);
    const body = el('div', undefined, 'sheet-screen');
    if (!scope) return body;
    const { room, current, viewer, live } = scope;
    const lookBlock = el('div', undefined, 'sheet-block chat-look-block');
    let applying: Promise<void> = Promise.resolve();
    const look = this.lookPicker(room, () => {
      // A choice queues behind the one before it; leaving the screen must not drop it.
      const chosen = look.chosen();
      applying = applying.then(async () => {
        if (!live()) return;
        const before = room.chatStyle ?? AUTOMATIC_LOOK;
        if (chosen.color === before.color && chosen.style === before.style) return;
        try {
          const applied = await room.setChatStyle(chosen);
          if (!live()) return;
          this.preferences = { ...this.preferences, look: applied };
          this.savePreferences(viewer);
          this.context.render();
          if (current()) markSaved(lookBlock);
        } catch (error) {
          if (current())
            this.context.options.notify(
              error instanceof Error ? error.message : 'Unable to save the look',
            );
        }
      });
    });
    lookBlock.append(el('span', '', 'sheet-row-saved'), ...look.nodes);
    const large = toggleRow('Larger chat text', this.preferences.largeText, (value) => {
      this.preferences = { ...this.preferences, largeText: value };
      this.savePreferences(viewer, { largeText: value });
      this.context.render();
      markSaved(large.row);
    });
    // Each choice is shown as it looks, in the viewer's own time.
    const timestamps = el('select');
    timestamps.dataset['preference'] = 'timestamps';
    const now = new Date();
    for (const format of TIMESTAMP_FORMATS) {
      const option = el(
        'option',
        format === 'hover' ? 'Only when hovering' : formatChatTime(now, format),
      );
      option.value = format;
      timestamps.append(option);
    }
    timestamps.value = this.preferences.timestamps;
    const timestampRow = selectRow('Timestamps', timestamps);
    timestamps.addEventListener('change', () => {
      const format = TIMESTAMP_FORMATS.find((entry) => entry === timestamps.value);
      if (!format) {
        timestamps.value = this.preferences.timestamps;
        return;
      }
      this.preferences = { ...this.preferences, timestamps: format };
      this.savePreferences(viewer, { timestamps: format });
      this.context.render();
      markSaved(timestampRow);
    });
    const rows: HTMLElement[] = [large.row, timestampRow];
    if (room.roomSettings)
      rows.push(
        actionRow('Room history', () =>
          openRoomHistory(
            room,
            current,
            Boolean(this.context.options.getToken?.()),
            undefined,
            (message) => {
              context.close();
              this.context.switchConversation('public');
              this.context.startReply(message);
            },
          ),
        ),
      );
    body.append(lookBlock, group(null, rows));
    return body;
  }

  private notificationScreen(context: ScreenContext): HTMLElement {
    const scope = this.screenContext(context);
    const body = el('div', undefined, 'sheet-screen');
    if (!scope) return body;
    const { room, current, viewer } = scope;
    const sounds = toggleRow('Sounds', this.preferences.sounds, (value) => {
      // Audio must start during the click that enabled it.
      this.context.resumeSoundFromGesture(value);
      if (!current()) return;
      this.preferences = { ...this.preferences, sounds: value };
      this.savePreferences(viewer, { sounds: value });
      if (value) this.context.playSound(550);
      markSaved(sounds.row);
    });
    const rows: HTMLElement[] = [sounds.row];
    // Only where the browser can show desktop notices at all.
    if (typeof Notification !== 'undefined') {
      const notices = toggleRow(
        'Alerts on this device',
        this.preferences.notifications && Notification.permission === 'granted',
        (value) => {
          // Permission must be requested during the click, before any wait.
          const allowed = this.context.notificationPermission(value);
          allowed
            .then((granted) => {
              if (!current()) return;
              if (value && !granted) {
                notices.input.checked = false;
                this.context.options.notify(
                  'Notifications are blocked for this site in your browser settings',
                );
              }
              this.preferences = { ...this.preferences, notifications: granted };
              if (!granted) this.context.closeDesktopNotices();
              previews.row.hidden = !granted;
              this.savePreferences(viewer);
              markSaved(notices.row);
            })
            .catch(() => {});
        },
      );
      notices.input.dataset['preference'] = 'notifications';
      const previews = toggleRow(
        'Show message text',
        this.preferences.notificationPreviews,
        (value) => {
          if (!current()) return;
          this.preferences = { ...this.preferences, notificationPreviews: value };
          if (!value) this.context.closeDesktopNotices();
          this.savePreferences(viewer);
          markSaved(previews.row);
        },
      );
      previews.input.dataset['preference'] = 'notificationPreviews';
      previews.row.hidden = !notices.input.checked;
      rows.push(notices.row, previews.row);
    }
    body.append(
      group(null, rows),
      note('Alerts with message text can show on a lock screen; without it they stay generic.'),
    );
    // The open private conversation's own mute and snooze come first.
    const peer = this.context.store.active;
    if (
      peer !== 'public' &&
      this.context.options.getToken?.() &&
      room.getParticipants().get(peer)?.authenticated
    )
      this.context.options.mountConversationNotifications?.(
        body,
        peer,
        () => current() && this.context.store.active === peer,
      );
    if (this.context.options.getToken?.())
      this.context.options.mountAccountNotifications?.(body, current);
    return body;
  }

  private privacyScreen(context: ScreenContext): HTMLElement {
    if (this.context.options.getToken?.() && this.context.options.accountPreferences) {
      const body = el('div', undefined, 'sheet-screen');
      const viewer = this.context.options.getViewerKey();
      this.context.options.accountPreferences.mountPrivacy(
        body,
        () => context.current() && viewer === this.context.options.getViewerKey(),
      );
      return body;
    }
    const scope = this.screenContext(context);
    const body = el('div', undefined, 'sheet-screen');
    if (!scope) return body;
    const { room, current, viewer } = scope;
    const allow = toggleRow(
      'Private messages from others',
      this.preferences.allowPrivateMessages,
      (value) => {
        const previous = this.preferences.allowPrivateMessages;
        let operation: number;
        try {
          operation = this.beginPreferenceUpdate();
        } catch (error) {
          allow.input.checked = previous;
          this.context.options.notify(error instanceof Error ? error.message : 'Please wait');
          return;
        }
        const next = { ...this.preferences, allowPrivateMessages: value };
        this.pushPreferences(room, next, new Map(this.temporaryIgnored))
          .then(() => {
            if (!current() || operation !== this.preferenceOperation) return;
            this.preferences = next;
            this.savePreferences(viewer);
            this.context.render();
            markSaved(allow.row);
          })
          .catch((error: unknown) => {
            if (!current() || operation !== this.preferenceOperation) return;
            allow.input.checked = previous;
            this.context.options.notify(
              error instanceof Error ? error.message : 'Unable to save preferences',
            );
          })
          .finally(() => {
            if (operation === this.preferenceOperation) this.preferenceBusy = false;
          });
      },
    );
    const ignored = [
      ...this.preferences.ignored,
      ...[...this.temporaryIgnored].map(([id, name]) => ({ id, name })),
    ];
    const people: HTMLElement[] = [];
    for (const person of ignored) {
      const row = el('div', undefined, 'sheet-row sheet-row-person');
      const restore = asyncButton(
        'Unignore',
        async () => {
          if (!current()) return;
          await this.toggleIgnore(person.id, person.name, false);
          if (current() && !this.isIgnored(person.id)) row.remove();
        },
        (error) => {
          if (current())
            this.context.options.notify(
              error instanceof Error ? error.message : 'Unable to restore messages',
            );
        },
        'sheet-button',
      );
      row.append(el('span', person.name, 'sheet-row-label'), restore);
      people.push(row);
    }
    body.append(
      group(null, [allow.row]),
      group(
        'Ignored people',
        people.length
          ? people
          : [note('No one is ignored. Use a person’s menu to ignore their messages.')],
      ),
      note(
        this.accountToken(viewer)
          ? 'Saved to your account, so they follow you to other devices. Guest ignores last for this room.'
          : 'Saved in this browser. Guest ignores last for this room.',
      ),
    );
    return body;
  }

  /** Swatches and styles for the local look, with a live preview row. */
  private lookPicker(
    room: RoomClient,
    onChange?: () => void,
  ): { nodes: HTMLElement[]; chosen: () => ChatStyle } {
    const current = room.chatStyle ?? AUTOMATIC_LOOK;
    const nickname = room.nickname;
    const preview = el('div');
    preview.append(
      el('span', nickname, 'sender'),
      el('div', 'This is how your messages look to everyone.', 'msg-text'),
    );
    const radio = (name: string, value: string, checked: boolean): HTMLInputElement => {
      const node = el('input');
      node.type = 'radio';
      node.name = name;
      node.value = value;
      node.checked = checked;
      node.addEventListener('change', () => {
        show();
        onChange?.();
      });
      return node;
    };
    const swatch = (value: string, color: string): HTMLInputElement => {
      const node = radio('chat-color', value, (current.color ?? '') === value);
      node.className = 'chat-swatch';
      node.style.setProperty('--swatch', color);
      return node;
    };
    const automatic = swatch('', chatColor(nickname, null));
    const automaticLabel = el('label', undefined, 'chat-swatch-automatic');
    automaticLabel.append(automatic, el('span', 'Automatic'));
    // Two rows of eight follow the hue wheel: warm colors first, then cool.
    const palette = el('div', undefined, 'chat-palette');
    const swatches = el('div', undefined, 'chat-swatches');
    swatches.append(automaticLabel, palette);
    const colors = [automatic];
    for (const [token, hex] of Object.entries(CHAT_PALETTE)) {
      const node = swatch(token, hex);
      const name = token.charAt(0).toUpperCase() + token.slice(1);
      node.title = name;
      node.setAttribute('aria-label', name);
      colors.push(node);
      palette.append(node);
    }
    const colorGroup = el('fieldset', undefined, 'chat-look-group');
    colorGroup.append(el('legend', 'Color'), swatches);
    const styles = LOOKS.map(({ kind, label }) => {
      const node = radio('chat-look', kind, current.style === kind);
      const wrapper = el('label', undefined, 'chat-look-choice');
      wrapper.append(node, el('span', label));
      return { node, wrapper };
    });
    const styleGroup = el('fieldset', undefined, 'chat-look-group');
    styleGroup.append(el('legend', 'Style'), ...styles.map((style) => style.wrapper));
    const chosen = (): ChatStyle =>
      validLook({
        color: colors.find((node) => node.checked)?.value,
        style: styles.find((style) => style.node.checked)?.node.value,
      }) ?? AUTOMATIC_LOOK;
    const show = (): void => {
      const look = chosen();
      preview.className = `chat-msg chat-look-preview look-${look.style}`;
      preview.style.setProperty('--sender-color', chatColor(nickname, look.color));
    };
    show();
    return {
      nodes: [colorGroup, styleGroup, preview],
      chosen,
    };
  }
}
