import type { ChatEntry, ChatHistoryPage, ChatReaction } from './protocol';
import { accountUpdatesLive, onAccountUpdate } from './account-updates';
import { renderAttachments } from './attachments';
import { mountPmComposer, type PmComposer } from './pm-composer';
import { mountContacts } from './discovery';
import { appendLinkedText, appendEditedLabel, editMessageDialog } from './chat-message-ui';
import { chatColor } from './avatar-colors';
import { CHAT_REACTIONS } from './chat-reactions';
import type { RoomClient } from './room';
import { api, asyncButton, button, el, field, input, modal } from './ui';

type HistoryOptions = {
  title: string;
  accountDialog?: boolean;
  navigation?: {
    accountId: string;
    destination: { kind: 'conversation'; peerId: string; peerName: string };
  };
  viewerId: string;
  initialMessageId?: string;
  draft?: { account: string; peer: string };
  edit?: (
    message: ChatEntry,
    content: string,
    revision: number,
    signal: AbortSignal,
  ) => Promise<ChatEntry>;
  current: () => boolean;
  mountNotifications?: (container: HTMLElement, current: () => boolean) => void;
  getToken?: () => string | null;
  privatePeerId?: string;
  attachmentAuthorization?: (id: string) => Promise<string>;
  load: (params: URLSearchParams, signal: AbortSignal) => Promise<ChatHistoryPage>;
  markRead?: (id: string, signal: AbortSignal) => Promise<unknown>;
  readConfirmed?: (cursor: ChatEntry) => void;
  reply?: (message: ChatEntry) => void;
  replyInPlace?: boolean;
  react?: (
    id: string,
    emoji: string,
    present: boolean,
    signal: AbortSignal,
  ) => Promise<{ messageId: string; reactions: ChatReaction[] }>;
  remove?: (id: string) => Promise<{ removedAt: string }>;
  send?: (
    content: string,
    id: string,
    signal: AbortSignal,
    attachmentIds: string[],
    replyTo?: string,
  ) => Promise<ChatEntry>;
  retention?: { days: number; save: (days: number) => Promise<unknown> };
};
const inboxListeners = new Set<(message?: ChatEntry) => void>();
const privateReadListeners = new Set<(account: string, peer: string, cursor: ChatEntry) => void>();

/** Only a successful account-history read publishes this cursor. */
export function onPrivateRead(
  listener: (account: string, peer: string, cursor: ChatEntry) => void,
): () => void {
  privateReadListeners.add(listener);
  return () => {
    privateReadListeners.delete(listener);
  };
}

export function onInboxChanged(listener: (message?: ChatEntry) => void): () => void {
  inboxListeners.add(listener);
  return () => {
    inboxListeners.delete(listener);
  };
}
export function notifyInboxChanged(message?: ChatEntry): void {
  for (const listener of inboxListeners) listener(message);
}
const editListeners = new Set<(message: ChatEntry) => void>();
export function onHistoryEdit(listener: (message: ChatEntry) => void): () => void {
  editListeners.add(listener);
  return () => {
    editListeners.delete(listener);
  };
}
export function notifyHistoryEdit(message: ChatEntry): void {
  for (const listener of editListeners) listener(message);
}
const removalListeners = new Set<(id: string, at: string) => void>();
const reactionListeners = new Set<(id: string, reactions: ChatReaction[]) => void>();

export function onHistoryReaction(
  listener: (id: string, reactions: ChatReaction[]) => void,
): () => void {
  reactionListeners.add(listener);
  return () => {
    reactionListeners.delete(listener);
  };
}

export function notifyHistoryReaction(id: string, reactions: ChatReaction[]): void {
  for (const listener of reactionListeners) listener(id, reactions);
}

/** Remove content from an open history view as soon as the live event arrives. */
export function notifyHistoryRemoval(id: string, at: string): void {
  for (const listener of removalListeners) listener(id, at);
}

