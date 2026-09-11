import type { ChatEntry } from './protocol';

export interface ChatItem extends ChatEntry {
  status: 'pending' | 'sent' | 'failed';
  error?: string;
}

type Composition = { draft: string; history: string[]; cursor: number; beforeRecall: string };

/** Drafts and sent-input recall never cross a conversation boundary. */
export class ConversationInputs {
  private readonly conversations = new Map<string, Composition>();

  private state(id: string): Composition {
    const value = this.conversations.get(id) ?? {
      draft: '',
      history: [],
      cursor: -1,
      beforeRecall: '',
    };
    this.conversations.delete(id);
    this.conversations.set(id, value);
    return value;
  }

  draft(id: string): string {
    return this.conversations.get(id)?.draft ?? '';
  }

  save(id: string, value: string): void {
    const state = this.state(id);
    state.draft = value.slice(0, 2000);
    state.cursor = -1;
    state.beforeRecall = '';
    this.trim(id);
  }

  sent(id: string, value: string): void {
    const state = this.state(id);
    state.history.unshift(value.slice(0, 2000));
    state.history.splice(50);
    state.draft = '';
    state.cursor = -1;
    state.beforeRecall = '';
    this.trim(id);
  }

  recall(id: string, direction: 'up' | 'down', current: string): string {
    const state = this.state(id);
    if (!state.history.length) return current;
    if (direction === 'up') {
      if (state.cursor < 0) state.beforeRecall = current;
      state.cursor = Math.min(state.history.length - 1, state.cursor + 1);
    } else if (state.cursor >= 0) state.cursor--;
    state.draft = state.history[state.cursor] ?? state.beforeRecall;
    this.trim(id);
    return state.draft;
  }

  isRecalling(id: string): boolean {
    return (this.conversations.get(id)?.cursor ?? -1) >= 0;
  }
  close(id: string): void {
    this.conversations.delete(id);
  }
  reset(): void {
    this.conversations.clear();
  }

  private trim(active: string): void {
    const size = (): number =>
      [...this.conversations.values()].reduce(
        (total, state) =>
          total +
          state.draft.length +
          state.beforeRecall.length +
          state.history.reduce((sum, text) => sum + text.length, 0),
        0,
      );
    while (this.conversations.size > 100 || size() > 256 * 1024) {
      const oldest = [...this.conversations.keys()].find((id) => id !== active);
      if (!oldest) break;
      this.conversations.delete(oldest);
    }
  }
}

/** Bounded room-session text and unread state; never written to browser storage. */
export class ChatStore {
  readonly messages: ChatItem[] = [];
  readonly unread = new Map<string, number>();
  readonly names = new Map<string, string>();
  private readonly unreadMessages = new Set<string>();
  private readonly dismissedMessages = new Map<string, true>();
  active = 'public';
  localId = '';

  constructor(
    private readonly maxMessages = 500,
    private readonly maxCharacters = 512 * 1024,
  ) {}

  conversation(message: ChatEntry): string {
    if (!message.recipientId) return 'public';
    return message.participantId === this.localId ? message.recipientId : message.participantId;
  }

  private key(message: ChatEntry): string {
    return JSON.stringify([
      message.participantId,
      message.clientMessageId,
      message.recipientId ?? 'public',
    ]);
  }

  private accepts(entry: ChatEntry): boolean {
    return (
      !!this.localId &&
      !!entry &&
      typeof entry.messageId === 'string' &&
      !!entry.messageId &&
      typeof entry.clientMessageId === 'string' &&
      !!entry.clientMessageId &&
      typeof entry.participantId === 'string' &&
      typeof entry.participantName === 'string' &&
      typeof entry.content === 'string' &&
      Number.isFinite(Date.parse(entry.sentAt)) &&
      (!entry.recipientId ||
        entry.participantId === this.localId ||
        entry.recipientId === this.localId)
    );
  }

  private rememberName(entry: ChatEntry): void {
    const conversation = this.conversation(entry);
    if (conversation === 'public') return;
    const name = entry.participantId === this.localId ? entry.recipientName : entry.participantName;
    this.names.set(conversation, name ?? this.names.get(conversation) ?? 'Conversation');
  }

