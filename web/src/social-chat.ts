import type { RoomClient } from './room';
import type { ChatEntry, RoomSnapshot, ServerMessage } from './protocol';
import { ChatStore, ConversationInputs } from './chat-store';
import { button, busy, el, field, modal } from './ui';

type Preferences = { allowPrivateMessages: boolean; sounds: boolean; largeText: boolean; ignored: { id: string; name: string }[] };
type Options = {
  getRoom: () => RoomClient | null;
  getViewerKey: () => string;
  notify: (message: string) => void;
  participantAction: (id: string, name: string, x: number, y: number) => void;
};

export class SocialChat {
  private readonly store = new ChatStore(300, 256 * 1024);
  private readonly composition = new ConversationInputs();
  private readonly messages = document.getElementById('chat-messages')!;
  private readonly input = document.getElementById('chat-input') as HTMLInputElement;
  private readonly sendButton = document.getElementById('chat-send-btn') as HTMLButtonElement;
  private readonly select = el('select');
  private readonly conversationStatus = el('span', '', 'conversation-status');
  private readonly closeButton = button('Close PM', () => this.closePrivate());
  private readonly emojiPanel = el('div', undefined, 'emoji-panel');
  private readonly pending = new Map<string, ReturnType<typeof setTimeout>>();
  private preferences: Preferences = { allowPrivateMessages: true, sounds: false, largeText: false, ignored: [] };
  private temporaryIgnored = new Map<string, string>();
  private scope = '';
  private viewerKey = '';
  private activation = 0;
  private membershipVersion = -1;
  private activationTask: Promise<void> | null = null;
  private activationSynced = false;
  private preferenceOperation = 0;
  private preferenceBusy = false;
  private preferencesDialog: ReturnType<typeof modal> | null = null;
  private audio: AudioContext | null = null;
  private seenAtBottom = true;

  constructor(private readonly options: Options) {
    const toolbar = el('div', undefined, 'conversation-toolbar');
    this.select.setAttribute('aria-label', 'Conversation');
    this.select.addEventListener('change', () => this.switchConversation(this.select.value));
    toolbar.append(this.select, this.closeButton, button('Chat options', () => this.openPreferences()));
    document.getElementById('chat-panel')!.prepend(toolbar, this.conversationStatus);
    const emojiButton = button('☺', () => { this.emojiPanel.hidden = !this.emojiPanel.hidden; }, 'emoji-toggle');
    emojiButton.title = 'Choose emoji';
    emojiButton.setAttribute('aria-label', 'Choose emoji');
    for (const emoji of ['😀', '😂', '😊', '❤️', '👍', '👋', '🎉', '🔥', '🤔', '😢', '🙌', '💯']) {
      this.emojiPanel.append(button(emoji, () => {
        this.insertText(emoji, this.input.selectionStart ?? this.input.value.length,
          this.input.selectionEnd ?? this.input.value.length);
        this.emojiPanel.hidden = true;
        this.input.focus();
      }, 'emoji-choice'));
    }
    this.emojiPanel.hidden = true;
    document.getElementById('chat-input-row')!.prepend(emojiButton);
    document.getElementById('chat-input-row')!.before(this.emojiPanel);
    this.input.maxLength = 2000;
    this.input.addEventListener('keydown', event => this.onKey(event));
    this.input.addEventListener('input', () => this.composition.save(this.store.active, this.input.value));
    this.sendButton.addEventListener('click', () => this.send());
    this.messages.setAttribute('role', 'log');
    this.messages.setAttribute('aria-label', 'Conversation messages');
    this.messages.addEventListener('scroll', () => {
      this.seenAtBottom = this.messages.scrollHeight - this.messages.scrollTop - this.messages.clientHeight < 48;
      if (this.seenAtBottom && this.isVisible()) this.store.markRead(this.store.active);
      this.updateBadges();
    });
    document.getElementById('scroll-bottom-btn')!.addEventListener('click', () => {
      this.messages.scrollTop = this.messages.scrollHeight;
      this.seenAtBottom = true;
      if (this.isVisible()) this.store.markRead(this.store.active);
      this.updateBadges();
    });
    document.addEventListener('visibilitychange', () => {
      if (!document.hidden) this.render();
    });
    document.querySelector<HTMLButtonElement>('[data-tab="chat"]')?.addEventListener('click', () => queueMicrotask(() => this.render()));
    document.addEventListener('pointerdown', () => this.resumeSoundFromGesture());
    document.addEventListener('keydown', () => this.resumeSoundFromGesture());
    this.render();
  }