function messageNode(message: ChatEntry): HTMLElement {
  const style = message.chatStyle?.style ?? 'accent';
  const row = el('article', undefined, `history-message chat-msg look-${style}`);
  row.style.setProperty(
    '--sender-color',
    chatColor(message.participantName, message.chatStyle?.color ?? null),
  );
  row.setAttribute('data-message-id', message.messageId);
  const time = el('time', new Date(message.sentAt).toLocaleString(), 'msg-time');
  time.dateTime = message.sentAt;
  row.append(el('strong', message.participantName, 'sender'));
  if (message.replyTo)
    row.append(
      el(
        'blockquote',
        `${message.replyTo.participantName}: ${message.replyTo.excerpt}`,
        'msg-reply',
      ),
    );
  const text = el('div', undefined, 'msg-text');
  if (message.removedAt) text.textContent = 'Message removed';
  else appendLinkedText(text, message.content);
  row.append(text, time);
  appendEditedLabel(row, message);
  return row;
}

type HistoryMessageActions = {
  options: HistoryOptions;
  current: () => boolean;
  close: () => void;
  signal: AbortSignal;
  fail: (error: unknown) => void;
  reactionVersion: (id: string) => number;
  readReactionVersion: () => number;
  refreshMessages: () => void;
};

/** Keep row actions independent from the reader's pagination and refresh lifecycle. */
function appendHistoryMessageActions(
  row: HTMLElement,
  message: ChatEntry,
  context: HistoryMessageActions,
): void {
  const {
    options,
    current,
    close,
    signal,
    fail,
    reactionVersion,
    readReactionVersion,
    refreshMessages,
  } = context;
  if (options.edit && message.participantId === options.viewerId && !message.removedAt) {
    row.append(
      button(
        'Edit',
        () =>
          editMessageDialog(message, {
            current,
            save: (content, revision) => options.edit!(message, content, revision, signal),
            changed: notifyHistoryEdit,
          }),
        'history-edit',
      ),
    );
  }
  if (options.reply && !message.removedAt) {
    row.append(
      button(
        'Reply',
        () => {
          if (!current()) return;
          if (!options.replyInPlace) close();
          options.reply?.(message);
        },
        'history-reply',
      ),
    );
  }
  if (options.react && !message.removedAt) {
    const controls = el('div', undefined, 'msg-reactions');
    const react = async (emoji: string): Promise<void> => {
      if (!current() || !options.react) return;
      const version = reactionVersion(message.messageId);
      const readVersion = readReactionVersion();
      const buttons = controls.querySelectorAll<HTMLButtonElement>('button');
      buttons.forEach((control) => {
        control.disabled = true;
      });
      try {
        const present = !(message.reactions ?? []).some(
          (reaction) =>
            reaction.emoji === emoji && reaction.participantIds.includes(options.viewerId),
        );
        const result = await options.react(message.messageId, emoji, present, signal);
        if (
          current() &&
          result.messageId === message.messageId &&
          version === reactionVersion(message.messageId) &&
          readVersion === readReactionVersion()
        )
          notifyHistoryReaction(result.messageId, result.reactions);
        else if (current() && readVersion !== readReactionVersion()) refreshMessages();
      } finally {
        if (current())
          buttons.forEach((control) => {
            control.disabled = false;
          });
      }
    };
    for (const reaction of message.reactions ?? []) {
      const chip = asyncButton(
        `${reaction.emoji} ${reaction.participantIds.length}`,
        () => react(reaction.emoji),
        fail,
        'reaction-chip',
      );
      chip.setAttribute('aria-pressed', String(reaction.participantIds.includes(options.viewerId)));
      controls.append(chip);
    }
    const picker = el('div', undefined, 'reaction-picker');
    picker.hidden = true;
    for (const emoji of CHAT_REACTIONS)
      picker.append(
        asyncButton(
          emoji,
          async () => {
            picker.hidden = true;
            await react(emoji);
          },
          fail,
          'msg-action',
        ),
      );
    controls.append(
      button('React', () => {
        picker.hidden = !picker.hidden;
      }),
      picker,
    );
    row.append(controls);
  }
  if (options.remove && !message.removedAt) {
    row.append(
      button(
        'Remove',
        () => {
          const confirmation = modal('Remove message');
          confirmation.body.append(
            el('p', 'Remove this public message and its quoted text for everyone?'),
          );
          const remove = asyncButton(
            'Remove message',
            async () => {
              if (!current() || !options.remove || remove.disabled) return;
              remove.disabled = true;
              try {
                const result = await options.remove(message.messageId);
                if (current()) notifyHistoryRemoval(message.messageId, result.removedAt);
                confirmation.close();
              } finally {
                remove.disabled = false;
              }
            },
            (error) => {
              confirmation.error.textContent =
                error instanceof Error ? error.message : 'Message could not be removed';
              confirmation.error.hidden = false;
            },
          );
          confirmation.body.append(remove);
        },
        'history-remove',
      ),
    );
  }
}

