import type { TelemetryHandler, TelemetryOutcome } from './telemetry-types';
import type { RoomClient } from './room';
import type { ChatEntry, ChatStyle, ChatStyleKind, ServerMessage } from './protocol';
import { ChatStore, ConversationInputs, type ChatItem } from './chat-store';
import { api, asyncButton, button, busy, el, field, modal } from './ui';
import { CHAT_PALETTE, chatColor } from './avatar-colors';

/** How a viewer sees message times: on hover, or always in one format. */
type TimestampFormat = 'hover' | 'time' | 'time12' | 'seconds' | 'datetime';
const TIMESTAMP_FORMATS: readonly TimestampFormat[] = [
  'hover',
  'time',
  'time12',
  'seconds',
  'datetime',
];
type Preferences = {
  allowPrivateMessages: boolean;
  sounds: boolean;
  /** Desktop notices for mentions and private messages while the tab is out of sight. */
  notifications: boolean;
  largeText: boolean;
  timestamps: TimestampFormat;
  /** The last look chosen here; a guest's next join asks for it. */
  look: ChatStyle | null;
  ignored: { id: string; name: string }[];
};

const LOOKS: readonly { kind: ChatStyleKind; label: string }[] = [
  { kind: 'accent', label: 'Stripe' },
  { kind: 'text', label: 'Colored text' },
  { kind: 'bubble', label: 'Tinted bubble' },
];
const AUTOMATIC_LOOK: ChatStyle = { color: null, style: 'accent' };
/** The reactions the server accepts (`REACTIONS` in protocol.rs), in its order. */
export const CHAT_REACTIONS = ['👍', '❤️', '😂', '😮', '😢', '🎉', '🔥', '👏'] as const;

/** The start of a message on one line, as a reply quotes it. */
function excerpt(text: string): string {
  const flat = text.split(/\s+/).join(' ').trim();
  return flat.length > 140 ? `${flat.slice(0, 140).trimEnd()}…` : flat;
}

/** A stored or typed look reduced to the palette and the known styles, like the server does. */
function validLook(value: unknown): ChatStyle | null {
  if (typeof value !== 'object' || value === null) return null;
  const { color, style } = value as Record<string, unknown>;
  return {
    color:
      typeof color === 'string' && Object.prototype.hasOwnProperty.call(CHAT_PALETTE, color)
        ? color
        : null,
    style: LOOKS.some((look) => look.kind === style) ? (style as ChatStyleKind) : 'accent',
  };
}

const pad = (value: number): string => String(value).padStart(2, '0');

