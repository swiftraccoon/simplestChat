import { createScope } from './disposable';
import type { RoomClient } from './room';
import type { ChatEntry, ChatStyle, ServerMessage, TimestampFormat } from './protocol';
import { ChatStore } from './chat/store';
import type { SocialChatOptions } from './chat/context';
import { ChatComposer } from './chat/composer';
import { ChatRenderer } from './chat/render';
import { ChatNotifications } from './chat/notifications';
import { ChatPreferencesController } from './chat/preferences';
import { AUTOMATIC_LOOK, excerpt } from './chat/format';
import { api, button, el } from './ui';
import type { SheetSection } from './settings-sheet';
import {
  notifyHistoryRemoval,
  notifyHistoryEdit,
  onHistoryEdit,
  notifyHistoryReaction,
  onHistoryReaction,
  onPrivateRead,
  notifyInboxChanged,
} from './chat-history';
export { CHAT_REACTIONS } from './chat-reactions';

/** Coordinates membership, ordered event ingestion and conversation read ownership. */
export class SocialChat {
  private readonly options: SocialChatOptions;

  private readonly events = createScope();
  private readonly composer: ChatComposer;
  private readonly renderer: ChatRenderer;
  private readonly notifications: ChatNotifications;
  private readonly privacy: ChatPreferencesController;
  private readonly stopHistoryEdit: () => void;
  private readonly store = new ChatStore(300, 256 * 1024);
  private readonly messages: HTMLDivElement;
  private readonly input: HTMLTextAreaElement;
  private readonly inputRow: HTMLDivElement;
  private readonly sendButton: HTMLButtonElement;
  private readonly accountPeers = new Set<string>();
  private readonly conversationStatus = el('span', '', 'conversation-status');
  private readonly closeButton = button('✕', () => {
    if (this.disposed) return;
    this.closePrivate();
    this.renderer.conversationButtons.get('public')?.node.focus();
  });
  private readonly savedRead = new Map<string, string>();
  private readController = new AbortController();
  private readTimer: ReturnType<typeof setTimeout> | null = null;
  private scope = '';
  private activeRoom: RoomClient | null = null;
  private viewerKey = '';
  private activation = 0;
  private membershipVersion = -1;
  private activationTask: Promise<void> | null = null;
  private activationSynced = false;
  private snapshotPublicIds: Set<string> | null = null;
  private readonly stopHistoryReaction: () => void;
  private readonly stopPrivateRead: () => void;
  private disposed = false;
  private seenAtBottom = true;
  // Where reading last resumed: the first message that had arrived unseen.
  private dividerKey: string | null = null;
  private resumeKey: string | null = null;
  private readonly divider = el('div', undefined, 'chat-divider');
  private readonly historyStart = el('div', undefined, 'chat-history-start');
  private readonly olderButton = button(
    'Earlier messages',
    () => {
      if (this.disposed) return;
      this.loadEarlier().catch(() => {});
    },
    'chat-earlier',
  );
  private readonly pinsButton = button(
    '',
    () => {
      if (this.disposed) return;
      this.pinsExpanded = !this.pinsExpanded;
      this.renderPins();
    },
    'chat-pins-toggle',
  );
  private readonly pinsList = el('div', undefined, 'chat-pinned-messages');
  private pins: ChatEntry[] = [];
  private pinsExpanded = false;
  private pinsVersion = 0;
  private historyCursor: string | null = null;
  private historyLoaded = false;
  private historyLoading = false;