/** A bounded page reader shared by room history and the account inbox. */
function openHistory(options: HistoryOptions): void {
  const view = modal(options.title, undefined, options.navigation);
  if (options.accountDialog) view.dialog.setAttribute('data-account-dialog', 'true');
  const controller = new AbortController();
  const current = (): boolean => view.dialog.open && options.current();
  options.mountNotifications?.(view.body, current);
  const status = el('p', 'Loading messages…', 'setting-hint');
  status.setAttribute('role', 'status');
  const search = input('', 'search', 128);
  search.placeholder = 'Search saved messages';
  search.title = 'Search the newest 10,000 saved messages';
  const searchForm = el('form', undefined, 'history-search');
  const searchButton = el('button', 'Search', 'btn-secondary');
  searchButton.type = 'submit';
  const list = el('div', undefined, 'history-messages timestamps-always');
  list.setAttribute('aria-label', 'Saved messages');
  list.setAttribute('tabindex', '0');
  const pages = el('div', undefined, 'history-pagination');
  let page: ChatHistoryPage | null = null;
  let before: string | null = null;
  let query = '';
  let position: { after?: string; around?: string; resume?: boolean } = {};
  let anchor: string | null = options.initialMessageId ?? null;
  let operation = 0;
  let loading = false;
  let refreshQueued = false;
  let read = '';
  let attachmentRows: (() => void)[] = [];
  let disposeComposer: PmComposer | null = null;
  const clearAttachmentRows = (): void => {
    for (const dispose of attachmentRows) dispose();
    attachmentRows = [];
  };
  const removed = new Map<string, string>();
  const edits = new Map<string, ChatEntry>();
  const reactions = new Map<string, ChatReaction[]>();
  const reactionVersions = new Map<string, number>();
  let reactionReadVersion = 0;
  let failedLoadMessage: string | null = null;
  const redact = (entry: ChatEntry): void => {
    const updatedReactions = reactions.get(entry.messageId);
    if (updatedReactions) entry.reactions = updatedReactions;
    const edit = edits.get(entry.messageId);
    if (edit && edit.revision > entry.revision)
      Object.assign(entry, {
        content: edit.content,
        revision: edit.revision,
        editedAt: edit.editedAt,
      });
    const quoted = entry.replyTo && edits.get(entry.replyTo.messageId);
    if (quoted && entry.replyTo) {
      const flat = quoted.content.split(/\s+/).join(' ').trim();
      entry.replyTo.excerpt = flat.length > 140 ? `${flat.slice(0, 140).trimEnd()}…` : flat;
    }
    const at = removed.get(entry.messageId);
    if (at) {
      entry.content = '';
      entry.removedAt = at;
      delete entry.replyTo;
      entry.reactions = [];
      entry.attachments = [];
    }
    if (entry.replyTo && removed.has(entry.replyTo.messageId))
      entry.replyTo.excerpt = 'Message removed';
  };
  const fail = (error: unknown): void => {
    if (!current()) return;
    view.error.textContent =
      error instanceof Error ? error.message : 'Messages could not be loaded';
    view.error.hidden = false;
  };
  const visible = (): boolean => {
    const dialogs = document.querySelectorAll<HTMLDialogElement>('dialog[open]');
    return !document.hidden && dialogs[dialogs.length - 1] === view.dialog;
  };
  const markRead = (): void => {
    if (
      !current() ||
      !visible() ||
      before ||
      query ||
      position.around ||
      !options.markRead ||
      !page?.retentionDays ||
      loading
    )
      return;
    if (list.scrollHeight - list.scrollTop - list.clientHeight > 48) return;
    const cursor = page.messages[page.messages.length - 1];
    if (!cursor || cursor.messageId === read) return;
    const latest = cursor.messageId;
    read = latest;
    options
      .markRead(latest, controller.signal)
      .then(() => {
        if (current()) {
          options.readConfirmed?.(cursor);
          notifyInboxChanged();
        }
      })
      .catch((error: unknown) => {
        if (read === latest) read = '';
        fail(error);
      });
  };
  const messageActions: HistoryMessageActions = {
    options,
    current,
    close: () => view.close(),
    signal: controller.signal,
    fail,
    reactionVersion: (id) => reactionVersions.get(id) ?? 0,
    readReactionVersion: () => reactionReadVersion,
    refreshMessages: () => refreshMessages(),
  };
  const render = (bottom: boolean): void => {
    const scroll = list.scrollTop;
    clearAttachmentRows();
    const nodes = (page?.messages ?? []).map((message) => {
      const row = messageNode(message);
      if (!message.removedAt && message.attachments?.length && options.attachmentAuthorization) {
        attachmentRows.push(
          renderAttachments(row, message.attachments, {
            current: () => current() && row.isConnected && !message.removedAt,
            authorization: options.attachmentAuthorization,
          }),
        );
      }
      if (message.messageId === page?.firstUnreadMessageId) {
        const divider = el('div', 'New messages', 'chat-divider');
        divider.setAttribute('role', 'separator');
        row.insertBefore(divider, row.firstChild);
      }
      if (query)
        row.append(
          button(
            'Show in conversation',
            () => {
              search.value = '';
              anchor = message.messageId;
              load(null, false, '', { around: message.messageId }).catch(fail);
            },
            'history-context',
          ),
        );
      appendHistoryMessageActions(row, message, messageActions);
      return row;
    });
    list.replaceChildren(...nodes);
    if (!nodes.length)
      list.append(
        el(
          'p',
          query ? 'No messages match this search.' : 'No saved messages yet.',
          'setting-hint',
        ),
      );
    list.scrollTop = bottom ? list.scrollHeight : scroll;
    const target = anchor && nodes.find((row) => row.getAttribute('data-message-id') === anchor);
    if (target) {
      target.scrollIntoView?.({ block: 'start' });
      target.classList?.toggle('flash', true);
    }
    anchor = null;
    updatePagination();
    status.textContent = page?.retentionDays
      ? `Messages are kept for ${page.retentionDays} days.${query ? ' Searching the newest 10,000 saved messages.' : ''}`
      : 'Room history is off. Messages are only available during the room session.';
    markRead();
  };
  const updatePagination = (): void => {
    older.disabled = loading || !page?.nextCursor;
    newer.disabled = loading || !page?.newerCursor;
    newest.disabled = loading || (!before && !page?.newerCursor && !position.around && !query);
  };
  const load = async (
    cursor: string | null,
    quiet = false,
    requestedQuery = query,
    requestedPosition: typeof position = {},
  ): Promise<void> => {
    if (!current()) return;
    const version = ++operation;
    loading = true;
    updatePagination();
    const bottom = !quiet || list.scrollHeight - list.scrollTop - list.clientHeight <= 48;
    const params = new URLSearchParams({ limit: '50' });
    if (cursor) params.set('before', cursor);
    if (requestedQuery) params.set('q', requestedQuery);
    if (requestedPosition.after) params.set('after', requestedPosition.after);
    if (requestedPosition.around) params.set('around', requestedPosition.around);
    if (requestedPosition.resume) params.set('resume', 'true');
    const observedReactions = new Map(reactionVersions);
    try {
      const result = await options.load(params, controller.signal);
      if (!current() || version !== operation) return;
      reactionReadVersion++;
      // A fresh HTTP snapshot supersedes cached reactions from before the read.
      // Keep only live events that arrived while this request was in flight.
      for (const [id, observed] of observedReactions)
        if (reactionVersions.get(id) === observed) reactions.delete(id);
      const previous = page === null ? null : JSON.stringify(page);
      page = result;
      page.messages.forEach(redact);
      if (requestedPosition.resume && !quiet) anchor = page.firstUnreadMessageId;
      position = requestedPosition;
      before = cursor;
      query = requestedQuery;
      if (!quiet || view.error.textContent === failedLoadMessage) view.error.hidden = true;
      failedLoadMessage = null;
      loading = false;
      if (quiet && previous === JSON.stringify(page)) {
        markRead();
        return;
      }
      render(bottom && !anchor);
    } catch (error) {
      if (version === operation) {
        fail(error);
        failedLoadMessage = view.error.textContent;
      }
    } finally {
      if (version === operation) {
        loading = false;
        if (current()) updatePagination();
        if (refreshQueued) {
          refreshQueued = false;
          refreshMessages();
        }
      }
    }
  };
  const older = button('Older messages', () => {
    if (!loading && page?.nextCursor) load(page.nextCursor).catch(fail);
  });
  const newer = button('Newer messages', () => {
    if (!loading && page?.newerCursor)
      load(null, false, '', { after: page.newerCursor }).catch(fail);
  });
  const newest = button('Newest messages', () => {
    if (!loading) {
      search.value = '';
      load(null, false, '').catch(fail);
    }
  });
  updatePagination();
  pages.append(older, newer, newest);
  searchForm.append(field('Search messages', search), searchButton);
  searchForm.addEventListener('submit', (event) => {
    event.preventDefault();
    const value = search.value.trim();
    if (value && value.length < 3) {
      fail(new Error('Use at least 3 characters to search'));
      return;
    }
    load(null, false, value).catch(fail);
  });
  view.body.append(status, searchForm, list, pages);
  if (options.retention) {
    const retention = options.retention;
    const select = el('select');
    for (const days of [0, 1, 7, 30, 90]) {
      const option = el('option', days ? `${days} day${days === 1 ? '' : 's'}` : 'Off');
      option.value = String(days);
      select.append(option);
    }
    select.value = String(retention.days);
    const save = asyncButton(
      'Save retention',
      async () => {
        if (!current() || save.disabled) return;
        save.disabled = true;
        try {
          await retention.save(Number(select.value));
          if (current()) await load(null);
        } finally {
          save.disabled = false;
        }
      },
      fail,
    );
    const controls = el('div', undefined, 'history-retention');
    controls.append(
      field('Room history retention', select),
      el(
        'p',
        'New people joining this room can read saved public messages. Turning history off deletes it; shortening retention deletes older messages.',
        'setting-hint',
      ),
      save,
    );
    view.body.append(controls);
  }
  if (options.send && options.draft) {
    disposeComposer = mountPmComposer(view.body, {
      ...options.draft,
      current: options.current,
      getToken: () => options.getToken?.() ?? null,
      signal: controller.signal,
      send: options.send,
      fail,
      confirmed: async (message) => {
        search.value = '';
        await load(null, false, '');
        notifyInboxChanged(message);
      },
    });
    options.reply = (message) => disposeComposer?.reply(message);
    options.replyInPlace = true;
  }
  const onRemoved = (id: string, at: string): void => {
    if (!current()) return;
    removed.set(id, at);
    // Never evict a tombstone while an older request could still restore its content.
    if (removed.size > 300) {
      view.close();
      return;
    }
    if (!page) return;
    page.messages.forEach(redact);
    render(false);
  };
  const onEdited = (message: ChatEntry): void => {
    if (!current() || removed.has(message.messageId)) return;
    const previous = edits.get(message.messageId);
    if (previous && previous.revision >= message.revision) return;
    edits.set(message.messageId, message);
    if (edits.size > 300) {
      view.close();
      return;
    }
    page?.messages.forEach(redact);
    render(false);
  };
  editListeners.add(onEdited);
  removalListeners.add(onRemoved);
  const stopReactions = onHistoryReaction((id, value) => {
    if (!current() || removed.has(id)) return;
    reactions.set(id, value);
    reactionVersions.set(id, (reactionVersions.get(id) ?? 0) + 1);
    if (reactionVersions.size > 300) {
      view.close();
      return;
    }
    page?.messages.forEach(redact);
    render(false);
  });
  const refreshMessages = (): void => {
    if (!current() || !visible() || (position.resume && page?.newerCursor)) return;
    if (loading) {
      refreshQueued = true;
      return;
    }
    load(before, true, query, position.resume ? {} : position).catch(fail);
  };
  const stopInbox = onInboxChanged((message) => {
    if (!options.privatePeerId || !message) return;
    if (page?.messages.some((entry) => entry.messageId === message.messageId)) return;
    const pair = [options.viewerId, options.privatePeerId];
    if (pair.includes(message.participantId) && pair.includes(message.recipientId ?? ''))
      refreshMessages();
  });
  let accountDirty = false;
  const refreshAccount = (): void => {
    if (!accountDirty || !current() || !visible()) return;
    accountDirty = false;
    refreshMessages();
  };
  const stopUpdates = onAccountUpdate((update) => {
    if (
      !options.privatePeerId ||
      update.account !== options.viewerId ||
      (update.scope !== 'ready' && update.scope !== 'inbox') ||
      (update.peerId && update.peerId !== options.privatePeerId)
    )
      return;
    accountDirty = true;
    refreshAccount();
  });
  window.addEventListener('focus', refreshAccount, { signal: controller.signal });
  document.addEventListener('visibilitychange', refreshAccount, { signal: controller.signal });
  document.addEventListener('close', () => queueMicrotask(refreshAccount), {
    capture: true,
    signal: controller.signal,
  });
  list.addEventListener('scroll', markRead);
  const timer = setInterval(() => {
    if (!options.current()) {
      view.close();
      return;
    }
    // Repeating resume after its cursor advances would silently skip to the next
    // unread page. Keep that page stable until the reader chooses Newer messages.
    refreshAccount();
    if (
      (!options.privatePeerId || !view.error.hidden || !accountUpdatesLive(options.viewerId)) &&
      visible() &&
      !loading &&
      !(position.resume && page?.newerCursor)
    )
      load(before, true, query, position.resume ? {} : position).catch(fail);
  }, 15_000);
  view.dialog.addEventListener(
    'close',
    () => {
      controller.abort();
      clearAttachmentRows();
      disposeComposer?.();
      clearInterval(timer);
      removalListeners.delete(onRemoved);
      editListeners.delete(onEdited);
      stopInbox();
      stopReactions();
      stopUpdates();
    },
    { once: true },
  );
  load(
    null,
    false,
    '',
    options.initialMessageId ? { around: options.initialMessageId } : { resume: true },
  ).catch(fail);
}

