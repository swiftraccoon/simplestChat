import type { ChatEntry, ChatHistoryPage } from './protocol';
import { SavedPmDrafts } from './chat-store';
import { mountContacts } from './discovery';
import { appendLinkedText, appendEditedLabel, editMessageDialog } from './chat-message-ui';
import { chatColor } from './avatar-colors';
import type { RoomClient } from './room';
import { api, asyncButton, button, el, field, input, modal } from './ui';

type HistoryOptions = {
  title: string;
  accountDialog?: boolean;
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
  load: (params: URLSearchParams, signal: AbortSignal) => Promise<ChatHistoryPage>;
  markRead?: (id: string, signal: AbortSignal) => Promise<unknown>;
  remove?: (id: string) => Promise<{ removedAt: string }>;
  send?: (content: string, id: string, signal: AbortSignal) => Promise<ChatEntry>;
  retention?: { days: number; save: (days: number) => Promise<unknown> };
};
const inboxListeners = new Set<() => void>();
export function onInboxChanged(listener: () => void): () => void {
  inboxListeners.add(listener);
  return () => {
    inboxListeners.delete(listener);
  };
}
export function notifyInboxChanged(): void {
  for (const listener of inboxListeners) listener();
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

/** A bounded page reader shared by room history and the account inbox. */
function openHistory(options: HistoryOptions): void {
  const view = modal(options.title);
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
  let read = '';
  const removed = new Map<string, string>();
  const edits = new Map<string, ChatEntry>();
  const redact = (entry: ChatEntry): void => {
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
    const latest = page.messages[page.messages.length - 1]?.messageId;
    if (!latest || latest === read) return;
    read = latest;
    options
      .markRead(latest, controller.signal)
      .then(() => {
        if (current()) notifyInboxChanged();
      })
      .catch((error: unknown) => {
        if (read === latest) read = '';
        fail(error);
      });
  };
  const render = (bottom: boolean): void => {
    const scroll = list.scrollTop;
    const nodes = (page?.messages ?? []).map((message) => {
      const row = messageNode(message);
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
      if (options.edit && message.participantId === options.viewerId && !message.removedAt) {
        row.append(
          button(
            'Edit',
            () =>
              editMessageDialog(message, {
                current,
                save: (content, revision) =>
                  options.edit!(message, content, revision, controller.signal),
                changed: notifyHistoryEdit,
              }),
            'history-edit',
          ),
        );
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
    try {
      const result = await options.load(params, controller.signal);
      if (!current() || version !== operation) return;
      page = result;
      page.messages.forEach(redact);
      if (requestedPosition.resume && !quiet) anchor = page.firstUnreadMessageId;
      position = requestedPosition;
      before = cursor;
      query = requestedQuery;
      if (!quiet) view.error.hidden = true;
      loading = false;
      render(bottom && !anchor);
    } catch (error) {
      if (version === operation) fail(error);
    } finally {
      if (version === operation) {
        loading = false;
        if (current()) updatePagination();
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
  if (options.send) {
    const composer = el('textarea');
    composer.rows = 3;
    composer.maxLength = 2000;
    composer.placeholder = 'Write a private message…';
    const drafts = new SavedPmDrafts(localStorage);
    if (options.draft) composer.value = drafts.get(options.draft.account, options.draft.peer);
    const saveDraft = (): void => {
      if (options.draft && options.current())
        drafts.save(options.draft.account, options.draft.peer, composer.value);
    };
    composer.addEventListener('input', saveDraft);
    view.dialog.addEventListener('close', saveDraft, { once: true });
    let attempt: { content: string; id: string } | null = null;
    const send = asyncButton(
      'Send',
      async () => {
        const content = composer.value.trim();
        if (!content || !current() || !options.send || send.disabled) return;
        if (!attempt || attempt.content !== content) attempt = { content, id: crypto.randomUUID() };
        const submitted = attempt;
        send.disabled = true;
        try {
          await options.send(content, submitted.id, controller.signal);
          if (!current()) return;
          if (composer.value.trim() === content) composer.value = '';
          saveDraft();
          notifyInboxChanged();
          attempt = null;
          search.value = '';
          await load(null, false, '');
        } finally {
          send.disabled = false;
        }
      },
      fail,
    );
    send.className = 'btn-secondary history-send';
    composer.addEventListener('keydown', (event) => {
      if (
        event.key === 'Enter' &&
        !event.isComposing &&
        (event.ctrlKey ||
          event.metaKey ||
          (!event.shiftKey && !window.matchMedia('(any-pointer: coarse)').matches))
      ) {
        event.preventDefault();
        send.click();
      }
    });
    view.body.append(field('Private message', composer), send);
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
  list.addEventListener('scroll', markRead);
  const timer = setInterval(() => {
    if (!options.current()) {
      view.close();
      return;
    }
    // Repeating resume after its cursor advances would silently skip to the next
    // unread page. Keep that page stable until the reader chooses Newer messages.
    if (visible() && !loading && !(position.resume && page?.newerCursor))
      load(before, true, query, position.resume ? {} : position).catch(fail);
  }, 15_000);
  view.dialog.addEventListener(
    'close',
    () => {
      controller.abort();
      clearInterval(timer);
      removalListeners.delete(onRemoved);
      editListeners.delete(onEdited);
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
): void {
  openHistory({
    title: 'Room history',
    viewerId: room.localParticipantId ?? '',
    ...(initialMessageId && { initialMessageId }),
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

export function openPrivateInbox(options: {
  getToken: () => string | null;
  getAccountId: () => string | null;
  mountConversationNotifications?: (
    container: HTMLElement,
    peerId: string,
    current: () => boolean,
  ) => void;
}): void {
  const account = options.getAccountId();
  if (!account || !options.getToken()) return;
  const view = modal('Messages');
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
  const openConversation = (peerId: string, peerName: string): void => {
    openHistory({
      title: `Messages with ${peerName}`,
      accountDialog: true,
      viewerId: account,
      draft: { account, peer: peerId },
      mountNotifications: (container, current) =>
        options.mountConversationNotifications?.(container, peerId, current),
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
      send: (content, id, signal) =>
        api.sendPrivateMessage(token(), peerId, { content, clientMessageId: id }, signal),
    });
  };
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
  const fail = (error: unknown): void => {
    if (!current()) return;
    view.error.textContent = error instanceof Error ? error.message : 'Messages unavailable';
    view.error.hidden = false;
  };
  const load = async (before: string | null): Promise<void> => {
    if (!current() || loading) return;
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
  const timer = setInterval(() => {
    if (!options.getToken() || options.getAccountId() !== account) {
      view.close();
      return;
    }
    if (!document.hidden) {
      load(cursor).catch(fail);
      contacts.refresh().catch(fail);
    }
  }, 15_000);
  view.dialog.addEventListener(
    'close',
    () => {
      controller.abort();
      contacts.dispose();
      clearInterval(timer);
    },
    { once: true },
  );
  load(null).catch(fail);
}