  constructor(options: SocialChatOptions) {
    this.options = options;
    this.messages = options.elements.chatMessages;
    this.input = options.elements.chatInput;
    this.inputRow = options.elements.chatInputRow;
    this.sendButton = options.elements.chatSendBtn;

    const owner = this;
    this.privacy = new ChatPreferencesController({
      options,
      store: this.store,
      get activation() {
        return owner.activation;
      },
      get viewerKey() {
        return owner.viewerKey;
      },
      get activeRoom() {
        return owner.activeRoom;
      },
      get disposed() {
        return owner.disposed;
      },
      contextCurrent: (activation, room, viewer) => owner.contextCurrent(activation, room, viewer),
      render: (force) => owner.render(force),
      switchConversation: (id, name) => owner.switchConversation(id, name),
      startReply: (message) => owner.composer.startReply(message),
      notificationPermission: (wanted) => owner.notifications.notificationPermission(wanted),
      closeDesktopNotices: () => owner.notifications.closeDesktopNotices(),
      resumeSoundFromGesture: (enabled) => owner.notifications.resumeSoundFromGesture(enabled),
      playSound: (frequency) => owner.notifications.playSound(frequency),
    });
    this.notifications = new ChatNotifications({
      elements: options.elements,
      options,
      store: this.store,
      get preferences() {
        return owner.privacy.preferences;
      },
      get activation() {
        return owner.activation;
      },
      get activeRoom() {
        return owner.activeRoom;
      },
      get viewerKey() {
        return owner.viewerKey;
      },
      get disposed() {
        return owner.disposed;
      },
      contextCurrent: (activation, room, viewer) => owner.contextCurrent(activation, room, viewer),
      switchConversation: (id, name) => owner.switchConversation(id, name),
    });
    this.renderer = new ChatRenderer({
      elements: options.elements,
      options,
      store: this.store,
      get preferences() {
        return owner.privacy.preferences;
      },
      get activeRoom() {
        return owner.activeRoom;
      },
      get activation() {
        return owner.activation;
      },
      get viewerKey() {
        return owner.viewerKey;
      },
      get pins() {
        return owner.pins;
      },
      get replyingTo() {
        return owner.composer.replyingTo;
      },
      get seenAtBottom() {
        return owner.seenAtBottom;
      },
      accountPeers: this.accountPeers,
      contextCurrent: (activation, room, viewer) => owner.contextCurrent(activation, room, viewer),
      isIgnored: (id) => owner.privacy.isIgnored(id),
      pinMessage: (message, pinned) => owner.pinMessage(message, pinned),
      startReply: (message) => owner.composer.startReply(message),
      cancelReply: () => owner.composer.cancelReply(),
      render: (force) => owner.render(force),
      retry: (message) => owner.composer.retry(message),
      restoreDraft: (message) => owner.composer.restoreDraft(message),
      switchConversation: (id, name) => owner.switchConversation(id, name),
    });
    this.composer = new ChatComposer({
      elements: {
        input: this.input,
        inputRow: this.inputRow,
        sendButton: this.sendButton,
        panel: options.elements.chatPanel,
      },
      options,
      store: this.store,
      get activeRoom() {
        return owner.activeRoom;
      },
      get activation() {
        return owner.activation;
      },
      get viewerKey() {
        return owner.viewerKey;
      },
      get membershipVersion() {
        return owner.membershipVersion;
      },
      get seenAtBottom() {
        return owner.seenAtBottom;
      },
      messages: this.messages,
      contextCurrent: (activation, room, viewer) => owner.contextCurrent(activation, room, viewer),
      render: (force) => owner.render(force),
      switchConversation: (id, name) => owner.switchConversation(id, name),
      clearDivider: () => {
        owner.dividerKey = null;
        owner.resumeKey = null;
      },
    });

    const toolbar = el('div', undefined, 'conversation-toolbar');
    this.events.onDispose(() => {
      toolbar.remove();
      this.conversationStatus.remove();
      this.historyStart.remove();
    });
    this.renderer.conversations.setAttribute('aria-label', 'Conversations');
    this.closeButton.setAttribute('aria-label', 'Close PM');
    this.closeButton.title = 'Close PM';
    const optionsButton = button(
      '⋯',
      () => {
        if (!this.disposed) this.options.openSettings?.('chat-look', optionsButton);
      },
      'chat-options-button',
    );
    optionsButton.setAttribute('aria-label', 'Chat options');
    optionsButton.title = 'Chat options';
    toolbar.append(this.renderer.conversations, this.closeButton, this.pinsButton, optionsButton);
    this.pinsButton.hidden = true;
    this.historyStart.append(this.pinsList, this.olderButton);
    this.stopHistoryReaction = onHistoryReaction((id, reactions) => {
      if (this.store.setReactions(id, reactions)) this.render();
    });
    this.stopPrivateRead = onPrivateRead((account, peer, cursor) => {
      const room = this.activeRoom;
      if (
        !room ||
        !this.options.getToken?.() ||
        account !== this.store.localId ||
        cursor.accountConversation !== true ||
        !this.contextCurrent(this.activation, room, account)
      )
        return;
      if (this.store.markReadThrough(peer, cursor)) this.render();
    });
    this.stopHistoryEdit = onHistoryEdit((message) => {
      if (this.store.editMessage(message)) this.render();
      if (this.composer.replyingTo?.messageId === message.messageId) {
        const quote = this.composer.replyBar.querySelector('.reply-bar-excerpt');
        if (quote) quote.textContent = excerpt(this.composer.replyingTo.content);
      }
      this.pinsVersion++;
      this.pins = this.pins.map((pin) =>
        pin.messageId === message.messageId && message.revision > pin.revision ? message : pin,
      );
      this.renderPins();
    });
    options.elements.chatPanel.prepend(toolbar, this.conversationStatus);
    this.messages.setAttribute('role', 'log');
    this.messages.setAttribute('aria-label', 'Conversation messages');
    this.divider.setAttribute('role', 'separator');
    this.divider.append(el('span', 'New messages'));
    this.messages.addEventListener(
      'scroll',
      () => {
        const wasAtBottom = this.seenAtBottom;
        this.seenAtBottom =
          this.messages.scrollHeight - this.messages.scrollTop - this.messages.clientHeight < 48;
        if (this.seenAtBottom && this.isVisible()) this.markActiveRead(!wasAtBottom);
        this.renderer.updateBadges();
      },
      { signal: this.events.signal },
    );
    options.elements.scrollBottomBtn.addEventListener(
      'click',
      () => {
        this.messages.scrollTop = this.messages.scrollHeight;
        this.seenAtBottom = true;
        if (this.isVisible()) this.markActiveRead(true);
        this.renderer.updateBadges();
      },
      { signal: this.events.signal },
    );
    document.addEventListener(
      'visibilitychange',
      () => {
        if (!document.hidden) this.render();
      },
      { signal: this.events.signal },
    );
    options.elements.chatTab.addEventListener(
      'click',
      () =>
        queueMicrotask(() => {
          if (!this.disposed) this.render();
        }),
      { signal: this.events.signal },
    );
    this.privacy.subscribe();
    this.render();
  }