  receive(entry: ChatEntry, _replay = false): boolean {
    if (!this.accepts(entry) || this.dismissedMessages.has(this.key(entry))) return false;
    const conversation = this.conversation(entry);
    const existing = this.messages.find(
      (message) => message.messageId === entry.messageId || this.key(message) === this.key(entry),
    );
    if (existing) {
      if (this.conversation(existing) !== conversation) return false;
      Object.assign(existing, entry, { status: 'sent', error: undefined });
      this.rememberName(entry);
      this.trim();
      return false;
    }
    this.rememberName(entry);
    const item: ChatItem = { ...entry, status: 'sent' };
    this.messages.push(item);
    if (
      conversation !== this.active &&
      entry.participantId &&
      entry.participantId !== this.localId
    ) {
      // Only unseen replay entries count; duplicates return above.
      this.unreadMessages.add(this.key(entry));
    }
    this.trim();
    return this.messages.includes(item);
  }

  pending(entry: ChatEntry): boolean {
    if (!this.accepts(entry) || entry.participantId !== this.localId) return false;
    if (this.messages.some((item) => this.key(item) === this.key(entry))) return false;
    this.rememberName(entry);
    const item: ChatItem = { ...entry, status: 'pending' };
    this.messages.push(item);
    this.trim();
    return this.messages.includes(item);
  }

  fail(clientMessageId: string, message: string): void {
    const existing = this.messages.find(
      (item) => item.clientMessageId === clientMessageId && item.participantId === this.localId,
    );
    if (existing?.status === 'pending') {
      existing.status = 'failed';
      existing.error = message.slice(0, 1024);
      this.trim();
    }
  }

  markRead(id: string): void {
    for (const item of this.messages)
      if (this.conversation(item) === id) this.unreadMessages.delete(this.key(item));
    this.refreshUnread();
  }

  markUnread(entry: ChatEntry): void {
    if (!entry.participantId || entry.participantId === this.localId) return;
    if (this.messages.some((item) => this.key(item) === this.key(entry)))
      this.unreadMessages.add(this.key(entry));
    this.refreshUnread();
  }

  open(id: string, name?: string): void {
    this.active = id;
    if (id !== 'public') this.names.set(id, name ?? this.names.get(id) ?? 'Conversation');
    this.markRead(id);
    this.trim();
  }

  close(id: string): void {
    for (let index = this.messages.length - 1; index >= 0; index--) {
      const item = this.messages[index]!;
      if (this.conversation(item) !== id) continue;
      const key = this.key(item);
      this.dismissedMessages.delete(key);
      this.dismissedMessages.set(key, true);
      this.unreadMessages.delete(key);
      this.messages.splice(index, 1);
    }
    this.names.delete(id);
    if (this.active === id) this.active = 'public';
    this.refreshUnread();
    // Identities suppress replay/late acks without trusting the browser clock.
    while (this.dismissedMessages.size > this.maxMessages * 2)
      this.dismissedMessages.delete(this.dismissedMessages.keys().next().value!);
  }

  reset(localId = ''): void {
    this.messages.length = 0;
    this.unread.clear();
    this.unreadMessages.clear();
    this.names.clear();
    this.dismissedMessages.clear();
    this.active = 'public';
    this.localId = localId;
  }

  private characters(item: ChatItem): number {
    return (
      item.content.length +
      item.participantName.length +
      (item.recipientName?.length ?? 0) +
      item.messageId.length +
      item.clientMessageId.length +
      item.participantId.length +
      (item.recipientId?.length ?? 0) +
      item.sentAt.length +
      (item.error?.length ?? 0)
    );
  }

  private refreshUnread(): void {
    this.unread.clear();
    const retained = new Set(this.messages.map((item) => this.key(item)));
    for (const key of this.unreadMessages) if (!retained.has(key)) this.unreadMessages.delete(key);
    for (const item of this.messages) {
      if (this.unreadMessages.has(this.key(item))) {
        const conversation = this.conversation(item);
        this.unread.set(conversation, (this.unread.get(conversation) ?? 0) + 1);
      }
    }
  }

  private trim(): void {
    this.messages.sort((left, right) => Date.parse(left.sentAt) - Date.parse(right.sentAt));
    let characters = this.messages.reduce((total, item) => total + this.characters(item), 0);
    while (this.messages.length > this.maxMessages || characters > this.maxCharacters) {
      const oldest = this.messages.shift();
      if (!oldest) break;
      characters -= this.characters(oldest);
      this.unreadMessages.delete(this.key(oldest));
    }
    while (this.names.size > 100) {
      const oldest = [...this.names.keys()].find((id) => id !== this.active);
      if (!oldest) break;
      this.close(oldest);
    }
    this.refreshUnread();
  }
}
