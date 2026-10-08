import type { ChatEntry, ChatReaction } from './protocol';

export interface ChatItem extends ChatEntry {
  status: 'pending' | 'sent' | 'failed' | 'unknown';
  error?: string;
  retry?: { sequence: number; chatSessionId: string; expiresAt: number };
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
  private readonly removedMessages = new Map<string, string>();
  private readonly editedMessages = new Map<string, ChatEntry>();
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
    if (entry.removedAt) this.removeMessage(entry.messageId, entry.removedAt);
    if (entry.editedAt && entry.revision > 0) this.editMessage(entry);
    entry = this.revised(entry);
    entry = this.redacted(entry);
    const conversation = this.conversation(entry);
    const existing = this.messages.find(
      (message) => message.messageId === entry.messageId || this.key(message) === this.key(entry),
    );
    if (existing) {
      if (this.conversation(existing) !== conversation) return false;
      if (!entry.removedAt && entry.revision < existing.revision) return false;
      Object.assign(existing, entry, { status: 'sent', error: undefined });
      if (existing.removedAt) delete existing.replyTo;
      delete existing.retry;
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

  pending(entry: ChatEntry, retry?: ChatItem['retry']): boolean {
    if (!this.accepts(entry) || entry.participantId !== this.localId) return false;
    if (this.messages.some((item) => this.key(item) === this.key(entry))) return false;
    this.rememberName(entry);
    const item: ChatItem = { ...entry, status: 'pending', ...(retry && { retry }) };
    this.messages.push(item);
    this.trim();
    return this.messages.includes(item);
  }

  fail(clientMessageId: string, message: string, unknown = false): void {
    const existing = this.messages.find(
      (item) => item.clientMessageId === clientMessageId && item.participantId === this.localId,
    );
    if (existing && (existing.status === 'pending' || existing.status === 'unknown')) {
      existing.status = unknown ? 'unknown' : 'failed';
      existing.error = message.slice(0, 1024);
      if (!unknown) delete existing.retry;
      this.trim();
    }
  }

  retireAttempts(): void {
    for (const item of this.messages) {
      if (item.status === 'pending' || item.status === 'unknown') {
        item.status = 'unknown';
        item.error = 'Delivery not confirmed. The room session changed; retry is unavailable.';
      }
      delete item.retry;
    }
  }

  /** The oldest retained message in a conversation still counted unread. */
  firstUnread(id: string): ChatItem | undefined {
    return this.messages.find(
      (item) => this.conversation(item) === id && this.unreadMessages.has(this.key(item)),
    );
  }

  /** A retained message's reactions changed; they count toward the store's bound. */
  setReactions(messageId: string, reactions: ChatReaction[]): boolean {
    const item = this.messages.find((entry) => entry.messageId === messageId);
    if (!item || item.removedAt) return false;
    item.reactions = reactions;
    this.trim();
    return true;
  }

  /** Edits update loaded quotes and fence older in-flight acknowledgements or replay. */
  editMessage(entry: ChatEntry): boolean {
    if (!this.accepts(entry) || this.removedMessages.has(entry.messageId)) return false;
    const loaded = this.messages.find((message) => message.messageId === entry.messageId);
    if (loaded && (loaded.removedAt || loaded.revision >= entry.revision)) return false;
    const previous = this.editedMessages.get(entry.messageId);
    if (previous && previous.revision >= entry.revision) return false;
    this.editedMessages.set(entry.messageId, { ...entry });
    while (
      this.editedMessages.size > this.maxMessages * 2 ||
      [...this.editedMessages.values()].reduce(
        (total, message) => total + message.content.length,
        0,
      ) > this.maxCharacters
    )
      this.editedMessages.delete(this.editedMessages.keys().next().value!);
    let changed = false;
    for (const message of this.messages) {
      const revised = this.revised(message);
      if (revised === message) continue;
      Object.assign(message, this.redacted(revised));
      changed = true;
    }
    this.trim();
    return changed;
  }

  private revised(entry: ChatEntry): ChatEntry {
    const edit = this.editedMessages.get(entry.messageId);
    if (
      edit &&
      edit.revision > entry.revision &&
      this.conversation(edit) === this.conversation(entry)
    )
      entry = {
        ...entry,
        content: edit.content,
        revision: edit.revision,
        ...(edit.editedAt && { editedAt: edit.editedAt }),
      };
    const quoted = entry.replyTo && this.editedMessages.get(entry.replyTo.messageId);
    if (quoted && entry.replyTo) {
      const flat = quoted.content.split(/\s+/).join(' ').trim();
      return {
        ...entry,
        replyTo: {
          ...entry.replyTo,
          excerpt: flat.length > 140 ? `${flat.slice(0, 140).trimEnd()}…` : flat,
        },
      };
    }
    return entry;
  }

  /** Redact late replay/acknowledgements too, including quotes without a loaded original. */
  private redacted(entry: ChatEntry): ChatEntry {
    if (entry.recipientId) return entry;
    const removedAt = this.removedMessages.get(entry.messageId) ?? entry.removedAt;
    if (removedAt) {
      const redacted = { ...entry, removedAt, content: '', reactions: [] };
      delete redacted.replyTo;
      return redacted;
    }
    if (entry.replyTo && this.removedMessages.has(entry.replyTo.messageId))
      return { ...entry, replyTo: { ...entry.replyTo, excerpt: 'Message removed' } };
    return entry;
  }

  removeMessage(messageId: string, removedAt: string): boolean {
    this.removedMessages.set(messageId, removedAt);
    this.editedMessages.delete(messageId);
    while (this.removedMessages.size > this.maxMessages * 2)
      this.removedMessages.delete(this.removedMessages.keys().next().value!);
    let changed = false;
    for (const item of this.messages) {
      const safe = this.redacted(item);
      if (safe === item) continue;
      Object.assign(item, safe);
      if (item.removedAt) {
        delete item.retry;
        delete item.replyTo;
      }
      changed = true;
    }
    this.trim();
    return changed;
  }

  /** Discard pre-recovery public rows that the authoritative replay no longer contains. */
  prunePublicReplay(captured: ReadonlySet<string>, retained: ReadonlySet<string>): Set<string> {
    const discarded = new Set<string>();
    for (let index = this.messages.length - 1; index >= 0; index--) {
      const message = this.messages[index]!;
      if (
        message.status !== 'sent' ||
        message.recipientId ||
        !message.participantId ||
        !captured.has(message.messageId) ||
        retained.has(message.messageId)
      )
        continue;
      discarded.add(message.messageId);
      this.unreadMessages.delete(this.key(message));
      this.messages.splice(index, 1);
    }
    this.refreshUnread();
    return discarded;
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
    this.removedMessages.clear();
    this.editedMessages.clear();
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
      (item.retry ? item.retry.chatSessionId.length + 24 : 0) +
      (item.error?.length ?? 0) +
      (item.replyTo
        ? item.replyTo.excerpt.length +
          item.replyTo.participantName.length +
          item.replyTo.messageId.length +
          item.replyTo.participantId.length
        : 0) +
      (item.reactions ?? []).reduce(
        (total, reaction) =>
          total + reaction.emoji.length + reaction.participantIds.join('').length,
        0,
      )
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

/** Only unsent account PM drafts persist on this device, separated by account and peer. */
export class SavedPmDrafts {
  constructor(private readonly storage: Pick<Storage, 'getItem' | 'setItem'>) {}
  private entries(account: string): [string, string][] {
    try {
      const value: unknown = JSON.parse(
        this.storage.getItem(`simplestchat.pm-drafts.${account}`) ?? '[]',
      );
      if (!Array.isArray(value)) return [];
      return value
        .filter(
          (entry): entry is [string, string] =>
            Array.isArray(entry) &&
            entry.length === 2 &&
            typeof entry[0] === 'string' &&
            entry[0].length <= 128 &&
            typeof entry[1] === 'string' &&
            entry[1].length <= 2000,
        )
        .slice(-100);
    } catch {
      return [];
    }
  }
  get(account: string, peer: string): string {
    return this.entries(account).find(([id]) => id === peer)?.[1] ?? '';
  }
  save(account: string, peer: string, text: string): void {
    if (!account || !peer) return;
    const entries = this.entries(account).filter(([id]) => id !== peer);
    if (text) entries.push([peer, text.slice(0, 2000)]);
    while (
      entries.length > 100 ||
      entries.reduce((size, [id, draft]) => size + id.length + draft.length, 0) > 64 * 1024
    )
      entries.shift();
    try {
      this.storage.setItem(`simplestchat.pm-drafts.${account}`, JSON.stringify(entries));
    } catch {
      /* Storage restrictions must never interrupt composing or sending. */
    }
  }
}