  async activate(): Promise<void> {
    const room = this.options.getRoom();
    if (!room?.localParticipantId) return;
    const viewerKey = this.options.getViewerKey();
    if (this.store.localId !== room.localParticipantId || this.scope !== room.currentRoomId || this.viewerKey !== viewerKey) {
      this.reset();
      this.scope = room.currentRoomId ?? '';
      this.viewerKey = viewerKey;
      this.store.localId = room.localParticipantId;
      this.loadPreferences();
    }
    if (this.membershipVersion !== room.membershipVersion) {
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
      try { await this.pushPreferences(room, this.preferences, this.temporaryIgnored); }
      catch (error) {
        success = false;
        if (current()) this.options.notify(error instanceof Error ? error.message : 'Chat preferences could not be applied');
      }
      if (!current()) return;
      // Replay still recovers joining text if a preference update failed.
      try { await room.requestSocial('getRoomSnapshot'); }
      catch (error) {
        success = false;
        if (current()) this.options.notify(error instanceof Error ? error.message : 'Chat history could not be refreshed');
      }
      if (current()) { this.activationSynced = success; this.render(); }
    })();
    this.activationTask = task;
    try { await task; }
    finally { if (this.activationTask === task) this.activationTask = null; }
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
    this.pending.clear();
    this.store.reset();
    this.composition.reset();
    this.temporaryIgnored.clear();
    this.scope = '';
    this.viewerKey = '';
    this.preferences = { allowPrivateMessages: true, sounds: false, largeText: false, ignored: [] };
    this.seenAtBottom = true;
    this.emojiPanel.hidden = true;
    this.input.value = '';
    this.audio?.close().catch(() => {});
    this.audio = null;
    this.render();
  }

  handleEvent(message: ServerMessage): void {
    const room = this.options.getRoom();
    if (room?.localParticipantId && (this.store.localId !== room.localParticipantId
      || this.scope !== room.currentRoomId || this.viewerKey !== this.options.getViewerKey()
      || this.membershipVersion !== room.membershipVersion)) void this.activate();
    if (message.type === 'chatReceived') this.receive(message);
    else if (message.type === 'privateMessageReceived') this.receive(message.message);
    else if (message.type === 'messageAck') this.receive(message.message);
    else if (message.type === 'socialError' && message.clientMessageId) {
      this.clearPending(message.clientMessageId);
      this.store.fail(message.clientMessageId, message.message);
      this.render();
    } else if (message.type === 'socialResponse' && message.action === 'getRoomSnapshot') {
      const snapshot = message.data as unknown as RoomSnapshot;
      for (const entry of snapshot.messages) this.receive(entry, true);
      this.render();
    } else if (message.type === 'nicknameChanged') {
      if (this.store.names.has(message.participantId)) this.store.names.set(message.participantId, message.nickname);
      this.render();
    }
  }

  participantsChanged(): void { this.render(); }

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
    return this.preferences.ignored.some(entry => entry.id === id) || this.temporaryIgnored.has(id);
  }

  async toggleIgnore(id: string, name: string, authenticated: boolean): Promise<void> {
    const room = this.options.getRoom();
    if (!room?.localParticipantId) throw new Error('Join a room first');
    if (id === room.localParticipantId) throw new Error('You cannot ignore yourself');
    const activation = this.activation, viewer = this.viewerKey;
    const next = structuredClone(this.preferences);
    const guests = new Map(this.temporaryIgnored);
    if (this.isIgnored(id)) {
      next.ignored = next.ignored.filter(entry => entry.id !== id);
      guests.delete(id);
    } else {
      if (next.ignored.length + guests.size >= 100) throw new Error('Ignore list is full (100 people)');
      if (authenticated) next.ignored.push({ id, name: name.slice(0, 128) });
      else guests.set(id, name.slice(0, 128));
    }
    const operation = this.beginPreferenceUpdate();
    try {
      await this.pushPreferences(room, next, guests);
      if (!this.contextCurrent(activation, room, viewer) || operation !== this.preferenceOperation) return;
      this.preferences = next;
      this.temporaryIgnored = guests;
      this.savePreferences(viewer);
      this.render();
    } finally { if (operation === this.preferenceOperation) this.preferenceBusy = false; }
  }

  system(text: string): void {
    if (!this.store.localId) return;
    const id = crypto.randomUUID();
    this.store.receive({ messageId: id, clientMessageId: id, participantId: '', participantName: '', content: text, sentAt: new Date().toISOString() }, true);
    this.render();
  }

  private receive(message: ChatEntry, replay = false): void {
    if (!this.store.localId || this.isIgnored(message.participantId)) return;
    if (message.recipientId && message.participantId !== this.store.localId && !this.preferences.allowPrivateMessages) return;
    const added = this.store.receive(message, replay);
    if (message.participantId === this.store.localId) this.clearPending(message.clientMessageId);
    if (added && message.participantId !== this.store.localId) {
      if (this.store.conversation(message) === this.store.active && (!this.seenAtBottom || !this.isVisible())) this.store.markUnread(message);
      if (!replay && this.preferences.sounds) this.playSound(message.recipientId ? 700 : 440);
    }
    this.render();
  }

  private send(): void {
    const room = this.options.getRoom();
    const content = this.input.value.trim();
    if (!room?.localParticipantId || !room.connected || !content || this.input.disabled) return;
    if (new TextEncoder().encode(content).length > 4096) { this.options.notify('Message is too long (maximum 4096 bytes)'); return; }
    if (this.pending.size >= 100) { this.options.notify('Please wait for pending messages'); return; }
    const id = crypto.randomUUID();
    const recipientId = this.store.active === 'public' ? undefined : this.store.active;
    const retained = this.store.pending({ messageId: `pending:${id}`, clientMessageId: id, participantId: room.localParticipantId,
      participantName: room.nickname, recipientId, recipientName: recipientId ? this.store.names.get(recipientId) : undefined,
      content, sentAt: new Date().toISOString() });
    if (!retained) { this.options.notify('This message could not be added to the conversation'); return; }
    this.pending.set(id, setTimeout(() => {
      this.pending.delete(id);
      this.store.fail(id, 'Delivery not confirmed. Reconnect to check before sending again.');
      this.render();
    }, 12_000));
    try {
      if (recipientId) room.sendPrivate(recipientId, content, id);
      else room.sendChat(content, id);
      this.composition.sent(this.store.active, content);
      this.input.value = '';
    } catch (error) {
      this.clearPending(id);
      this.store.fail(id, error instanceof Error ? error.message : 'Send failed');
    }
    this.render(true);
  }

  private clearPending(id: string): void {
    const timer = this.pending.get(id);
    if (timer !== undefined) clearTimeout(timer);
    this.pending.delete(id);
  }

  private render(forceScroll = false): void {
    const room = this.options.getRoom();
    const pendingIds = new Set(this.store.messages.filter(message => message.status === 'pending').map(message => message.clientMessageId));
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
      const option = el('option', `${name}${online ? '' : ' · offline'}${unread ? ` (${unread})` : ''}`);
      option.value = id;
      this.select.append(option);
    }
    this.select.value = this.store.active;
    const privateChat = this.store.active !== 'public';
    this.closeButton.hidden = !privateChat;
    this.conversationStatus.textContent = privateChat
      ? 'Private · available while both people are in this room · not saved after leaving'
      : 'Public room chat · @name to mention · Tab completes names';
    const disabled = !room?.localParticipantId || !room.connected || !room.canChat
      || (privateChat && (!room.getParticipants().has(this.store.active) || this.isIgnored(this.store.active)));
    this.input.disabled = disabled;
    this.sendButton.disabled = disabled;
    this.input.placeholder = disabled ? (privateChat ? 'This person is offline or chat is restricted' : 'Chat is currently restricted') : privateChat ? 'Write a private message…' : 'Type a message…';
    this.messages.classList.toggle('large-chat-text', this.preferences.largeText);
    this.messages.replaceChildren();
    let previousSender: string | undefined;
    let previousTime = 0;
    for (const message of this.store.messages) {
      if (this.store.conversation(message) !== this.store.active || this.isIgnored(message.participantId)) continue;
      const node = el('div', undefined, `chat-msg${message.participantId ? '' : ' system'}`);
      node.dataset['messageId'] = message.messageId;
      const time = Date.parse(message.sentAt);
      const grouped = previousSender === message.participantId && time - previousTime < 120_000;
      node.classList.toggle('grouped', grouped);
      if (message.participantId) {
        const sender = button(message.participantId === this.store.localId ? 'You' : message.participantName, () => {
          const rect = sender.getBoundingClientRect();
          this.options.participantAction(message.participantId, message.participantName, rect.left, rect.bottom);
        }, 'sender chat-sender-button');
        node.append(sender);
      }
      const text = el('div', undefined, 'msg-text');
      appendLinkedText(text, message.content);
      const nickname = room?.nickname;
      if (nickname && message.participantId !== this.store.localId && message.content.toLowerCase().includes(`@${nickname.toLowerCase()}`)) node.classList.add('mentioned');
      node.append(text);
      if (message.participantId) {
        const meta = el('div', undefined, 'msg-time');
        const date = new Date(message.sentAt);
        meta.textContent = `${date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}${message.status === 'pending' ? ' · Sending…' : message.status === 'failed' ? ` · ${message.error}` : ''}`;
        meta.title = date.toLocaleString();
        if (message.status === 'failed') {
          meta.classList.add('delivery-error');
          meta.append(button('Edit & resend', () => {
            this.input.value = message.content;
            this.composition.save(this.store.active, message.content);
            this.input.focus();
          }, 'auth-link-btn'));
        }
        node.append(meta);
      }
      this.messages.append(node);
      previousSender = message.participantId;
      previousTime = time;
    }
    this.messages.scrollTop = atBottom ? this.messages.scrollHeight : scrollTop;
    if (forceScroll) this.seenAtBottom = true;
    if (this.seenAtBottom && this.isVisible()) this.store.markRead(this.store.active);
    this.updateBadges();
  }

  private updateBadges(): void {
    const total = [...this.store.unread.values()].reduce((sum, count) => sum + count, 0);
    document.title = total ? `(${Math.min(total, 999)}) SimplestChat` : 'SimplestChat';
    const badge = document.getElementById('unread-badge')!;
    const activeUnread = this.store.unread.get(this.store.active) ?? 0;
    badge.textContent = String(activeUnread);
    badge.hidden = !activeUnread;
    document.getElementById('scroll-bottom-btn')!.hidden = this.seenAtBottom;
    const tab = document.querySelector<HTMLButtonElement>('[data-tab="chat"]');
    if (tab) {
      let count = tab.querySelector('.conversation-unread');
      if (!count) { count = el('span', '', 'conversation-unread'); tab.append(count); }
      count.textContent = total ? ` ${total}` : '';
    }
    for (const option of this.select.options) {
      const unread = this.store.unread.get(option.value) ?? 0;
      const name = option.value === 'public' ? 'Public chat' : `${this.store.names.get(option.value) ?? 'Conversation'}${this.options.getRoom()?.getParticipants().has(option.value) ? '' : ' · offline'}`;
      option.textContent = `${name}${unread ? ` (${unread})` : ''}`;
    }
  }

  private onKey(event: KeyboardEvent): void {
    if (event.isComposing) return;
    if (event.key === 'Enter') { event.preventDefault(); this.send(); }
    if (event.key === 'Tab') {
      const position = this.input.selectionStart ?? 0;
      const prefix = this.input.value.slice(0, position);
      const match = /(?:^|\s)@([^@\s]*)$/.exec(prefix);
      if (match) {
        const candidates = [...(this.options.getRoom()?.getParticipants().values() ?? [])]
          .filter(person => person.name.toLowerCase().startsWith(match[1]!.toLowerCase()));
        if (candidates.length) {
          event.preventDefault();
          this.insertText(`@${candidates[0]!.name} `, position - match[1]!.length - 1, position);
        }
      }
    }
    if (event.key === 'ArrowUp' && (this.input.value === '' || this.composition.isRecalling(this.store.active))) {
      event.preventDefault();
      this.input.value = this.composition.recall(this.store.active, 'up', this.input.value);
    } else if (event.key === 'ArrowDown' && this.composition.isRecalling(this.store.active)) {
      event.preventDefault();
      this.input.value = this.composition.recall(this.store.active, 'down', this.input.value);
    }
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
    this.composition.save(this.store.active, this.input.value);
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
    return !document.hidden && document.getElementById('room-screen')?.hidden === false
      && document.getElementById('chat-panel')?.classList.contains('active') === true;
  }

  private contextCurrent(activation: number, room: RoomClient, viewer: string): boolean {
    return activation === this.activation && this.options.getRoom() === room
      && viewer === this.viewerKey && viewer === this.options.getViewerKey()
      && this.scope === room.currentRoomId && this.store.localId === room.localParticipantId
      && this.membershipVersion === room.membershipVersion;
  }

  private beginPreferenceUpdate(): number {
    if (this.preferenceBusy) throw new Error('Please wait for the current chat preference update');
    this.preferenceBusy = true;
    return ++this.preferenceOperation;
  }

  private loadPreferences(): void {
    this.preferences = { allowPrivateMessages: true, sounds: false, largeText: false, ignored: [] };
    try {
      const value = JSON.parse(localStorage.getItem(this.preferenceKey()) ?? 'null') as Partial<Preferences> | null;
      if (!value) return;
      this.preferences.allowPrivateMessages = value.allowPrivateMessages !== false;
      this.preferences.sounds = value.sounds === true;
      this.preferences.largeText = value.largeText === true;
      const ignored = new Map<string, { id: string; name: string }>();
      if (Array.isArray(value.ignored)) for (const entry of value.ignored) {
        if (typeof entry?.id !== 'string' || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(entry.id)
          || entry.id === this.store.localId || typeof entry.name !== 'string') continue;
        ignored.set(entry.id, { id: entry.id, name: entry.name.slice(0, 128) });
        if (ignored.size >= 100) break;
      }
      this.preferences.ignored = [...ignored.values()];
    } catch { /* Preferences are optional. */ }
  }

  private preferenceKey(viewer = this.viewerKey): string { return `simplestchat.chat.v1.${viewer}`; }
  private savePreferences(viewer = this.viewerKey): void {
    if (viewer !== this.viewerKey || viewer !== this.options.getViewerKey()) return;
    try { localStorage.setItem(this.preferenceKey(viewer), JSON.stringify(this.preferences)); } catch { /* Private browsing/quota. */ }
  }
  private async pushPreferences(room: RoomClient, preferences: Preferences, temporary: Map<string, string>): Promise<void> {
    await room.requestSocial('setChatPreferences', {
      allowPrivateMessages: preferences.allowPrivateMessages,
      ignoredParticipantIds: [...preferences.ignored.map(entry => entry.id), ...temporary.keys()],
    });
  }

  private openPreferences(): void {
    const room = this.options.getRoom();
    if (!room?.localParticipantId) return;
    this.preferencesDialog?.close();
    const activation = this.activation, viewer = this.viewerKey;
    const view = modal('Chat preferences');
    this.preferencesDialog = view;
    view.dialog.addEventListener('close', () => { if (this.preferencesDialog === view) this.preferencesDialog = null; }, { once: true });
    const current = (): boolean => this.preferencesDialog === view && view.dialog.open
      && this.contextCurrent(activation, room, viewer);
    const allow = el('input'); allow.type = 'checkbox'; allow.checked = this.preferences.allowPrivateMessages;
    const sounds = el('input'); sounds.type = 'checkbox'; sounds.checked = this.preferences.sounds;
    const large = el('input'); large.type = 'checkbox'; large.checked = this.preferences.largeText;
    view.body.append(field('Allow incoming private messages', allow), field('Message and PM sounds', sounds), field('Larger chat text', large));
    const save = button('Save preferences', () => void busy(save, view.error, async () => {
      if (!current()) return;
      const operation = this.beginPreferenceUpdate();
      const next = { ...this.preferences, allowPrivateMessages: allow.checked, sounds: sounds.checked, largeText: large.checked };
      // Audio must unlock during the click, before waiting for the server.
      this.resumeSoundFromGesture(sounds.checked);
      try {
        await this.pushPreferences(room, next, new Map(this.temporaryIgnored));
        if (!this.contextCurrent(activation, room, viewer) || operation !== this.preferenceOperation) return;
        this.preferences = next;
        this.savePreferences(viewer);
        if (current() && next.sounds) this.playSound(550);
        this.render();
        if (current()) view.close();
      } finally { if (operation === this.preferenceOperation) this.preferenceBusy = false; }
    }), 'btn-primary');
    view.body.append(save, el('h3', 'Ignored people'));
    const ignored = [...this.preferences.ignored, ...[...this.temporaryIgnored].map(([id, name]) => ({ id, name }))];
    if (!ignored.length) view.body.append(el('p', 'No one is ignored. Use a person’s menu to ignore their messages.'));
    for (const person of ignored) {
      const row = el('div', undefined, 'community-row');
      const restore = button('Unignore', () => void busy(restore, view.error, async () => {
        if (!current()) return;
        await this.toggleIgnore(person.id, person.name, false);
        if (current() && !this.isIgnored(person.id)) row.remove();
      }));
      row.append(el('span', person.name), restore);
      view.body.append(row);
    }
    view.body.append(el('p', 'Account ignores and preferences are saved in this browser. Guest ignores last only for this room session.', 'setting-hint'));
  }

  private resumeSoundFromGesture(enabled = this.preferences.sounds): void {
    if (!enabled) return;
    try {
      if (!this.audio || this.audio.state === 'closed') this.audio = new AudioContext();
      if (this.audio.state === 'suspended') void this.audio.resume().catch(() => {});
    } catch { /* Unsupported or denied audio must never block chat. */ }
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
    oscillator.start(); oscillator.stop(this.audio.currentTime + 0.13);
    oscillator.onended = () => { oscillator.disconnect(); gain.disconnect(); };
    } catch { /* Output devices may disappear while the room is open. */ }
  }
}

export function appendLinkedText(parent: HTMLElement, text: string): void {
  const pattern = /https?:\/\/[^\s<>]+/g;
  let start = 0;
  for (const match of text.matchAll(pattern)) {
    const index = match.index!;
    parent.append(document.createTextNode(text.slice(start, index)));
    const link = el('a', match[0]);
    link.href = match[0]; link.target = '_blank'; link.rel = 'noopener noreferrer';
    parent.append(link);
    start = index + match[0].length;
  }
  parent.append(document.createTextNode(text.slice(start)));
}