  async activate(): Promise<void> {
    if (this.disposed) return;
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
            : this.composer.composition.draft('public')
          : '';
      this.reset();
      this.scope = room.currentRoomId ?? '';
      this.activeRoom = room;
      this.viewerKey = viewerKey;
      this.store.localId = room.localParticipantId;
      this.composer.composition.save('public', publicDraft);
      this.input.value = publicDraft;
      this.privacy.loadPreferences();
    }
    if (this.membershipVersion !== room.membershipVersion) {
      this.prepareSnapshotRecovery();
      this.composer.retireDeliveryAttempts();
      this.activation++;
      this.privacy.preferenceOperation++;
      this.privacy.preferenceBusy = false;
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
      const token = this.privacy.accountToken(viewerKey);
      if (token) {
        try {
          await this.privacy.loadAccountPreferences(current);
        } catch {
          if (current())
            this.options.notify(
              'Saved chat preferences could not be loaded; using this browser’s copy',
            );
        }
        if (!current()) return;
      }
      try {
        await this.privacy.pushPreferences(
          room,
          this.privacy.preferences,
          this.privacy.temporaryIgnored,
        );
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

  /** Capture once before recovery so a delayed snapshot never prunes newer live rows. */
  prepareSnapshotRecovery(): void {
    if (this.snapshotPublicIds !== null) return;
    this.snapshotPublicIds = new Set(
      this.store.messages
        .filter(
          (message) => message.status === 'sent' && !message.recipientId && !!message.participantId,
        )
        .map((message) => message.messageId),
    );
  }

  reset(): void {
    this.readController.abort();
    this.readController = new AbortController();
    if (this.readTimer !== null) clearTimeout(this.readTimer);
    this.readTimer = null;
    this.savedRead.clear();
    this.activation++;
    this.privacy.reset();
    this.activationTask = null;
    this.activationSynced = false;
    this.snapshotPublicIds = null;
    this.membershipVersion = -1;
    this.notifications.reset();
    this.composer.reset();
    this.store.reset();
    this.accountPeers.clear();
    this.pins = [];
    this.pinsVersion++;
    this.pinsExpanded = false;
    this.historyCursor = null;
    this.historyLoaded = false;
    this.historyLoading = false;
    this.renderer.reset();
    this.dividerKey = null;
    this.resumeKey = null;
    this.divider.remove();
    this.scope = '';
    this.activeRoom = null;
    this.viewerKey = '';
    this.seenAtBottom = true;
    this.render();
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.events.dispose();
    this.privacy.dispose();
    this.stopHistoryEdit();
    this.stopHistoryReaction();
    this.stopPrivateRead();
    this.reset();
    this.composer.dispose();
    this.notifications.dispose();
  }

  handleEvent(message: ServerMessage): void {
    if (this.disposed) return;
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
    else if (message.type === 'privateMessageReceived') {
      this.receive(message.message);
      notifyInboxChanged(message.message);
    } else if (message.type === 'messageAck') this.receive(message.message);
    else if (message.type === 'socialError' && message.clientMessageId) {
      // The reply has the original message ID, not a retry-attempt ID. Once
      // unconfirmed, a delayed rejection cannot prove the original was unsent.
      const unknown =
        this.composer.retrying.has(message.clientMessageId) ||
        this.store.messages.some(
          (item) =>
            item.participantId === this.store.localId &&
            item.clientMessageId === message.clientMessageId &&
            item.status === 'unknown',
        );
      this.composer.finishSend(message.clientMessageId, unknown ? 'unknown' : 'denied');
      this.composer.clearPending(message.clientMessageId);
      this.store.fail(
        message.clientMessageId,
        unknown ? `Delivery not confirmed. ${message.message}` : message.message,
        unknown,
      );
      this.render();
    } else if (message.type === 'socialResponse' && message.action === 'getRoomSnapshot') {
      const snapshot = message.data;
      if (snapshot.chatSessionId !== undefined) {
        if (
          this.composer.chatSessionId !== null &&
          this.composer.chatSessionId !== snapshot.chatSessionId
        )
          this.composer.retireDeliveryAttempts();
        this.composer.chatSessionId = snapshot.chatSessionId;
      }
      // Ingest synchronously so replay retains the same ordering, privacy and
      // acknowledgement semantics, then reconcile the bounded view just once.
      if (this.snapshotPublicIds !== null) {
        const discarded = this.store.prunePublicReplay(
          this.snapshotPublicIds,
          new Set(
            snapshot.messages.filter((entry) => !entry.recipientId).map((entry) => entry.messageId),
          ),
        );
        this.snapshotPublicIds = null;
        if (this.composer.replyingTo && discarded.has(this.composer.replyingTo.messageId))
          this.composer.cancelReply();
      }
      for (const entry of snapshot.messages) this.ingest(entry, true);
      this.refreshPins();
      this.render();
    } else if (message.type === 'messageRetryResult') {
      this.composer.finishSend(message.clientMessageId, 'unknown');
      this.composer.clearPending(message.clientMessageId);
      const explanations: Record<typeof message.reason, string> = {
        session_changed: 'The room session changed.',
        receipt_expired: 'The server no longer retains this confirmation.',
        sequence_superseded:
          'A later message was accepted and this earlier attempt cannot be verified.',
        capacity: 'The room could not retain this confirmation. Try again within the retry window.',
        conflict: 'The original message identity could not be verified.',
        recipient_unconfirmed: 'The original private recipient session could not be verified.',
        storage_unconfirmed:
          'The database did not confirm whether this message was saved. Check saved history after reconnecting before sending it again.',
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
    } else if (message.type === 'chatMessageEdited') {
      notifyHistoryEdit(message.message);
    } else if (message.type === 'pinnedMessagesChanged') {
      this.pinsVersion++;
      this.pins = message.messages;
      this.render();
    } else if (message.type === 'chatMessageRemoved') {
      notifyHistoryRemoval(message.messageId, message.removedAt);
      this.store.removeMessage(message.messageId, message.removedAt);
      this.pinsVersion++;
      this.pins = this.pins.filter((pin) => pin.messageId !== message.messageId);
      if (this.composer.replyingTo?.messageId === message.messageId) this.composer.cancelReply();
      this.render();
    } else if (message.type === 'messageReactions') {
      notifyHistoryReaction(message.messageId, message.reactions);
    } else if (message.type === 'nicknameChanged') {
      if (this.store.names.has(message.participantId))
        this.store.names.set(message.participantId, message.nickname);
      this.render();
    } else if (message.type === 'participantTyping') {
      this.composer.noteRemoteTyping(message.participantId, message.targetParticipantId ?? null);
    }
  }

  participantsChanged(): void {
    if (this.disposed) return;
    this.render();
  }

  openPrivate(id: string, name: string): void {
    if (this.disposed) return;
    if (id === this.store.localId) return;
    if (this.openAccountConversation(id, name)) return;
    if (this.store.names.size >= 100 && !this.store.names.has(id)) {
      this.options.notify('Close an existing conversation first');
      return;
    }
    this.switchConversation(id, name);
    this.options.elements.chatTab.click();
    this.render();
    this.input.focus();
  }

  private openAccountConversation(id: string, name?: string): boolean {
    if (
      !this.options.getToken?.() ||
      !this.options.openAccountConversation ||
      (!this.accountPeers.has(id) &&
        !this.options.getRoom()?.getParticipants().get(id)?.authenticated)
    )
      return false;
    this.options.openAccountConversation(id, name ?? this.store.names.get(id) ?? 'Account');
    return true;
  }

  system(text: string): void {
    if (this.disposed) return;
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
        revision: 0,
        chatStyle: AUTOMATIC_LOOK,
      },
      true,
    );
    this.render();
  }

  private receive(message: ChatEntry, replay = false): void {
    if (this.ingest(message, replay)) this.render();
  }

  private ingest(message: ChatEntry, replay: boolean): boolean {
    if (!this.store.localId || this.privacy.isIgnored(message.participantId)) return false;
    if (
      message.accountConversation &&
      message.recipientId &&
      this.accountPeers.size < 100 &&
      (message.participantId === this.store.localId || message.recipientId === this.store.localId)
    ) {
      const peer =
        message.participantId === this.store.localId ? message.recipientId : message.participantId;
      this.accountPeers.add(peer);
    }
    if (
      message.recipientId &&
      message.participantId !== this.store.localId &&
      !this.privacy.preferences.allowPrivateMessages
    )
      return false;
    const added = this.store.receive(message, replay);
    if (message.participantId === this.store.localId) {
      this.composer.finishSend(message.clientMessageId, 'ok');
      this.composer.clearPending(message.clientMessageId);
    }
    if (added && message.participantId !== this.store.localId) {
      if (
        this.store.conversation(message) === this.store.active &&
        (!this.seenAtBottom || !this.isVisible())
      )
        this.store.markUnread(message);
      const retained = this.store.messages.find((entry) => entry.messageId === message.messageId);
      if (
        !replay &&
        retained &&
        !retained.removedAt &&
        this.notifications.allowsNotification(retained)
      ) {
        if (this.privacy.preferences.sounds)
          this.notifications.playSound(retained.recipientId ? 700 : 440);
        this.notifications.notifyDesktop(retained);
      }
    }
    return true;
  }

  private render(forceScroll = false): void {
    if (this.disposed) return;
    const room = this.options.getRoom();
    const pendingIds = new Set(
      this.store.messages
        .filter((message) => message.status === 'pending')
        .map((message) => message.clientMessageId),
    );
    for (const id of this.composer.pending.keys())
      if (!pendingIds.has(id)) this.composer.clearPending(id);
    const scrollTop = this.messages.scrollTop;
    const atBottom = this.seenAtBottom || forceScroll;
    const privateChat = this.store.active !== 'public';
    this.closeButton.hidden = !privateChat;
    // Public chat needs no permanent banner; its mention hint lives in the composer.
    this.conversationStatus.hidden = !privateChat;
    this.composer.renderTyping();
    this.conversationStatus.textContent = '';
    if (privateChat) {
      const peer = room?.getParticipants().get(this.store.active);
      if (!peer) this.conversationStatus.textContent = 'Private · this person is offline';
      else if (this.options.getToken?.() && peer.authenticated)
        this.conversationStatus.textContent = 'Private · saved for 90 days in Messages';
      else
        this.conversationStatus.textContent = 'Private · available only during this room session';
    }
    const disabled =
      !room?.localParticipantId ||
      !room.connected ||
      !room.canChat ||
      (privateChat &&
        (!room.getParticipants().has(this.store.active) ||
          this.privacy.isIgnored(this.store.active)));
    this.input.disabled = disabled;
    this.sendButton.disabled = disabled;
    this.input.placeholder = disabled
      ? privateChat
        ? 'This person is offline or chat is restricted'
        : 'Chat is currently restricted'
      : privateChat
        ? 'Write a private message…'
        : 'Message · @mention';
    this.messages.classList.toggle('large-chat-text', this.privacy.preferences.largeText);
    this.messages.classList.toggle(
      'timestamps-always',
      this.privacy.preferences.timestamps !== 'hover',
    );
    const visible = this.store.messages.filter(
      (message) =>
        this.store.conversation(message) === this.store.active &&
        !this.privacy.isIgnored(message.participantId),
    );
    const keys = new Set(visible.map((message) => this.renderer.rowKey(message)));
    for (const [key, row] of this.renderer.rows) {
      if (!keys.has(key)) {
        row.dispose();
        row.node.remove();
        this.renderer.rows.delete(key);
      }
    }
    let previousSender: string | undefined;
    let previousTime = 0;
    if (this.historyStart.parentNode !== this.messages)
      this.messages.insertBefore(this.historyStart, this.messages.firstChild);
    this.olderButton.hidden =
      privateChat ||
      !room?.roomSettings?.historyRetentionDays ||
      (this.historyLoaded && !this.historyCursor);
    this.olderButton.disabled = this.historyLoading;
    this.renderPins();
    let position = this.historyStart.nextSibling;
    for (const message of visible) {
      const node = this.renderer.messageRow(message, room?.nickname);
      const time = Date.parse(message.sentAt);
      if (position === this.divider) position = position.nextSibling;
      // The divider starts a new block even within one sender's run.
      const grouped =
        previousSender === message.participantId &&
        time - previousTime < 120_000 &&
        this.renderer.rowKey(message) !== this.dividerKey;
      if (node.classList.contains('grouped') !== grouped) node.classList.toggle('grouped', grouped);
      // Keep unchanged rows in place. insertBefore also handles the uncommon
      // chronological move when an acknowledgement updates a pending timestamp.
      if (node === position) position = position.nextSibling;
      else this.messages.insertBefore(node, position);
      previousSender = message.participantId;
      previousTime = time;
    }
    this.placeDivider();
    this.composer.resizeInput();
    this.messages.scrollTop = atBottom ? this.messages.scrollHeight : scrollTop;
    if (forceScroll) {
      this.seenAtBottom = true;
      this.resumeKey = null;
    }
    if (this.resumeKey && this.isVisible()) {
      const row = this.renderer.rows.get(this.resumeKey)?.node;
      row?.scrollIntoView?.({ block: 'start' });
      this.seenAtBottom =
        this.messages.scrollHeight - this.messages.scrollTop - this.messages.clientHeight < 48;
      this.resumeKey = null;
    }
    if (this.seenAtBottom && this.isVisible() && this.markActiveRead()) this.placeDivider();
    this.renderer.updateBadges();
  }

  /**
   * Reading resumes: remember where the unseen messages began, then count them read.
   * Returns whether that moved the divider.
   */
  private markActiveRead(placeNow = false): boolean {
    const first = this.store.firstUnread(this.store.active);
    const key = first ? this.renderer.rowKey(first) : this.dividerKey;
    this.store.markRead(this.store.active);
    this.scheduleSavedRead();
    if (key === this.dividerKey) return false;
    this.dividerKey = key;
    if (placeNow) this.placeDivider();
    return true;
  }

  /** Reading the live room also advances the account's saved conversation cursor. */
  private scheduleSavedRead(): void {
    if (this.readTimer !== null || !this.options.getToken?.()) return;
    const room = this.options.getRoom();
    if (!room || !this.isVisible() || !this.seenAtBottom) return;
    const conversation = this.store.active;
    if (
      conversation === 'public'
        ? !room.roomSettings?.historyRetentionDays
        : !room.getParticipants().get(conversation)?.authenticated
    )
      return;
    const activation = this.activation;
    const viewer = this.viewerKey;
    this.readTimer = setTimeout(() => {
      this.readTimer = null;
      if (!this.contextCurrent(activation, room, viewer) || !this.isVisible() || !this.seenAtBottom)
        return;
      if (this.store.active !== conversation) {
        this.scheduleSavedRead();
        return;
      }
      const token = this.options.getToken?.();
      if (!token) return;
      const messages = this.store.messages.filter(
        (entry) => entry.status === 'sent' && this.store.conversation(entry) === conversation,
      );
      const latest = messages[messages.length - 1];
      if (!latest || this.savedRead.get(conversation) === latest.messageId) return;
      const id = latest.messageId;
      this.savedRead.set(conversation, id);
      if (this.savedRead.size > 100) this.savedRead.delete(this.savedRead.keys().next().value!);
      const request =
        conversation === 'public'
          ? room.requestSocial('markChatRead', { messageId: id })
          : api.readPrivateMessages(token, conversation, id, this.readController.signal);
      request
        .then(() => {
          if (this.contextCurrent(activation, room, viewer)) notifyInboxChanged();
        })
        .catch(() => {
          if (
            this.contextCurrent(activation, room, viewer) &&
            this.savedRead.get(conversation) === id
          )
            this.savedRead.delete(conversation);
        });
    }, 500);
  }

  /** The divider sits above the first message that arrived unseen, while it is shown. */
  private placeDivider(): void {
    const row = this.dividerKey ? this.renderer.rows.get(this.dividerKey)?.node : undefined;
    if (!row?.isConnected) {
      this.divider.remove();
      return;
    }
    if (this.divider.nextSibling !== row) this.messages.insertBefore(this.divider, row);
    row.classList.toggle('grouped', false);
  }

  private pinMessage(message: ChatEntry, pinned: boolean): void {
    const room = this.options.getRoom();
    if (!room) return;
    const activation = this.activation;
    const viewer = this.viewerKey;
    const version = ++this.pinsVersion;
    room
      .requestSocial('setPinnedMessage', { messageId: message.messageId, pinned })
      .then((result) => {
        if (!this.contextCurrent(activation, room, viewer)) return;
        if (version !== this.pinsVersion) return;
        this.pins = result.messages;
        this.render();
      })
      .catch((error: unknown) => {
        if (this.contextCurrent(activation, room, viewer))
          this.options.notify(
            error instanceof Error ? error.message : 'Could not update pinned messages',
          );
      });
  }

  /** Refresh expiry-sensitive saved state when the app returns or while it is visible. */
  refreshSavedChat(): void {
    if (this.disposed) return;
    if (document.hidden || !this.options.getRoom()?.localParticipantId) return;
    this.refreshPins();
  }

  private refreshPins(): void {
    const room = this.options.getRoom();
    if (!room?.localParticipantId) return;
    const activation = this.activation;
    const viewer = this.viewerKey;
    const version = this.pinsVersion;
    room
      .requestSocial('getPinnedMessages')
      .then((result) => {
        if (!this.contextCurrent(activation, room, viewer)) return;
        if (version !== this.pinsVersion) return;
        this.pins = result.messages;
        this.render();
      })
      .catch(() => {
        /* Reconnection retries the bounded snapshot. */
      });
  }

  private renderPins(): void {
    this.pinsButton.hidden = this.store.active !== 'public' || !this.pins.length;
    this.pinsButton.textContent = `Pinned (${this.pins.length})`;
    this.pinsButton.setAttribute('aria-expanded', String(this.pinsExpanded));
    this.pinsList.hidden = this.pinsButton.hidden || !this.pinsExpanded;
    this.pinsList.replaceChildren();
    if (this.pinsList.hidden) return;
    for (const pin of this.pins) {
      const entry = el('div', undefined, 'chat-pinned-message');
      entry.append(
        button(
          `${pin.participantName}: ${pin.content || pin.attachments?.map((attachment) => attachment.name).join(', ') || 'Message'}`,
          () => this.renderer.showMessage(pin.messageId),
          'chat-pinned-link',
        ),
      );
      if (['moderator', 'admin', 'owner'].includes(this.options.getRoom()?.role ?? ''))
        entry.append(button('Unpin', () => this.pinMessage(pin, false), 'msg-action'));
      this.pinsList.append(entry);
    }
  }

  private async loadEarlier(): Promise<void> {
    const room = this.options.getRoom();
    if (!room || this.store.active !== 'public' || this.historyLoading) return;
    const activation = this.activation;
    const viewer = this.viewerKey;
    this.historyLoading = true;
    this.olderButton.disabled = true;
    try {
      let page = await room.requestSocial('getChatHistory', {
        limit: 50,
        ...(this.historyCursor && { before: this.historyCursor }),
      });
      const retained = new Set(this.store.messages.map((entry) => entry.messageId));
      // A reconnect snapshot already contains the newest public rows. Skip overlapping
      // saved pages so the first click actually reveals earlier text.
      for (
        let count = 0;
        count < 6 &&
        page.nextCursor &&
        page.messages.every((entry) => retained.has(entry.messageId));
        count++
      ) {
        if (!this.contextCurrent(activation, room, viewer)) return;
        page = await room.requestSocial('getChatHistory', { limit: 50, before: page.nextCursor });
      }
      if (!this.contextCurrent(activation, room, viewer) || this.store.active !== 'public') return;
      const top = this.messages.scrollTop;
      const height = this.messages.scrollHeight;
      for (const entry of page.messages) this.store.receive(entry, true);
      const missing = page.messages.find(
        (entry) =>
          !retained.has(entry.messageId) &&
          !this.store.messages.some((item) => item.messageId === entry.messageId),
      );
      if (missing) {
        // The live window is bounded by both message count and characters. Keep its
        // cursor unchanged and open saved context when older text cannot fit.
        this.render();
        this.renderer.showMessage(missing.messageId);
        return;
      }
      this.historyCursor = page.nextCursor;
      this.historyLoaded = true;
      this.seenAtBottom = false;
      this.render();
      this.messages.scrollTop = top + this.messages.scrollHeight - height;
    } catch (error) {
      if (this.contextCurrent(activation, room, viewer))
        this.options.notify(
          error instanceof Error ? error.message : 'Earlier messages could not be loaded',
        );
    } finally {
      if (this.contextCurrent(activation, room, viewer)) {
        this.historyLoading = false;
        this.olderButton.disabled = false;
      }
    }
  }

  private switchConversation(id: string, name?: string): void {
    if (this.disposed) return;
    if (id !== 'public' && this.openAccountConversation(id, name)) return;
    if (id !== this.store.active) this.composer.attachments.reset();
    this.composer.cancelReply();
    this.composer.composition.save(this.store.active, this.input.value);
    const first = this.store.firstUnread(id);
    this.dividerKey = first ? this.renderer.rowKey(first) : null;
    this.resumeKey = this.dividerKey;
    this.seenAtBottom = !first;
    this.store.open(id, name);
    this.input.value = this.composer.composition.draft(id);
    this.render(!first);
  }

  private closePrivate(): void {
    const id = this.store.active;
    if (id === 'public') return;
    this.composer.attachments.reset();
    for (const message of this.store.messages) {
      if (this.store.conversation(message) === id)
        this.composer.clearPending(message.clientMessageId);
    }
    this.store.close(id);
    this.composer.composition.close(id);
    this.input.value = this.composer.composition.draft('public');
    this.render(true);
  }

  private isVisible(): boolean {
    return (
      !document.hidden &&
      !document.querySelector('dialog[open]') &&
      this.options.elements.roomScreen.hidden === false &&
      this.options.elements.chatPanel.classList.contains('active')
    );
  }

  private contextCurrent(activation: number, room: RoomClient, viewer: string): boolean {
    return (
      !this.disposed &&
      activation === this.activation &&
      this.options.getRoom() === room &&
      viewer === this.viewerKey &&
      viewer === this.options.getViewerKey() &&
      this.scope === room.currentRoomId &&
      this.store.localId === room.localParticipantId &&
      this.membershipVersion === room.membershipVersion
    );
  }

  isIgnored(id: string): boolean {
    return this.privacy.isIgnored(id);
  }

  toggleIgnore(id: string, name: string, authenticated: boolean): Promise<void> {
    return this.privacy.toggleIgnore(id, name, authenticated);
  }

  setTimestampFormat(format: TimestampFormat): void {
    this.privacy.setTimestampFormat(format);
  }

  savedLook(): ChatStyle | null {
    return this.privacy.savedLook();
  }

  sections(): SheetSection[] {
    return this.privacy.sections();
  }
}

export { appendLinkedText } from './chat-message-ui';