export function openRoomHistory(
  room: RoomClient,
  current: () => boolean,
  signedIn: boolean,
  initialMessageId?: string,
  reply?: (message: ChatEntry) => void,
): void {
  openHistory({
    title: 'Room history',
    viewerId: room.localParticipantId ?? '',
    attachmentAuthorization: async (attachmentId) => {
      if (!current()) throw new Error('This conversation changed');
      const grant = await room.requestSocial('getAttachmentAccess', { attachmentId });
      return `Attachment ${grant.token}`;
    },
    ...(initialMessageId && { initialMessageId }),
    ...(reply && { reply }),
    react: (messageId, emoji) => room.requestSocial('reactToMessage', { messageId, emoji }),
    edit: async (message, content, expectedRevision) =>
      (
        await room.requestSocial('editChatMessage', {
          messageId: message.messageId,
          content,
          expectedRevision,
        })
      ).message,
    current,
    load: (params) =>
      room.requestSocial('getChatHistory', {
        ...(params.get('before') && { before: params.get('before')! }),
        ...(params.get('after') && { after: params.get('after')! }),
        ...(params.get('around') && { around: params.get('around')! }),
        ...(params.has('resume') && { resume: true }),
        ...(params.get('q') && { q: params.get('q')! }),
        limit: 50,
      }),
    ...(signedIn && {
      markRead: (messageId: string) => room.requestSocial('markChatRead', { messageId }),
    }),
    ...(['moderator', 'admin', 'owner'].includes(room.role) && {
      remove: (messageId: string) => room.requestSocial('removeChatMessage', { messageId }),
    }),
    ...(room.role === 'owner' && {
      retention: {
        days: room.roomSettings?.historyRetentionDays ?? 0,
        save: (retentionDays: number) => room.requestSocial('setRoomHistory', { retentionDays }),
      },
    }),
  });
}