/** A message time in the viewer's format, always in their local time. */
function formatChatTime(date: Date, format: TimestampFormat): string {
  const time = `${pad(date.getHours())}:${pad(date.getMinutes())}`;
  switch (format) {
    case 'time':
      return time;
    case 'time12':
      return date.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit', hour12: true });
    case 'seconds':
      return `${time}:${pad(date.getSeconds())}`;
    case 'datetime':
      return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${time}:${pad(date.getSeconds())}`;
    case 'hover':
      return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }
}
type Options = {
  telemetry?: TelemetryHandler;
  getRoom: () => RoomClient | null;
  getViewerKey: () => string;
  /** The account's bearer token, when the viewer has one: its preferences then follow it. */
  getToken?: () => string | null;
  notify: (message: string) => void;
  participantAction: (id: string, name: string, x: number, y: number) => void;
};
type MessageRow = { node: HTMLElement; fingerprint: string };

export class SocialChat {
  private readonly store = new ChatStore(300, 256 * 1024);
  private readonly composition = new ConversationInputs();
  private readonly messages = document.getElementById('chat-messages')!;
  private readonly input = document.getElementById('chat-input') as HTMLInputElement;
  private readonly sendButton = document.getElementById('chat-send-btn') as HTMLButtonElement;
  private readonly select = el('select');
  private readonly conversationStatus = el('span', '', 'conversation-status');
  /** Who is composing in the visible conversation, until each notice expires. */
  private readonly typingLine = el('div', '', 'typing-line');
  private typing = new Map<string, { until: number; target: string | null }>();
  private typingTimer: ReturnType<typeof setTimeout> | null = null;
  private lastTypingSent = 0;
  private readonly closeButton = button('Close PM', () => this.closePrivate());
  private readonly emojiPanel = el('div', undefined, 'emoji-panel');
  private readonly pendingStarted = new Map<string, number>();
  private readonly pending = new Map<string, ReturnType<typeof setTimeout>>();
  private readonly retrying = new Set<string>();
  private chatSessionId: string | null = null;
  private chatSequence = 0;
  // Only the active conversation's visible, retained messages own DOM nodes.
  private readonly rows = new Map<string, MessageRow>();
  private preferences: Preferences = {
    allowPrivateMessages: true,
    sounds: false,
    notifications: false,
    timestamps: 'hover',
    look: null,
    largeText: false,
    ignored: [],
  };
  private temporaryIgnored = new Map<string, string>();
  private scope = '';
  private activeRoom: RoomClient | null = null;
  private viewerKey = '';
  private activation = 0;
  private membershipVersion = -1;
  private activationTask: Promise<void> | null = null;
  private activationSynced = false;
  private preferenceOperation = 0;
  private preferenceBusy = false;
  private accountSync = 0;
  private preferencesDialog: ReturnType<typeof modal> | null = null;
  private audio: AudioContext | null = null;
  private seenAtBottom = true;
  // Where reading last resumed: the first message that had arrived unseen.
  private dividerKey: string | null = null;
  private readonly divider = el('div', undefined, 'chat-divider');
  // People offered while an @mention is being typed, and the highlighted one.
  private readonly mentionList = el('ul', undefined, 'mention-list');
  private mentionOptions: { name: string; chatStyle?: ChatStyle }[] = [];
  private mentionActive = 0;
  // The message the next send answers, shown above the message box until sent or cancelled.
  private replyingTo: ChatItem | null = null;
  private readonly replyBar = el('div', undefined, 'reply-bar');

  constructor(private readonly options: Options) {
    const toolbar = el('div', undefined, 'conversation-toolbar');
    this.select.setAttribute('aria-label', 'Conversation');
    this.select.addEventListener('change', () => this.switchConversation(this.select.value));
    toolbar.append(
      this.select,
      this.closeButton,
      button('Chat options', () => this.openPreferences()),
    );
    document.getElementById('chat-panel')!.prepend(toolbar, this.conversationStatus);
    this.typingLine.hidden = true;
    this.typingLine.setAttribute('aria-live', 'polite');
    document.getElementById('chat-input-row')?.before(this.typingLine);
    const emojiButton = button(
      '☺',
      () => {
        this.emojiPanel.hidden = !this.emojiPanel.hidden;
      },
      'emoji-toggle',
    );
    emojiButton.title = 'Choose emoji';
    emojiButton.setAttribute('aria-label', 'Choose emoji');
    for (const emoji of ['😀', '😂', '😊', '❤️', '👍', '👋', '🎉', '🔥', '🤔', '😢', '🙌', '💯']) {
      this.emojiPanel.append(
        button(
          emoji,
          () => {
            this.insertText(
              emoji,
              this.input.selectionStart ?? this.input.value.length,
              this.input.selectionEnd ?? this.input.value.length,
            );
            this.emojiPanel.hidden = true;
            this.input.focus();
          },
          'emoji-choice',
        ),
      );
    }
    this.emojiPanel.hidden = true;
    document.getElementById('chat-input-row')!.prepend(emojiButton);
    document.getElementById('chat-input-row')!.before(this.emojiPanel);
    this.input.maxLength = 2000;
    this.mentionList.id = 'mention-list';
    this.mentionList.setAttribute('role', 'listbox');
    this.mentionList.setAttribute('aria-label', 'People to mention');
    this.mentionList.hidden = true;
    // Choosing with the pointer must leave focus in the message box.
    this.mentionList.addEventListener('mousedown', (event) => event.preventDefault());
    this.input.setAttribute('role', 'combobox');
    this.input.setAttribute('aria-autocomplete', 'list');
    this.input.setAttribute('aria-controls', 'mention-list');
    this.input.setAttribute('aria-expanded', 'false');
    document.getElementById('chat-input-row')!.before(this.mentionList);
    this.replyBar.hidden = true;
    document.getElementById('chat-input-row')!.before(this.replyBar);
    this.input.addEventListener('input', () => this.suggestMentions());
    this.input.addEventListener('input', () => this.noteTyping());
    this.input.addEventListener('blur', () => this.closeMentions());
    this.input.addEventListener('keydown', (event) => this.onKey(event));
    this.input.addEventListener('input', () =>
      this.composition.save(this.store.active, this.input.value),
    );
    this.sendButton.addEventListener('click', () => this.send());
    this.messages.setAttribute('role', 'log');
    this.messages.setAttribute('aria-label', 'Conversation messages');
    this.divider.setAttribute('role', 'separator');
    this.divider.append(el('span', 'New messages'));
    this.messages.addEventListener('scroll', () => {
      const wasAtBottom = this.seenAtBottom;
      this.seenAtBottom =
        this.messages.scrollHeight - this.messages.scrollTop - this.messages.clientHeight < 48;
      if (this.seenAtBottom && this.isVisible()) this.markActiveRead(!wasAtBottom);
      this.updateBadges();
    });
    document.getElementById('scroll-bottom-btn')!.addEventListener('click', () => {
      this.messages.scrollTop = this.messages.scrollHeight;
      this.seenAtBottom = true;
      if (this.isVisible()) this.markActiveRead(true);
      this.updateBadges();
    });
    document.addEventListener('visibilitychange', () => {
      if (!document.hidden) this.render();
    });
    document
      .querySelector<HTMLButtonElement>('[data-tab="chat"]')
      ?.addEventListener('click', () => queueMicrotask(() => this.render()));
    document.addEventListener('pointerdown', () => this.resumeSoundFromGesture());
    document.addEventListener('keydown', () => this.resumeSoundFromGesture());
    this.render();
  }

  async activate(): Promise<void> {
    const room = this.options.getRoom();
    if (!room?.localParticipantId) return;
    const viewerKey = this.options.getViewerKey();
    if (
      this.store.localId !== room.localParticipantId ||
      this.scope !== room.currentRoomId ||
      this.viewerKey !== viewerKey ||
      this.activeRoom !== room
    ) {
      // A restart gives guests new participant IDs. Preserve only their public
      // draft within this same room/viewer intent, never history or old PM
      // recipients. Explicit leave and identity changes still clear everything.
      const publicDraft =
        room.rejoiningAfterRestart &&
        this.activeRoom === room &&
        this.scope === room.currentRoomId &&
        this.viewerKey === viewerKey
          ? this.store.active === 'public'
            ? this.input.value
            : this.composition.draft('public')
          : '';
      this.reset();
      this.scope = room.currentRoomId ?? '';
      this.activeRoom = room;
      this.viewerKey = viewerKey;
      this.store.localId = room.localParticipantId;
      this.composition.save('public', publicDraft);
      this.input.value = publicDraft;
      this.loadPreferences();
    }
    if (this.membershipVersion !== room.membershipVersion) {
      this.retireDeliveryAttempts();
      this.activation++;
      this.preferenceOperation++;
      this.preferenceBusy = false;
      this.preferencesDialog?.close();
      this.preferencesDialog = null;
      this.membershipVersion = room.membershipVersion;
      this.activationSynced = false;
      this.activationTask = null;
    }
    if (this.activationTask) return this.activationTask;
    if (this.activationSynced) return;
    const activation = this.activation;
    this.render();
    const current = (): boolean => this.contextCurrent(activation, room, viewerKey);
    const task = (async () => {
      let success = true;
      // An account's saved copy replaces this browser's before the room hears
      // the ignore list; a guest's push stays synchronous with the activation.
      const token = this.accountToken(viewerKey);
      if (token) {
        try {
          await this.loadAccountPreferences(viewerKey, token);
        } catch {
          if (current())
            this.options.notify(
              'Saved chat preferences could not be loaded; using this browser’s copy',
            );
        }
        if (!current()) return;
      }
      try {
        await this.pushPreferences(room, this.preferences, this.temporaryIgnored);
      } catch (error) {
        success = false;
        if (current())
          this.options.notify(
            error instanceof Error ? error.message : 'Chat preferences could not be applied',
          );
      }
      if (!current()) return;
      // Replay still recovers joining text if a preference update failed.
      try {
        await room.requestSocial('getRoomSnapshot');
      } catch (error) {
        success = false;
        if (current())
          this.options.notify(
            error instanceof Error ? error.message : 'Chat history could not be refreshed',
          );
      }
      if (current()) {
        this.activationSynced = success;
        this.render();
      }
    })();
    this.activationTask = task;
    try {
      await task;
    } finally {
      if (this.activationTask === task) this.activationTask = null;
    }
  }

  reset(): void {
    this.activation++;
    this.preferenceOperation++;
    this.preferenceBusy = false;
    this.activationTask = null;
    this.activationSynced = false;
    this.membershipVersion = -1;
    this.preferencesDialog?.close();
    this.preferencesDialog = null;
    for (const timer of this.pending.values()) clearTimeout(timer);
    for (const id of this.pendingStarted.keys()) this.finishSend(id, 'superseded');
    this.pending.clear();
    this.retrying.clear();
    this.chatSessionId = null;
    this.chatSequence = 0;
    this.store.reset();
    for (const row of this.rows.values()) row.node.remove();
    this.rows.clear();
    this.dividerKey = null;
    this.divider.remove();
    this.cancelReply();
    this.composition.reset();
    this.temporaryIgnored.clear();
    this.typing.clear();
    this.lastTypingSent = 0;
    this.renderTyping();
    this.scope = '';
    this.activeRoom = null;
    this.viewerKey = '';
    this.preferences = {
      allowPrivateMessages: true,
      sounds: false,
      notifications: false,
      largeText: false,
      timestamps: 'hover',
      look: null,
      ignored: [],
    };
    this.seenAtBottom = true;
    this.emojiPanel.hidden = true;
    this.input.value = '';
    this.audio?.close().catch(() => {});
    this.audio = null;
    this.render();
  }

  handleEvent(message: ServerMessage): void {
    const room = this.options.getRoom();
    if (
      room?.localParticipantId &&
      (this.store.localId !== room.localParticipantId ||
        this.scope !== room.currentRoomId ||
        this.viewerKey !== this.options.getViewerKey() ||
        this.membershipVersion !== room.membershipVersion)
    ) {
      const viewerKey = this.options.getViewerKey();
      const membership = room.membershipVersion;
      this.activate().catch(() => {
        if (
          this.options.getRoom() === room &&
          room.membershipVersion === membership &&
          this.options.getViewerKey() === viewerKey
        )
          this.options.notify('Chat could not be initialized. Please reconnect.');
      });
    }
    if (message.type === 'chatReceived') this.receive(message);
    else if (message.type === 'privateMessageReceived') this.receive(message.message);
    else if (message.type === 'messageAck') this.receive(message.message);
    else if (message.type === 'socialError' && message.clientMessageId) {
      // The reply has the original message ID, not a retry-attempt ID. Once
      // unconfirmed, a delayed rejection cannot prove the original was unsent.
      const unknown =
        this.retrying.has(message.clientMessageId) ||
        this.store.messages.some(
          (item) =>
            item.participantId === this.store.localId &&
            item.clientMessageId === message.clientMessageId &&
            item.status === 'unknown',
        );
      this.finishSend(message.clientMessageId, unknown ? 'unknown' : 'denied');
      this.clearPending(message.clientMessageId);
      this.store.fail(
        message.clientMessageId,
        unknown ? `Delivery not confirmed. ${message.message}` : message.message,
        unknown,
      );
      this.render();
    } else if (message.type === 'socialResponse' && message.action === 'getRoomSnapshot') {
      const snapshot = message.data;
      if (snapshot.chatSessionId !== undefined) {
        if (this.chatSessionId !== null && this.chatSessionId !== snapshot.chatSessionId)
          this.retireDeliveryAttempts();
        this.chatSessionId = snapshot.chatSessionId;
      }
      // Ingest synchronously so replay retains the same ordering, privacy and
      // acknowledgement semantics, then reconcile the bounded view just once.
      for (const entry of snapshot.messages) this.ingest(entry, true);
      this.render();
    } else if (message.type === 'messageRetryResult') {
      this.finishSend(message.clientMessageId, 'unknown');
      this.clearPending(message.clientMessageId);
      const explanations: Record<typeof message.reason, string> = {
        session_changed: 'The room session changed.',
        receipt_expired: 'The server no longer retains this confirmation.',
        sequence_superseded:
          'A later message was accepted and this earlier attempt cannot be verified.',
        capacity: 'The room could not retain this confirmation. Try again within the retry window.',
        conflict: 'The original message identity could not be verified.',
        recipient_unconfirmed: 'The original private recipient session could not be verified.',
      };
      this.store.fail(
        message.clientMessageId,
        `Delivery not confirmed. ${explanations[message.reason]}`,
        true,
      );
      if (message.reason !== 'capacity') {
        const item = this.store.messages.find(
          (entry) =>
            entry.participantId === this.store.localId &&
            entry.clientMessageId === message.clientMessageId,
        );
        if (item) delete item.retry;
      }
      this.render();
    } else if (message.type === 'messageReactions') {
      if (this.store.setReactions(message.messageId, message.reactions)) this.render();
    } else if (message.type === 'nicknameChanged') {
      if (this.store.names.has(message.participantId))
        this.store.names.set(message.participantId, message.nickname);
      this.render();
    } else if (message.type === 'participantTyping') {
      this.noteRemoteTyping(message.participantId, message.targetParticipantId ?? null);
    }
  }

  /** Composing in an open conversation tells its recipients, at most every 2.5 s. */
  private noteTyping(): void {
    const room = this.options.getRoom();
    if (!room?.localParticipantId || !room.connected || !room.canChat || this.input.disabled)
      return;
    if (!this.input.value.trim()) return;
    const now = Date.now();
    if (now - this.lastTypingSent < 2500) return;
    this.lastTypingSent = now;
    room.sendTyping(this.store.active === 'public' ? undefined : this.store.active);
  }

  private noteRemoteTyping(id: string, target: string | null): void {
    if (id === this.store.localId) return;
    this.typing.set(id, { until: Date.now() + 4000, target });
    this.renderTyping();
  }

  /** "Alice is typing…" for the visible conversation only; notices expire on their own. */
  private renderTyping(): void {
    const now = Date.now();
    for (const [id, entry] of this.typing) if (entry.until <= now) this.typing.delete(id);
    const room = this.options.getRoom();
    const names: string[] = [];
    for (const [id, entry] of this.typing) {
      const visible =
        this.store.active === 'public'
          ? entry.target === null
          : id === this.store.active && entry.target === this.store.localId;
      if (!visible) continue;
      const name = room?.getParticipants().get(id)?.name ?? this.store.names.get(id);
      if (name) names.push(name);
    }
    this.typingLine.hidden = names.length === 0;
    this.typingLine.textContent =
      names.length === 0
        ? ''
        : names.length === 1
          ? `${names[0]} is typing…`
          : names.length === 2
            ? `${names[0]} and ${names[1]} are typing…`
            : 'Several people are typing…';
    if (this.typingTimer !== null) clearTimeout(this.typingTimer);
    this.typingTimer = null;
    if (this.typing.size) {
      const next = Math.min(...[...this.typing.values()].map((entry) => entry.until));
      this.typingTimer = setTimeout(() => this.renderTyping(), Math.max(0, next - now));
    }
  }

  participantsChanged(): void {
    this.render();
  }

  openPrivate(id: string, name: string): void {
    if (id === this.store.localId) return;
    if (this.store.names.size >= 100 && !this.store.names.has(id)) {
      this.options.notify('Close an existing conversation first');
      return;
    }
    this.switchConversation(id, name);
    document.querySelector<HTMLButtonElement>('[data-tab="chat"]')?.click();
    this.render(true);
    this.input.focus();
  }

  isIgnored(id: string): boolean {
    return (
      this.preferences.ignored.some((entry) => entry.id === id) || this.temporaryIgnored.has(id)
    );
  }

  async toggleIgnore(id: string, name: string, authenticated: boolean): Promise<void> {
    const room = this.options.getRoom();
    if (!room?.localParticipantId) throw new Error('Join a room first');
    if (id === room.localParticipantId) throw new Error('You cannot ignore yourself');
    const activation = this.activation,
      viewer = this.viewerKey;
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
      if (!this.contextCurrent(activation, room, viewer) || operation !== this.preferenceOperation)
        return;
      this.preferences = next;
      this.temporaryIgnored = guests;
      this.savePreferences(viewer);
      this.render();
    } finally {
      if (operation === this.preferenceOperation) this.preferenceBusy = false;
    }
  }

  system(text: string): void {
    if (!this.store.localId) return;
    const id = crypto.randomUUID();
    this.store.receive(
      {
        messageId: id,
        clientMessageId: id,
        participantId: '',
        participantName: '',
        content: text,
        sentAt: new Date().toISOString(),
      },
      true,
    );
    this.render();
  }

  private receive(message: ChatEntry, replay = false): void {
    if (this.ingest(message, replay)) this.render();
  }

  private ingest(message: ChatEntry, replay: boolean): boolean {
    if (!this.store.localId || this.isIgnored(message.participantId)) return false;
    if (
      message.recipientId &&
      message.participantId !== this.store.localId &&
      !this.preferences.allowPrivateMessages
    )
      return false;
    const added = this.store.receive(message, replay);
    if (message.participantId === this.store.localId) {
      this.finishSend(message.clientMessageId, 'ok');
      this.clearPending(message.clientMessageId);
    }
    if (added && message.participantId !== this.store.localId) {
      if (
        this.store.conversation(message) === this.store.active &&
        (!this.seenAtBottom || !this.isVisible())
      )
        this.store.markUnread(message);
      if (!replay && this.preferences.sounds) this.playSound(message.recipientId ? 700 : 440);
      if (!replay) this.notifyDesktop(message);
    }
    return true;
  }

  /** An opted-in desktop notice for a mention or private message while the tab is out of sight. */
  private notifyDesktop(message: ChatEntry): void {
    if (!this.preferences.notifications || typeof Notification === 'undefined') return;
    if (Notification.permission !== 'granted') return;
    if (!document.hidden && document.hasFocus?.() !== false) return;
    const nickname = this.options.getRoom()?.nickname;
    const direct = Boolean(message.recipientId);
    const mentioned = Boolean(
      nickname && message.content.toLowerCase().includes(`@${nickname.toLowerCase()}`),
    );
    if (!direct && !mentioned) return;
    try {
      const notice = new Notification(
        direct
          ? `${message.participantName} sent you a private message`
          : `${message.participantName} mentioned you`,
        { body: message.content.slice(0, 160), tag: message.messageId },
      );
      notice.onclick = () => {
        globalThis.focus?.();
        if (direct) this.switchConversation(message.participantId, message.participantName);
        else if (this.store.active !== 'public') this.switchConversation('public');
        document.querySelector<HTMLButtonElement>('[data-tab="chat"]')?.click();
        notice.close();
      };
    } catch {
      /* Some browsers (Android Chrome) only notify through a service worker. */
    }
  }

  /** Asks during the click that enabled notices; resolves whether they may be shown. */
  private notificationPermission(wanted: boolean): Promise<boolean> {
    if (!wanted || typeof Notification === 'undefined') return Promise.resolve(false);
    if (Notification.permission !== 'default')
      return Promise.resolve(Notification.permission === 'granted');
    return Notification.requestPermission().then(
      (answer) => answer === 'granted',
      () => false,
    );
  }

  private send(): void {
    const room = this.options.getRoom();
    const content = this.input.value.trim();
    if (!room?.localParticipantId || !room.connected || !content || this.input.disabled) return;
    if (new TextEncoder().encode(content).length > 4096) {
      this.options.notify('Message is too long (maximum 4096 bytes)');
      return;
    }
    if (this.pending.size >= 100) {
      this.options.notify('Please wait for pending messages');
      return;
    }
    const id = crypto.randomUUID();
    if (this.chatSequence >= Number.MAX_SAFE_INTEGER) return;
    const sequence = ++this.chatSequence;
    const recipientId = this.store.active === 'public' ? undefined : this.store.active;
    const recipientName = recipientId === undefined ? undefined : this.store.names.get(recipientId);
    // Sending shows the conversation is caught up; the divider has served its purpose.
    this.dividerKey = null;
    const answering =
      this.replyingTo && this.store.conversation(this.replyingTo) === this.store.active
        ? this.replyingTo
        : null;
    const retained = this.store.pending(
      {
        messageId: `pending:${id}`,
        ...(answering && {
          replyTo: {
            messageId: answering.messageId,
            participantId: answering.participantId,
            participantName: answering.participantName,
            excerpt: excerpt(answering.content),
          },
        }),
        clientMessageId: id,
        participantId: room.localParticipantId,
        participantName: room.nickname,
        ...(recipientId !== undefined && { recipientId }),
        ...(recipientName !== undefined && { recipientName }),
        content,
        sentAt: new Date().toISOString(),
      },
      this.chatSessionId === null
        ? undefined
        : {
            sequence,
            chatSessionId: this.chatSessionId,
            expiresAt: Date.now() + 300_000,
          },
    );
    if (!retained) {
      this.options.notify('This message could not be added to the conversation');
      return;
    }
    this.startPending(id);
    try {
      if (recipientId) room.sendPrivate(recipientId, content, id, sequence, answering?.messageId);
      else room.sendChat(content, id, sequence, answering?.messageId);
      this.composition.sent(this.store.active, content);
      this.input.value = '';
      this.cancelReply();
    } catch (error) {
      this.finishSend(id, 'error');
      this.clearPending(id);
      this.store.fail(id, error instanceof Error ? error.message : 'Send failed');
    }
    this.render(true);
  }

  private startPending(id: string): void {
    this.pendingStarted.set(id, performance.now());
    this.recordTelemetry({ name: 'chat_send', outcome: 'started' });
    this.pending.set(
      id,
      setTimeout(() => {
        this.finishSend(id, 'timeout');
        this.pending.delete(id);
        this.retrying.delete(id);
        this.store.fail(
          id,
          this.store.messages.some((message) => message.clientMessageId === id && message.retry)
            ? 'Delivery not confirmed. Retry checks whether this message was accepted; available for up to five minutes.'
            : 'Delivery not confirmed. This send has no retry session; reconnecting may recover its acknowledgement.',
          true,
        );
        this.render();
      }, 12_000),
    );
  }

  private retireDeliveryAttempts(): void {
    for (const id of this.pending.keys()) this.clearPending(id);
    this.store.retireAttempts();
    this.chatSessionId = null;
    this.chatSequence = 0;
  }

  private retry(message: ChatItem): void {
    const room = this.options.getRoom();
    if (message.status !== 'unknown' || !this.store.messages.includes(message)) return;
    const attempt = message.retry;
    if (
      !attempt ||
      Date.now() >= attempt.expiresAt ||
      room !== this.activeRoom ||
      room?.membershipVersion !== this.membershipVersion ||
      this.viewerKey !== this.options.getViewerKey() ||
      attempt.chatSessionId !== this.chatSessionId
    ) {
      delete message.retry;
      this.store.fail(
        message.clientMessageId,
        'Delivery not confirmed. The retry window or room session ended.',
        true,
      );
      this.render();
      return;
    }
    if (!room.connected || this.pending.size >= 100) {
      this.options.notify('Wait for the room connection before retrying');
      return;
    }
    message.status = 'pending';
    delete message.error;
    this.startPending(message.clientMessageId);
    this.retrying.add(message.clientMessageId);
    try {
      room.retryChat({
        clientMessageId: message.clientMessageId,
        sequence: attempt.sequence,
        chatSessionId: attempt.chatSessionId,
        content: message.content,
        ...(message.recipientId !== undefined && { targetParticipantId: message.recipientId }),
        ...(message.replyTo && { replyTo: message.replyTo.messageId }),
      });
    } catch {
      this.finishSend(message.clientMessageId, 'unknown');
      this.clearPending(message.clientMessageId);
      this.store.fail(
        message.clientMessageId,
        'Delivery not confirmed. Wait for the connection and retry.',
        true,
      );
    }
    this.render();
  }

  private restoreDraft(message: ChatItem): void {
    const room = this.options.getRoom();
    if (
      room !== this.activeRoom ||
      room?.membershipVersion !== this.membershipVersion ||
      this.viewerKey !== this.options.getViewerKey() ||
      !this.store.messages.includes(message)
    )
      return;
    const conversation = this.store.conversation(message);
    const draft =
      conversation === this.store.active ? this.input.value : this.composition.draft(conversation);
    if (draft.trim() && draft !== message.content) {
      this.options.notify('Keep or clear the existing draft before copying this message');
      return;
    }
    this.switchConversation(conversation, message.recipientName);
    this.input.value = message.content;
    this.composition.save(conversation, message.content);
    this.input.focus();
    if (message.status === 'unknown')
      this.options.notify(
        'Delivery was not confirmed. Sending a new message may duplicate the original.',
      );
  }

  private recordTelemetry(event: Parameters<TelemetryHandler>[0]): void {
    try {
      this.options.telemetry?.(event);
    } catch {
      /* Delivery state must not depend on its observer. */
    }
  }

  private finishSend(id: string, outcome: TelemetryOutcome): void {
    const started = this.pendingStarted.get(id);
    if (started === undefined) return;
    this.pendingStarted.delete(id);
    this.recordTelemetry({
      name: 'chat_send',
      outcome,
      durationMs: performance.now() - started,
    });
  }

  private clearPending(id: string): void {
    this.finishSend(id, 'superseded');
    const timer = this.pending.get(id);
    if (timer !== undefined) clearTimeout(timer);
    this.pending.delete(id);
    this.retrying.delete(id);
  }

  private render(forceScroll = false): void {
    const room = this.options.getRoom();
    const pendingIds = new Set(
      this.store.messages
        .filter((message) => message.status === 'pending')
        .map((message) => message.clientMessageId),
    );
    for (const id of this.pending.keys()) if (!pendingIds.has(id)) this.clearPending(id);
    const scrollTop = this.messages.scrollTop;
    const atBottom = this.seenAtBottom || forceScroll;
    this.select.replaceChildren();
    const publicUnread = this.store.unread.get('public') ?? 0;
    const publicOption = el('option', `Public chat${publicUnread ? ` (${publicUnread})` : ''}`);
    publicOption.value = 'public';
    this.select.append(publicOption);
    for (const [id, name] of this.store.names) {
      const online = room?.getParticipants().has(id);
      const unread = this.store.unread.get(id) ?? 0;
      const option = el(
        'option',
        `${name}${online ? '' : ' · offline'}${unread ? ` (${unread})` : ''}`,
      );
      option.value = id;
      this.select.append(option);
    }
    this.select.value = this.store.active;
    const privateChat = this.store.active !== 'public';
    this.closeButton.hidden = !privateChat;
    // Public chat needs no permanent banner; its mention hint lives in the composer.
    this.conversationStatus.hidden = !privateChat;
    this.renderTyping();
    this.conversationStatus.textContent = privateChat
      ? 'Private · available while both people are in this room · not saved after leaving'
      : '';
    const disabled =
      !room?.localParticipantId ||
      !room.connected ||
      !room.canChat ||
      (privateChat &&
        (!room.getParticipants().has(this.store.active) || this.isIgnored(this.store.active)));
    this.input.disabled = disabled;
    this.sendButton.disabled = disabled;
    this.input.placeholder = disabled
      ? privateChat
        ? 'This person is offline or chat is restricted'
        : 'Chat is currently restricted'
      : privateChat
        ? 'Write a private message…'
        : 'Type a message · @name to mention';
    this.messages.classList.toggle('large-chat-text', this.preferences.largeText);
    this.messages.classList.toggle('timestamps-always', this.preferences.timestamps !== 'hover');
    const visible = this.store.messages.filter(
      (message) =>
        this.store.conversation(message) === this.store.active &&
        !this.isIgnored(message.participantId),
    );
    const keys = new Set(visible.map((message) => this.rowKey(message)));
    for (const [key, row] of this.rows) {
      if (!keys.has(key)) {
        row.node.remove();
        this.rows.delete(key);
      }
    }
    let previousSender: string | undefined;
    let previousTime = 0;
    let position = this.messages.firstChild;
    for (const message of visible) {
      const node = this.messageRow(message, room?.nickname);
      const time = Date.parse(message.sentAt);
      if (position === this.divider) position = position.nextSibling;
      // The divider starts a new block even within one sender's run.
      const grouped =
        previousSender === message.participantId &&
        time - previousTime < 120_000 &&
        this.rowKey(message) !== this.dividerKey;
      if (node.classList.contains('grouped') !== grouped) node.classList.toggle('grouped', grouped);
      // Keep unchanged rows in place. insertBefore also handles the uncommon
      // chronological move when an acknowledgement updates a pending timestamp.
      if (node === position) position = position.nextSibling;
      else this.messages.insertBefore(node, position);
      previousSender = message.participantId;
      previousTime = time;
    }
    this.placeDivider();
    this.messages.scrollTop = atBottom ? this.messages.scrollHeight : scrollTop;
    if (forceScroll) this.seenAtBottom = true;
    if (this.seenAtBottom && this.isVisible() && this.markActiveRead()) this.placeDivider();
    this.updateBadges();
  }

  /**
   * Reading resumes: remember where the unseen messages began, then count them read.
   * Returns whether that moved the divider.
   */
  private markActiveRead(placeNow = false): boolean {
    const first = this.store.firstUnread(this.store.active);
    const key = first ? this.rowKey(first) : this.dividerKey;
    this.store.markRead(this.store.active);
    if (key === this.dividerKey) return false;
    this.dividerKey = key;
    if (placeNow) this.placeDivider();
    return true;
  }

  /** The divider sits above the first message that arrived unseen, while it is shown. */
  private placeDivider(): void {
    const row = this.dividerKey ? this.rows.get(this.dividerKey)?.node : undefined;
    if (!row?.isConnected) {
      this.divider.remove();
      return;
    }
    if (this.divider.nextSibling !== row) this.messages.insertBefore(this.divider, row);
    row.classList.toggle('grouped', false);
  }

  private rowKey(message: ChatEntry): string {
    // Match the store's correlation identity: the server replaces the provisional
    // message ID on acknowledgement without replacing this visible message.
    return JSON.stringify([
      message.participantId,
      message.clientMessageId,
      message.recipientId ?? 'public',
    ]);
  }

  private messageRow(message: ChatItem, nickname: string | undefined): HTMLElement {
    const { messageId, participantId, participantName, content, sentAt, status, error } = message;
    const local = participantId === this.store.localId;
    const mentioned = Boolean(
      nickname && !local && content.toLowerCase().includes(`@${nickname.toLowerCase()}`),
    );
    const look = participantId ? this.lookOf(message, local) : null;
    const color = look ? chatColor(participantName, look.color) : '';
    // ChatStore updates objects in place. Compare immutable display/action values,
    // never object identity; grouping depends on adjacent visible rows instead.
    const fingerprint = JSON.stringify([
      messageId,
      participantId,
      participantName,
      content,
      sentAt,
      status,
      error,
      local,
      mentioned,
      !!message.retry,
      this.preferences.timestamps,
      look?.style,
      color,
      message.replyTo,
      message.reactions,
    ]);
    const key = this.rowKey(message);
    const previous = this.rows.get(key);
    if (previous?.fingerprint === fingerprint) return previous.node;
    const node = previous?.node ?? el('div');
    node.className = `chat-msg${look ? ` look-${look.style}` : ' system'}${mentioned ? ' mentioned' : ''}`;
    if (look) node.style.setProperty('--sender-color', color);
    node.dataset['messageId'] = messageId;
    node.replaceChildren();
    if (participantId) {
      const sender = button(
        local ? 'You' : participantName,
        () => {
          const rect = sender.getBoundingClientRect();
          this.options.participantAction(participantId, participantName, rect.left, rect.bottom);
        },
        'sender chat-sender-button',
      );
      node.append(sender);
    }
    if (message.replyTo) {
      const reply = message.replyTo;
      const quote = button(
        this.isIgnored(reply.participantId)
          ? 'Reply to a message you have hidden'
          : `${reply.participantId === this.store.localId ? 'You' : reply.participantName}: ${reply.excerpt}`,
        () => this.showMessage(reply.messageId),
        'msg-reply',
      );
      quote.title = 'Show the message this answers';
      node.append(quote);
    }
    const text = el('div', undefined, 'msg-text');
    appendLinkedText(text, content, mentioned ? `@${nickname}` : undefined);
    node.append(text);
    this.appendReactions(node, message);
    if (participantId && status === 'sent') this.appendActions(node, message);
    if (participantId) {
      const meta = el('div', undefined, 'msg-time');
      const date = new Date(sentAt);
      meta.textContent = formatChatTime(date, this.preferences.timestamps);
      meta.title = date.toLocaleString();
      node.append(meta);
      // The timestamp is revealed on hover; delivery state must stay visible on its own.
      if (status === 'pending' || status === 'failed' || status === 'unknown') {
        const state = el(
          'div',
          status === 'pending' ? 'Sending…' : (error ?? 'Delivery failed'),
          'msg-status',
        );
        if (status !== 'pending') {
          state.classList.add('delivery-error');
          if (status === 'unknown' && message.retry)
            state.append(button('Retry same message', () => this.retry(message), 'auth-link-btn'));
          state.append(
            button(
              status === 'unknown' ? 'Copy to draft' : 'Edit & resend',
              () => this.restoreDraft(message),
              'auth-link-btn',
            ),
          );
        }
        node.append(state);
      }
    }
    this.rows.set(key, { node, fingerprint });
    return node;
  }

  /** Reaction chips; each toggles the viewer's own. Ignored people are left out. */
  private appendReactions(node: HTMLElement, message: ChatItem): void {
    const reactions = (message.reactions ?? [])
      .map((reaction) => ({
        emoji: reaction.emoji,
        people: reaction.participantIds.filter((id) => !this.isIgnored(id)),
      }))
      .filter((reaction) => reaction.people.length);
    if (!reactions.length) return;
    const bar = el('div', undefined, 'msg-reactions');
    for (const reaction of reactions) {
      const chip = button(
        `${reaction.emoji} ${reaction.people.length}`,
        () => this.react(message, reaction.emoji),
        'reaction-chip',
      );
      chip.setAttribute('aria-pressed', String(reaction.people.includes(this.store.localId ?? '')));
      chip.title = reaction.people.map((id) => this.personName(id)).join(', ');
      bar.append(chip);
    }
    node.append(bar);
  }

  /** React and Reply, floating above a message on hover, focus or a tap. */
  private appendActions(node: HTMLElement, message: ChatItem): void {
    const actions = el('div', undefined, 'msg-actions');
    const picker = el('div', undefined, 'reaction-picker');
    picker.hidden = true;
    for (const emoji of CHAT_REACTIONS)
      picker.append(
        button(
          emoji,
          () => {
            picker.hidden = true;
            this.react(message, emoji);
          },
          'msg-action',
        ),
      );
    const react = button(
      '☺',
      () => {
        picker.hidden = !picker.hidden;
      },
      'msg-action',
    );
    const reply = button('↩', () => this.startReply(message), 'msg-action');
    for (const [control, label] of [
      [react, 'Add a reaction'],
      [reply, 'Reply'],
    ] as const) {
      control.setAttribute('aria-label', label);
      control.title = label;
    }
    actions.append(react, reply, picker);
    node.append(actions);
    // Touch screens have no hover: a tap on the message shows its actions.
    node.onclick = (event) => {
      if (!(event.target as Element).closest('button, a')) node.classList.toggle('show-actions');
    };
  }

  private react(message: ChatItem, emoji: string): void {
    const room = this.options.getRoom();
    if (!room) return;
    room
      .requestSocial('reactToMessage', { messageId: message.messageId, emoji })
      .then((result) => {
        if (this.store.setReactions(result.messageId, result.reactions)) this.render();
      })
      .catch((error: unknown) =>
        this.options.notify(error instanceof Error ? error.message : 'Could not add the reaction'),
      );
  }

  private personName(id: string): string {
    if (id === this.store.localId) return 'You';
    return this.options.getRoom()?.getParticipants().get(id)?.name ?? 'someone who left';
  }

  /** Answer a message: the next send quotes it. */
  private startReply(message: ChatItem): void {
    this.replyingTo = message;
    const cancel = button('✕', () => this.cancelReply(), 'reply-bar-cancel');
    cancel.setAttribute('aria-label', 'Cancel reply');
    this.replyBar.replaceChildren(
      el(
        'span',
        `Replying to ${message.participantId === this.store.localId ? 'yourself' : message.participantName}`,
        'reply-bar-label',
      ),
      el('span', excerpt(message.content), 'reply-bar-excerpt'),
      cancel,
    );
    this.replyBar.hidden = false;
    this.input.focus();
  }

  private cancelReply(): void {
    this.replyingTo = null;
    this.replyBar.hidden = true;
    this.replyBar.replaceChildren();
  }

  /** Brings the message a reply quotes into view, if it is still in this chat. */
  private showMessage(messageId: string): void {
    const row = [...this.rows.values()].find(
      (entry) => entry.node.dataset['messageId'] === messageId,
    );
    if (!row) {
      this.options.notify('That message is no longer in this chat');
      return;
    }
    row.node.scrollIntoView?.({ block: 'center', behavior: 'smooth' });
    row.node.classList.toggle('flash', true);
    setTimeout(() => row.node.classList.toggle('flash', false), 1200);
  }

  /** The look a message was sent with; entries without one wear the sender's current look. */
  private lookOf(message: ChatEntry, local: boolean): ChatStyle {
    const room = this.options.getRoom();
    const current = local
      ? room?.chatStyle
      : room?.getParticipants().get(message.participantId)?.chatStyle;
    return message.chatStyle ?? current ?? AUTOMATIC_LOOK;
  }

  private updateBadges(): void {
    const total = [...this.store.unread.values()].reduce((sum, count) => sum + count, 0);
    document.title = total ? `(${Math.min(total, 999)}) simplestChat` : 'simplestChat';
    const badge = document.getElementById('unread-badge')!;
    const activeUnread = this.store.unread.get(this.store.active) ?? 0;
    badge.textContent = String(activeUnread);
    badge.hidden = !activeUnread;
    document.getElementById('scroll-bottom-btn')!.hidden = this.seenAtBottom;
    const tab = document.querySelector<HTMLButtonElement>('[data-tab="chat"]');
    if (tab) {
      let count = tab.querySelector('.conversation-unread');
      if (!count) {
        count = el('span', '', 'conversation-unread');
        tab.append(count);
      }
      count.textContent = total ? ` ${total}` : '';
    }
    for (const option of this.select.options) {
      const unread = this.store.unread.get(option.value) ?? 0;
      const name =
        option.value === 'public'
          ? 'Public chat'
          : `${this.store.names.get(option.value) ?? 'Conversation'}${this.options.getRoom()?.getParticipants().has(option.value) ? '' : ' · offline'}`;
      option.textContent = `${name}${unread ? ` (${unread})` : ''}`;
    }
  }

  private onKey(event: KeyboardEvent): void {
    if (event.isComposing) return;
    // Tab completes a typed or pasted "@name" even before the list has opened.
    if (event.key === 'Tab' && !this.mentionOptions.length) this.suggestMentions();
    if (this.mentionOptions.length) {
      const count = this.mentionOptions.length;
      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
        event.preventDefault();
        this.mentionActive =
          (this.mentionActive + (event.key === 'ArrowDown' ? 1 : count - 1)) % count;
        this.renderMentions();
        return;
      }
      if (event.key === 'Enter' || event.key === 'Tab') {
        event.preventDefault();
        this.chooseMention(this.mentionActive);
        return;
      }
      if (event.key === 'Escape') {
        event.preventDefault();
        event.stopPropagation();
        this.closeMentions();
        return;
      }
    }
    if (event.key === 'Escape' && this.replyingTo) {
      event.preventDefault();
      this.cancelReply();
      return;
    }
    if (event.key === 'Enter') {
      event.preventDefault();
      this.send();
    }
    if (
      event.key === 'ArrowUp' &&
      (this.input.value === '' || this.composition.isRecalling(this.store.active))
    ) {
      event.preventDefault();
      this.input.value = this.composition.recall(this.store.active, 'up', this.input.value);
    } else if (event.key === 'ArrowDown' && this.composition.isRecalling(this.store.active)) {
      event.preventDefault();
      this.input.value = this.composition.recall(this.store.active, 'down', this.input.value);
    }
  }

  /** The "@name" being typed just before the caret, if any. */
  private mentionQuery(): { query: string; start: number; end: number } | null {
    const end = this.input.selectionStart ?? this.input.value.length;
    const match = /(?:^|\s)@([^@\s]*)$/.exec(this.input.value.slice(0, end));
    return match ? { query: match[1]!, start: end - match[1]!.length - 1, end } : null;
  }

  /** Typing "@" and part of a name offers the people it could mean, best matches first. */
  private suggestMentions(): void {
    const typed = this.mentionQuery()?.query.toLowerCase();
    const people =
      typed === undefined ? [] : [...(this.options.getRoom()?.getParticipants().values() ?? [])];
    const name = (person: { name: string }): string => person.name.toLowerCase();
    const starts = people.filter((person) => name(person).startsWith(typed!));
    const contains = people.filter(
      (person) => !starts.includes(person) && name(person).includes(typed!),
    );
    this.mentionOptions = [...starts, ...contains].slice(0, 6);
    this.mentionActive = 0;
    this.renderMentions();
  }

  private renderMentions(): void {
    const open = this.mentionOptions.length > 0;
    this.mentionList.hidden = !open;
    this.input.setAttribute('aria-expanded', String(open));
    this.mentionList.replaceChildren(
      ...this.mentionOptions.map((person, index) => {
        const option = el('li');
        option.id = `mention-option-${index}`;
        option.setAttribute('role', 'option');
        option.setAttribute('aria-selected', String(index === this.mentionActive));
        const dot = el('span', undefined, 'mention-dot');
        dot.style.setProperty('--person-color', chatColor(person.name, person.chatStyle?.color));
        option.append(dot, el('span', person.name));
        option.addEventListener('click', () => this.chooseMention(index));
        return option;
      }),
    );
    if (open)
      this.input.setAttribute('aria-activedescendant', `mention-option-${this.mentionActive}`);
    else this.input.removeAttribute('aria-activedescendant');
  }

  private chooseMention(index: number): void {
    const person = this.mentionOptions[index];
    const query = this.mentionQuery();
    this.closeMentions();
    if (person && query) this.insertText(`@${person.name} `, query.start, query.end);
  }

  private closeMentions(): void {
    if (!this.mentionOptions.length) return;
    this.mentionOptions = [];
    this.renderMentions();
  }

  private insertText(text: string, start: number, end: number): void {
    const next = this.input.value.slice(0, start) + text + this.input.value.slice(end);
    if (next.length > 2000 || new TextEncoder().encode(next).length > 4096) {
      this.options.notify('Message is too long');
      return;
    }
    this.input.setRangeText(text, start, end, 'end');
    this.composition.save(this.store.active, this.input.value);
  }

  private switchConversation(id: string, name?: string): void {
    this.cancelReply();
    this.composition.save(this.store.active, this.input.value);
    const first = this.store.firstUnread(id);
    this.dividerKey = first ? this.rowKey(first) : null;
    this.store.open(id, name);
    this.input.value = this.composition.draft(id);
    this.render(true);
  }

  private closePrivate(): void {
    const id = this.store.active;
    if (id === 'public') return;
    for (const message of this.store.messages) {
      if (this.store.conversation(message) === id) this.clearPending(message.clientMessageId);
    }
    this.store.close(id);
    this.composition.close(id);
    this.input.value = this.composition.draft('public');
    this.render(true);
  }

  private isVisible(): boolean {
    return (
      !document.hidden &&
      document.getElementById('room-screen')?.hidden === false &&
      document.getElementById('chat-panel')?.classList.contains('active') === true
    );
  }

  private contextCurrent(activation: number, room: RoomClient, viewer: string): boolean {
    return (
      activation === this.activation &&
      this.options.getRoom() === room &&
      viewer === this.viewerKey &&
      viewer === this.options.getViewerKey() &&
      this.scope === room.currentRoomId &&
      this.store.localId === room.localParticipantId &&
      this.membershipVersion === room.membershipVersion
    );
  }

  private beginPreferenceUpdate(): number {
    if (this.preferenceBusy) throw new Error('Please wait for the current chat preference update');
    this.preferenceBusy = true;
    return ++this.preferenceOperation;
  }

  private loadPreferences(): void {
    this.preferences = {
      allowPrivateMessages: true,
      sounds: false,
      notifications: false,
      largeText: false,
      timestamps: 'hover',
      look: null,
      ignored: [],
    };
    try {
      const value = JSON.parse(
        localStorage.getItem(this.preferenceKey()) ?? 'null',
      ) as Partial<Preferences> | null;
      if (!value) return;
      this.preferences.allowPrivateMessages = value.allowPrivateMessages !== false;
      this.preferences.sounds = value.sounds === true;
      this.preferences.notifications = value.notifications === true;
      this.preferences.largeText = value.largeText === true;
      if (TIMESTAMP_FORMATS.includes(value.timestamps as TimestampFormat))
        this.preferences.timestamps = value.timestamps as TimestampFormat;
      this.preferences.look = validLook(value.look);
      const ignored = new Map<string, { id: string; name: string }>();
      if (Array.isArray(value.ignored))
        for (const entry of value.ignored) {
          if (
            typeof entry?.id !== 'string' ||
            !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(entry.id) ||
            entry.id === this.store.localId ||
            typeof entry.name !== 'string'
          )
            continue;
          ignored.set(entry.id, { id: entry.id, name: entry.name.slice(0, 128) });
          if (ignored.size >= 100) break;
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
    this.savePreferences();
    this.render();
  }

  /** The look saved for whoever joins next, before the room has confirmed anything. */
  savedLook(): ChatStyle | null {
    try {
      const value = JSON.parse(
        localStorage.getItem(this.preferenceKey(this.options.getViewerKey())) ?? 'null',
      ) as Partial<Preferences> | null;
      return validLook(value?.look);
    } catch {
      return null;
    }
  }

  private preferenceKey(viewer = this.viewerKey): string {
    return `simplestchat.chat.v1.${viewer}`;
  }
  private savePreferences(viewer = this.viewerKey): void {
    if (viewer !== this.viewerKey || viewer !== this.options.getViewerKey()) return;
    this.storePreferences(viewer);
    this.syncAccountPreferences(viewer);
  }
  private storePreferences(viewer: string): void {
    try {
      localStorage.setItem(this.preferenceKey(viewer), JSON.stringify(this.preferences));
    } catch {
      /* Private browsing/quota. */
    }
  }
  /** The token of a signed-in viewer; guests keep everything in this browser. */
  private accountToken(viewer: string): string | null {
    if (viewer === 'guest') return null;
    return this.options.getToken?.() ?? null;
  }
  /** Replace the synced subset with the account's saved copy, when the viewer has one. */
  private async loadAccountPreferences(viewer: string, token: string): Promise<void> {
    const saved = await api.accountPreferences(token);
    if (this.viewerKey !== viewer) return;
    const ignored = new Map<string, { id: string; name: string }>();
    for (const entry of saved.ignored) {
      if (entry.id === this.store.localId || ignored.size >= 100) continue;
      ignored.set(entry.id, { id: entry.id, name: entry.name.slice(0, 128) });
    }
    this.preferences = {
      ...this.preferences,
      allowPrivateMessages: saved.allowPrivateMessages,
      sounds: saved.sounds,
      largeText: saved.largeText,
      timestamps: TIMESTAMP_FORMATS.includes(saved.timestamps as TimestampFormat)
        ? (saved.timestamps as TimestampFormat)
        : 'hover',
      ignored: [...ignored.values()],
    };
    this.storePreferences(viewer);
  }
  /** Save an account's synced subset; the newest write wins, and only the newest failure is reported. */
  private syncAccountPreferences(viewer: string): void {
    const token = this.accountToken(viewer);
    if (!token) return;
    const { allowPrivateMessages, sounds, largeText, timestamps, ignored } = this.preferences;
    const attempt = ++this.accountSync;
    api
      .updatePreferences(token, { allowPrivateMessages, sounds, largeText, timestamps, ignored })
      .catch(() => {
        if (attempt === this.accountSync && this.viewerKey === viewer)
          this.options.notify('Chat preferences could not be saved to your account');
      });
  }
  private async pushPreferences(
    room: RoomClient,
    preferences: Preferences,
    temporary: Map<string, string>,
  ): Promise<void> {
    await room.requestSocial('setChatPreferences', {
      allowPrivateMessages: preferences.allowPrivateMessages,
      ignoredParticipantIds: [...preferences.ignored.map((entry) => entry.id), ...temporary.keys()],
    });
  }

  private openPreferences(): void {
    const room = this.options.getRoom();
    if (!room?.localParticipantId) return;
    this.preferencesDialog?.close();
    const activation = this.activation,
      viewer = this.viewerKey;
    const view = modal('Chat preferences');
    this.preferencesDialog = view;
    view.dialog.addEventListener(
      'close',
      () => {
        if (this.preferencesDialog === view) this.preferencesDialog = null;
      },
      { once: true },
    );
    const current = (): boolean =>
      this.preferencesDialog === view &&
      view.dialog.open &&
      this.contextCurrent(activation, room, viewer);
    const allow = el('input');
    allow.type = 'checkbox';
    allow.checked = this.preferences.allowPrivateMessages;
    const sounds = el('input');
    sounds.type = 'checkbox';
    sounds.checked = this.preferences.sounds;
    const large = el('input');
    large.type = 'checkbox';
    large.checked = this.preferences.largeText;
    // Only where the browser can show desktop notices at all.
    const notices = typeof Notification === 'undefined' ? null : el('input');
    if (notices) {
      notices.type = 'checkbox';
      notices.dataset['preference'] = 'notifications';
      notices.checked = this.preferences.notifications && Notification.permission === 'granted';
    }
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
    const look = this.lookPicker(room);
    view.body.append(
      ...look.nodes,
      field('Allow incoming private messages', allow),
      field('Message and PM sounds', sounds),
      ...(notices ? [field('Desktop notifications for mentions and PMs', notices)] : []),
      field('Larger chat text', large),
      field('Timestamps', timestamps),
    );
    const save = asyncButton(
      'Save preferences',
      () =>
        busy(save, view.error, async () => {
          if (!current()) return;
          const operation = this.beginPreferenceUpdate();
          const chosen = look.chosen();
          const before = room.chatStyle ?? AUTOMATIC_LOOK;
          const lookChanged = chosen.color !== before.color || chosen.style !== before.style;
          const next = {
            ...this.preferences,
            allowPrivateMessages: allow.checked,
            sounds: sounds.checked,
            largeText: large.checked,
            timestamps: TIMESTAMP_FORMATS.includes(timestamps.value as TimestampFormat)
              ? (timestamps.value as TimestampFormat)
              : this.preferences.timestamps,
          };
          // Audio and notification permission must start during the click, before any wait.
          this.resumeSoundFromGesture(sounds.checked);
          const noticesAllowed = this.notificationPermission(notices?.checked === true);
          try {
            next.notifications = await noticesAllowed;
            if (notices?.checked && !next.notifications)
              this.options.notify(
                'Notifications are blocked for this site in your browser settings',
              );
            await this.pushPreferences(room, next, new Map(this.temporaryIgnored));
            if (
              !this.contextCurrent(activation, room, viewer) ||
              operation !== this.preferenceOperation
            )
              return;
            this.preferences = next;
            this.savePreferences(viewer);
            if (current() && next.sounds) this.playSound(550);
            this.render();
            if (lookChanged) {
              const applied = await room.setChatStyle(chosen);
              if (!this.contextCurrent(activation, room, viewer)) return;
              this.preferences = { ...this.preferences, look: applied };
              this.savePreferences(viewer);
              this.render();
            }
            if (current()) view.close();
          } finally {
            if (operation === this.preferenceOperation) this.preferenceBusy = false;
          }
        }),
      (error) => {
        if (!current()) return;
        view.error.textContent =
          error instanceof Error ? error.message : 'Unable to save preferences';
        view.error.hidden = false;
      },
      'btn-primary',
    );
    view.body.append(save, el('h3', 'Ignored people'));
    const ignored = [
      ...this.preferences.ignored,
      ...[...this.temporaryIgnored].map(([id, name]) => ({ id, name })),
    ];
    if (!ignored.length)
      view.body.append(el('p', 'No one is ignored. Use a person’s menu to ignore their messages.'));
    for (const person of ignored) {
      const row = el('div', undefined, 'community-row');
      const restore = asyncButton(
        'Unignore',
        () =>
          busy(restore, view.error, async () => {
            if (!current()) return;
            await this.toggleIgnore(person.id, person.name, false);
            if (current() && !this.isIgnored(person.id)) row.remove();
          }),
        (error) => {
          if (!current()) return;
          view.error.textContent =
            error instanceof Error ? error.message : 'Unable to restore messages';
          view.error.hidden = false;
        },
      );
      row.append(el('span', person.name), restore);
      view.body.append(row);
    }
    view.body.append(
      el(
        'p',
        this.accountToken(viewer)
          ? 'These preferences and your ignore list are saved to your account and follow you to other devices. Guest ignores last only for this room session.'
          : 'Preferences are saved in this browser. Guest ignores last only for this room session.',
        'setting-hint',
      ),
    );
  }

  /** Swatches and styles for the local look, with a live preview row. */
  private lookPicker(room: RoomClient): { nodes: HTMLElement[]; chosen: () => ChatStyle } {
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
      node.addEventListener('change', () => show());
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
      nodes: [
        el('h3', 'Your chat color'),
        el(
          'p',
          'Everyone in the room sees your name, messages and video tag in this color.',
          'setting-hint',
        ),
        colorGroup,
        styleGroup,
        preview,
      ],
      chosen,
    };
  }

  private resumeSoundFromGesture(enabled = this.preferences.sounds): void {
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

  private playSound(frequency: number): void {
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

/** Links stay whole; outside them, each occurrence of `mention` is marked, in any case. */
export function appendLinkedText(parent: HTMLElement, text: string, mention?: string): void {
  const pattern = /https?:\/\/[^\s<>]+/g;
  // Bidi controls, zero-width space and the BOM can make a link read as another.
  const hidden = /[\u200B\u202A-\u202E\u2066-\u2069\u2028\u2029\uFEFF]/;
  let start = 0;
  for (const match of text.matchAll(pattern)) {
    const index = match.index;
    if (hidden.test(match[0])) continue;
    appendMarkedText(parent, text.slice(start, index), mention);
    const link = el('a', match[0]);
    link.href = match[0];
    link.target = '_blank';
    link.rel = 'noopener noreferrer';
    parent.append(link);
    start = index + match[0].length;
  }
  appendMarkedText(parent, text.slice(start), mention);
}

function appendMarkedText(parent: HTMLElement, text: string, mention: string | undefined): void {
  if (!mention) {
    parent.append(document.createTextNode(text));
    return;
  }
  // Match on the original text: lowercasing can change a string's length.
  const pattern = new RegExp(mention.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'giu');
  let start = 0;
  for (const match of text.matchAll(pattern)) {
    parent.append(document.createTextNode(text.slice(start, match.index)));
    parent.append(el('mark', match[0], 'mention'));
    start = match.index + match[0].length;
  }
  parent.append(document.createTextNode(text.slice(start)));
}