export interface PrivateConversationOptions {
  getToken: () => string | null;
  getAccountId: () => string | null;
  getRoomId?: () => string | null;
  mountConversationNotifications?: (
    container: HTMLElement,
    peerId: string,
    current: () => boolean,
  ) => void;
  mountConversationPrivacy?: (
    container: HTMLElement,
    peerId: string,
    peerName: string,
    current: () => boolean,
  ) => void;
}

/** Account conversations use the same durable composer from every entry point. */
export function openPrivateConversation(
  peerId: string,
  peerName: string,
  options: PrivateConversationOptions,
): void {
  const account = options.getAccountId();
  if (!account || !options.getToken()) return;
  const token = (): string => {
    const value = options.getToken();
    if (!value || options.getAccountId() !== account)
      throw new Error('Sign in to read your messages');
    return value;
  };
  openHistory({
    title: `Messages with ${peerName}`,
    accountDialog: true,
    navigation: { accountId: account, destination: { kind: 'conversation', peerId, peerName } },
    viewerId: account,
    draft: { account, peer: peerId },
    privatePeerId: peerId,
    getToken: options.getToken,
    attachmentAuthorization: async () => `Bearer ${token()}`,
    mountNotifications: (container, current) => {
      options.mountConversationNotifications?.(container, peerId, current);
      options.mountConversationPrivacy?.(container, peerId, peerName, current);
    },
    edit: (message, content, expectedRevision, signal) =>
      api.editPrivateMessage(
        token(),
        peerId,
        message.messageId,
        { content, expectedRevision },
        signal,
      ),
    current: () => options.getAccountId() === account && !!options.getToken(),
    load: (params, signal) => api.privateHistory(token(), peerId, params, signal),
    markRead: (id, signal) => api.readPrivateMessages(token(), peerId, id, signal),
    readConfirmed: (cursor) => {
      for (const listener of privateReadListeners) listener(account, peerId, cursor);
    },
    react: (id, emoji, present, signal) =>
      api.setPrivateReaction(token(), peerId, id, { emoji, present }, signal),
    send: (content, id, signal, attachmentIds, replyTo) => {
      const roomId = options.getRoomId?.();
      return api.sendPrivateMessage(
        token(),
        peerId,
        {
          content,
          clientMessageId: id,
          ...(attachmentIds.length && { attachmentIds }),
          ...(replyTo && { replyTo }),
          ...(roomId && { roomId }),
        },
        signal,
      );
    },
  });
}

export function openPrivateInbox(options: PrivateConversationOptions): void {
  const account = options.getAccountId();
  if (!account || !options.getToken()) return;
  const view = modal('Messages', undefined, { accountId: account, destination: { kind: 'inbox' } });
  view.dialog.setAttribute('data-account-dialog', 'true');
  const controller = new AbortController();
  const current = (): boolean =>
    view.dialog.open && options.getAccountId() === account && !!options.getToken();
  const token = (): string => {
    const value = options.getToken();
    if (!value || options.getAccountId() !== account)
      throw new Error('Sign in to read your messages');
    return value;
  };
  const openConversation = (peerId: string, peerName: string): void =>
    openPrivateConversation(peerId, peerName, options);
  const contacts = mountContacts(view.body, {
    ...options,
    current,
    onMessage: openConversation,
  });
  const status = el('p', 'Loading conversations…', 'setting-hint');
  status.setAttribute('role', 'status');
  const list = el('div', undefined, 'inbox-conversations');
  let cursor: string | null = null;
  let next: string | null = null;
  let loading = false;
  let dirty = false;
  const visible = (): boolean => {
    const dialogs = document.querySelectorAll<HTMLDialogElement>('dialog[open]');
    return !document.hidden && dialogs[dialogs.length - 1] === view.dialog;
  };
  const fail = (error: unknown): void => {
    if (!current()) return;
    view.error.textContent = error instanceof Error ? error.message : 'Messages unavailable';
    view.error.hidden = false;
  };
  const load = async (before: string | null): Promise<void> => {
    if (!current() || loading) return;
    dirty = false;
    loading = true;
    older.disabled = true;
    newest.disabled = true;
    try {
      const params = new URLSearchParams();
      if (before) params.set('before', before);
      const page = await api.inbox(token(), params, controller.signal);
      if (!current()) return;
      cursor = before;
      next = page.nextCursor;
      view.error.hidden = true;
      status.textContent = `Account PMs are kept for ${page.retentionDays} days. Guest PMs stay in the room session.`;
      list.replaceChildren(
        ...page.conversations.map((conversation) => {
          const entry = button(
            '',
            () => {
              if (!current()) return;
              openConversation(conversation.peerId, conversation.peerName);
            },
            'inbox-conversation',
          );
          const label = conversation.unreadCount
            ? `${conversation.peerName} · ${conversation.unreadCount >= 1000 ? '1,000+' : conversation.unreadCount} unread`
            : conversation.peerName;
          entry.append(
            el('strong', label),
            el('span', conversation.lastMessage.content.slice(0, 140)),
            el('time', new Date(conversation.lastMessage.sentAt).toLocaleString()),
          );
          return entry;
        }),
      );
      if (!page.conversations.length)
        list.append(
          el(
            'p',
            'No saved conversations yet. Message an accepted contact or someone in your room.',
            'setting-hint',
          ),
        );
    } catch (error) {
      fail(error);
    } finally {
      loading = false;
      if (current()) {
        older.disabled = !next;
        newest.disabled = !cursor;
        if (dirty && visible()) load(cursor).catch(fail);
      }
    }
  };
  const older = button('Older conversations', () => {
    if (next) load(next).catch(fail);
  });
  const newest = button('Newest conversations', () => {
    load(null).catch(fail);
  });
  older.disabled = true;
  newest.disabled = true;
  const pages = el('div', undefined, 'history-pagination');
  pages.append(older, newest);
  view.body.append(status, list, pages);
  const refresh = (): void => {
    if (current() && visible() && dirty) load(cursor).catch(fail);
  };
  const stopUpdates = onAccountUpdate((update) => {
    if (update.account !== account || (update.scope !== 'ready' && update.scope !== 'inbox'))
      return;
    dirty = true;
    refresh();
  });
  window.addEventListener('focus', refresh, { signal: controller.signal });
  document.addEventListener('visibilitychange', refresh, { signal: controller.signal });
  document.addEventListener('close', () => queueMicrotask(refresh), {
    capture: true,
    signal: controller.signal,
  });
  const timer = setInterval(() => {
    if (!options.getToken() || options.getAccountId() !== account) {
      view.close();
      return;
    }
    refresh();
    if (visible() && contacts.needsRefresh?.()) contacts.refresh().catch(fail);
    if (visible() && (!view.error.hidden || !accountUpdatesLive(account))) {
      load(cursor).catch(fail);
      contacts.refresh().catch(fail);
    }
  }, 15_000);
  view.dialog.addEventListener(
    'close',
    () => {
      controller.abort();
      contacts.dispose();
      stopUpdates();
      clearInterval(timer);
    },
    { once: true },
  );
  load(null).catch(fail);
}
