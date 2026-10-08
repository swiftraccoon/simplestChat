import assert from 'node:assert/strict';
import test from 'node:test';
import { loadContractModules, loadTypeScript } from './source-loader.mjs';
import { deferred, flush, uiFixture } from './ui-fixture.mjs';

const message = (id, content = `Message ${id}`) => ({
  messageId: id,
  clientMessageId: `client-${id}`,
  participantId: 'peer',
  participantName: 'Peer',
  revision: 0,
  recipientId: 'account',
  recipientName: 'Account',
  content,
  sentAt: '2026-10-07T10:00:00Z',
  reactions: [],
});
const history = (messages = [message('newest')], nextCursor = null) => ({
  messages,
  nextCursor,
  readMessageId: null,
  newerCursor: null,
  firstUnreadMessageId: null,
  retentionDays: 30,
});
const inbox = (
  conversations = [
    { peerId: 'peer', peerName: 'Peer', lastMessage: message('newest'), unreadCount: 2 },
  ],
  nextCursor = null,
) => ({
  conversations,
  nextCursor,
  retentionDays: 30,
});

async function fixture(t) {
  const f = await uiFixture();
  const timers = new Map();
  let timerId = 0,
    attemptId = 0;
  const listeners = new Map();
  f.document.hidden = false;
  f.document.addEventListener = (name, listener) => {
    if (!listeners.has(name)) listeners.set(name, new Set());
    listeners.get(name).add(listener);
  };
  f.document.removeEventListener = (name, listener) => listeners.get(name)?.delete(listener);
  const storage = new Map();
  f.document.createTextNode = (text) => f.ui.el('text', text);
  Object.defineProperty(f.Node.prototype, 'style', {
    get() {
      return { setProperty() {} };
    },
  });
  const state = {
    account: 'account',
    token: 'token-a',
    current: true,
    calls: [],
    loadInbox: async () => inbox(),
    loadHistory: async () => history(),
    send: async (_token, _peer, data) => message(data.clientMessageId, data.content),
    markRead: async () => ({ readMessageId: 'newest' }),
    edit: async (_token, _peer, _id, data) => ({
      ...message(_id, data.content),
      revision: data.expectedRevision + 1,
      editedAt: '2026-10-08T00:00:00Z',
    }),
    room: async (method) =>
      method === 'getChatHistory'
        ? history()
        : method === 'removeChatMessage'
          ? { removedAt: '2026-10-07T11:00:00Z' }
          : {},
  };
  Object.defineProperties(f.Node.prototype, {
    scrollHeight: {
      get() {
        return this._scrollHeight ?? this.querySelectorAll('article').length * 100;
      },
    },
    clientHeight: {
      get() {
        return this._clientHeight ?? 200;
      },
    },
    scrollTop: {
      get() {
        return this._scrollTop ?? 0;
      },
      set(value) {
        this._scrollTop = Math.max(0, Math.min(value, this.scrollHeight - this.clientHeight));
      },
    },
  });
  const invoke =
    (method, handler) =>
    async (...args) => {
      state.calls.push({ method, args });
      return handler(...args);
    };
  const api = {
    inbox: invoke('inbox', (...args) => state.loadInbox(...args)),
    privateHistory: invoke('history', (...args) => state.loadHistory(...args)),
    readPrivateMessages: invoke('read', (...args) => state.markRead(...args)),
    sendPrivateMessage: invoke('send', (...args) => state.send(...args)),
    editPrivateMessage: invoke('edit', (...args) => state.edit(...args)),
  };
  const module = await loadTypeScript('src/chat-history.ts', {
    modules: {
      './ui': { ...f.ui, api },
      './chat-store': await loadTypeScript('src/chat-store.ts'),
      './avatar-colors': await loadTypeScript('src/avatar-colors.ts'),
      './chat-message-ui': await loadTypeScript('src/chat-message-ui.ts', {
        modules: { './ui': f.ui },
        globals: { document: f.document },
      }),
    },
    globals: {
      document: f.document,
      localStorage: {
        getItem: (key) => storage.get(key) ?? null,
        setItem: (key, value) => storage.set(key, value),
      },
      window: { matchMedia: () => ({ matches: false }) },
      crypto: { randomUUID: () => `attempt-${++attemptId}` },
      URLSearchParams,
      setInterval: (callback, ms) => {
        const id = ++timerId;
        timers.set(id, { callback, ms });
        return id;
      },
      clearInterval: (id) => timers.delete(id),
    },
  });
  const room = {
    role: 'owner',
    roomSettings: { historyRetentionDays: 30 },
    requestSocial: invoke('room', (...args) => state.room(...args)),
  };
  const tick = async () => {
    for (const { callback } of [...timers.values()]) callback();
    await flush();
  };
  const openInbox = async () => {
    module.openPrivateInbox({ getToken: () => state.token, getAccountId: () => state.account });
    await flush();
    return dialog(f, 'Messages');
  };
  const openConversation = async () => {
    const parent = await openInbox();
    parent.querySelector('.inbox-conversation').click();
    await flush();
    return dialog(f, 'Messages with Peer');
  };
  t.after(() => {
    for (const view of [...f.document.querySelectorAll('dialog')]) view.close();
    assert.equal(timers.size, 0);
  });
  return {
    ...f,
    ...module,
    state,
    room,
    timers,
    tick,
    openInbox,
    openConversation,
    visibility: () => {
      for (const listener of listeners.get('visibilitychange') ?? []) listener();
    },
  };
}
function dialog(f, name) {
  const found = f.document
    .querySelectorAll('dialog')
    .find((node) => node.children[0].children[0].textContent === name);
  assert.ok(found, `Expected ${name} dialog`);
  return found;
}
function action(view, label) {
  const found = view.querySelectorAll('button').find((node) => node.textContent === label);
  assert.ok(found, `Expected ${label} action`);
  return found;
}
function search(view, value) {
  view.querySelector('input').value = value;
  view.querySelector('form').emit('submit', { preventDefault() {} });
}
const rows = (view) =>
  view.querySelectorAll('article').map((node) => node.getAttribute('data-message-id'));
const calls = (f, method) => f.state.calls.filter((call) => call.method === method);

test('inbox and history preserve server ordering and page without accumulating unbounded DOM', async (t) => {
  const f = await fixture(t);
  const a = { peerId: 'peer', peerName: 'Peer', lastMessage: message('last'), unreadCount: 2 };
  f.state.loadInbox = async (_token, params) =>
    params.has('before')
      ? inbox([{ ...a, peerName: 'Older peer' }])
      : inbox([a], 'inbox cursor /?');
  const parent = await f.openInbox();
  action(parent, 'Older conversations').click();
  await flush();
  assert.equal(calls(f, 'inbox').at(-1).args[1].get('before'), 'inbox cursor /?');
  assert.equal(parent.querySelectorAll('.inbox-conversation').length, 1);
  assert.match(parent.textContent, /Older peer/);
  action(parent, 'Newest conversations').click();
  await flush();
  f.state.loadHistory = async (_token, _peer, params) =>
    params.has('before')
      ? history([message('older-1'), message('older-2')])
      : history([message('newer-1'), message('newer-2')], 'history cursor /?');
  parent.querySelector('.inbox-conversation').click();
  await flush();
  const view = dialog(f, 'Messages with Peer');
  assert.deepEqual(rows(view), ['newer-1', 'newer-2']);
  action(view, 'Older messages').click();
  await flush();
  assert.deepEqual(rows(view), ['older-1', 'older-2']);
  assert.equal(calls(f, 'history').at(-1).args[2].get('before'), 'history cursor /?');
  assert.equal(calls(f, 'read').length, 1, 'historical pages must not advance unread state');
  action(view, 'Newest messages').click();
  await flush();
  assert.deepEqual(rows(view), ['newer-1', 'newer-2']);
});

test('search validates length, resets the cursor and does not mark matching messages read', async (t) => {
  const f = await fixture(t);
  f.state.loadHistory = async (_token, _peer, params) =>
    params.has('q') ? history([message('match')]) : history([message('newest')], 'older');
  const view = await f.openConversation();
  action(view, 'Older messages').click();
  await flush();
  const count = calls(f, 'history').length;
  search(view, 'ab');
  await flush();
  assert.equal(calls(f, 'history').length, count);
  assert.match(view.textContent, /at least 3 characters/);
  search(view, '  a&b /?  ');
  await flush();
  const params = calls(f, 'history').at(-1).args[2];
  assert.equal(params.get('q'), 'a&b /?');
  assert.equal(params.has('before'), false);
  assert.deepEqual(rows(view), ['match']);
  assert.equal(calls(f, 'read').length, 1);
});

test('a failed search keeps the displayed page, cursor and polling query coherent', async (t) => {
  const f = await fixture(t);
  let fail = true;
  f.state.loadHistory = async (_token, _peer, params) => {
    if (params.has('q') && fail) throw new Error('Search temporarily unavailable');
    return params.has('before')
      ? history([message('older')], 'even-older')
      : history([message('newest')], 'older-cursor');
  };
  const view = await f.openConversation();
  action(view, 'Older messages').click();
  await flush();
  search(view, 'needle');
  await flush();
  assert.deepEqual(rows(view), ['older']);
  assert.match(view.textContent, /Search temporarily unavailable/);
  await f.tick();
  const params = calls(f, 'history').at(-1).args[2];
  assert.equal(params.get('before'), 'older-cursor');
  assert.equal(
    params.has('q'),
    false,
    'a failed search cannot contaminate the previous page cursor',
  );
  fail = false;
  search(view, 'needle');
  await flush();
  assert.equal(calls(f, 'history').at(-1).args[2].get('q'), 'needle');
  assert.equal(calls(f, 'history').at(-1).args[2].has('before'), false);
});

test('pagination stays disabled during a pending page and out-of-order search results are retired', async (t) => {
  const f = await fixture(t);
  const first = deferred(),
    second = deferred();
  f.state.loadHistory = async (_token, _peer, params) =>
    params.get('q') === 'first'
      ? first.promise
      : params.get('q') === 'second'
        ? second.promise
        : history([message('initial')], 'older');
  const view = await f.openConversation();
  search(view, 'first');
  assert.equal(action(view, 'Older messages').disabled, true);
  search(view, 'second');
  second.resolve(history([message('second')]));
  await flush();
  first.resolve(history([message('first')]));
  await flush();
  assert.deepEqual(rows(view), ['second']);
});

test('PMs remain unread while hidden, scrolled away, or covered by another modal', async (t) => {
  const f = await fixture(t);
  f.document.hidden = true;
  f.state.loadHistory = async () =>
    history(Array.from({ length: 5 }, (_, index) => message(`initial-${index}`)));
  const view = await f.openConversation();
  assert.equal(calls(f, 'read').length, 0);
  f.document.hidden = false;
  view.querySelector('.history-messages').scrollTop = 0;
  f.state.loadHistory = async () =>
    history(Array.from({ length: 6 }, (_, index) => message(`initial-${index}`)));
  await f.tick();
  assert.equal(
    calls(f, 'read').length,
    0,
    'background refresh must not read messages below the scroll position',
  );
  const list = view.querySelector('.history-messages');
  list.scrollTop = list.scrollHeight;
  list.emit('scroll');
  await flush();
  assert.equal(calls(f, 'read').length, 1);
  const overlay = f.ui.modal('Other modal');
  f.state.loadHistory = async () => history([message('new-under-overlay')]);
  await f.tick();
  assert.equal(calls(f, 'read').length, 1, 'a covered conversation is not visibly read');
  overlay.close();
  await f.tick();
  assert.equal(calls(f, 'read').at(-1).args[2], 'new-under-overlay');
});

test('disabled room history never sends a persistent read marker', async (t) => {
  const f = await fixture(t);
  f.state.room = async () => ({ ...history([message('ephemeral')]), retentionDays: 0 });
  f.openRoomHistory(f.room, () => f.state.current, true);
  await flush();
  assert.equal(calls(f, 'room').filter((call) => call.args[0] === 'markChatRead').length, 0);
});

test('closing a history view aborts and fences late messages, reads and errors', async (t) => {
  const f = await fixture(t);
  const pending = deferred();
  f.state.loadHistory = () => pending.promise;
  const view = await f.openConversation();
  const request = calls(f, 'history')[0];
  action(view, 'Close').click();
  assert.equal(request.args[3].aborted, true);
  pending.resolve(history([message('private-late', 'Never render this private text')]));
  await flush();
  assert.deepEqual(rows(view), []);
  assert.equal(calls(f, 'read').length, 0);
  assert.doesNotMatch(view.textContent, /Never render/);
});

test('late inbox responses cannot expose conversations after close or an account change', async (t) => {
  for (const outcome of ['close', 'account', 'error']) {
    const f = await fixture(t);
    const pending = deferred();
    f.state.loadInbox = () => pending.promise;
    const parent = await f.openInbox();
    const request = calls(f, 'inbox')[0];
    if (outcome === 'account') f.state.account = 'replacement';
    else action(parent, 'Close').click();
    if (outcome === 'error') pending.reject(new Error('Retired private error'));
    else pending.resolve(inbox());
    await flush();
    assert.equal(parent.querySelectorAll('.inbox-conversation').length, 0);
    assert.doesNotMatch(parent.textContent, /Retired private error|Peer/);
    if (outcome !== 'account') assert.equal(request.args[2].aborted, true);
    await f.tick();
    assert.equal(parent.open, false);
  }
});

test('inbox buttons stay disabled until a page finishes and detached entries cannot reopen PMs', async (t) => {
  const f = await fixture(t);
  const pending = deferred();
  const initial = inbox(inbox().conversations, 'older');
  f.state.loadInbox = async (_token, params) => (params.has('before') ? pending.promise : initial);
  const parent = await f.openInbox();
  const entry = parent.querySelector('.inbox-conversation');
  action(parent, 'Older conversations').click();
  assert.equal(action(parent, 'Older conversations').disabled, true);
  assert.equal(action(parent, 'Newest conversations').disabled, true);
  action(parent, 'Close').click();
  entry.click();
  assert.equal(f.document.querySelectorAll('dialog').length, 0);
  pending.resolve(inbox());
  await flush();
});

test('account replacement fences both inbox and conversation responses and closes their polling', async (t) => {
  const f = await fixture(t);
  const pending = deferred();
  const parent = await f.openInbox();
  f.state.loadHistory = () => pending.promise;
  parent.querySelector('.inbox-conversation').click();
  await flush();
  const view = dialog(f, 'Messages with Peer');
  f.state.account = 'replacement';
  f.state.token = 'replacement-token';
  pending.resolve(history([message('stale')]));
  await flush();
  assert.deepEqual(rows(view), []);
  assert.equal(calls(f, 'read').length, 0);
  await f.tick();
  assert.equal(parent.open, false);
  assert.equal(view.open, false);
  assert.equal(f.timers.size, 0);
});

test('PM send retries reuse the id after an uncertain response and preserve edits made during send', async (t) => {
  const f = await fixture(t);
  const view = await f.openConversation();
  const composer = view.querySelector('textarea');
  composer.value = '  First message  ';
  f.state.send = async () => {
    throw new Error('Response lost');
  };
  action(view, 'Send').click();
  await flush();
  assert.equal(composer.value, '  First message  ');
  const pending = deferred();
  f.state.send = () => pending.promise;
  f.state.token = 'refreshed-token';
  action(view, 'Send').click();
  action(view, 'Send').click();
  composer.value = 'Draft for later';
  pending.resolve(message('confirmed'));
  await flush();
  assert.equal(calls(f, 'send').length, 2);
  assert.equal(
    calls(f, 'send')[0].args[2].clientMessageId,
    calls(f, 'send')[1].args[2].clientMessageId,
  );
  assert.equal(calls(f, 'send')[1].args[0], 'refreshed-token');
  assert.equal(calls(f, 'send')[1].args[2].content, 'First message');
  assert.equal(composer.value, 'Draft for later');
  f.state.send = async () => message('second');
  action(view, 'Send').click();
  await flush();
  assert.notEqual(
    calls(f, 'send')[2].args[2].clientMessageId,
    calls(f, 'send')[1].args[2].clientMessageId,
  );
  assert.equal(composer.value, '');
});

test('room removals redact the body, replies and late initial or refreshed pages', async (t) => {
  const f = await fixture(t);
  const pending = deferred();
  const quoted = {
    ...message('reply'),
    replyTo: {
      messageId: 'original',
      participantId: 'peer',
      participantName: 'Peer',
      excerpt: 'Removed quote secret',
    },
  };
  const source = history([message('original', 'Removed body secret'), quoted]);
  f.state.room = async (method) => (method === 'getChatHistory' ? pending.promise : {});
  f.openRoomHistory(f.room, () => f.state.current, true);
  const view = dialog(f, 'Room history');
  f.notifyHistoryRemoval('original', '2026-10-07T11:00:00Z');
  pending.resolve(structuredClone(source));
  await flush();
  assert.doesNotMatch(view.textContent, /Removed body secret|Removed quote secret/);
  assert.match(view.textContent, /Message removed/);
  assert.equal(
    view.querySelectorAll('button').filter((node) => node.textContent === 'Remove').length,
    1,
  );
  f.state.room = async (method) => (method === 'getChatHistory' ? structuredClone(source) : {});
  await f.tick();
  assert.doesNotMatch(view.textContent, /Removed body secret|Removed quote secret/);
  f.notifyHistoryRemoval('reply', '2026-10-07T12:00:00Z');
  assert.equal(view.querySelectorAll('blockquote').length, 0);
  assert.equal(
    view.querySelectorAll('button').filter((node) => node.textContent === 'Remove').length,
    0,
  );
});

test('room moderation waits for confirmation and retention controls remain owner-only', async (t) => {
  const f = await fixture(t);
  f.openRoomHistory(f.room, () => f.state.current, true);
  await flush();
  const view = dialog(f, 'Room history');
  action(view, 'Remove').click();
  assert.equal(calls(f, 'room').filter((call) => call.args[0] === 'removeChatMessage').length, 0);
  action(dialog(f, 'Remove message'), 'Remove message').click();
  await flush();
  assert.equal(calls(f, 'room').filter((call) => call.args[0] === 'removeChatMessage').length, 1);
  assert.match(view.textContent, /Message removed/);
  view.querySelector('select').value = '7';
  action(view, 'Save retention').click();
  await flush();
  assert.deepEqual(calls(f, 'room').find((call) => call.args[0] === 'setRoomHistory').args[1], {
    retentionDays: 7,
  });
  view.close();
  f.room.role = 'moderator';
  f.openRoomHistory(f.room, () => f.state.current, true);
  await flush();
  assert.equal(dialog(f, 'Room history').querySelector('select'), null);
});

test('history HTTP APIs encode peer/cursor/search independently and validate every response', async () => {
  const f = await uiFixture();
  const params = new URLSearchParams({ before: 'cursor /?&', q: 'name & text', limit: '50' });
  const signal = new AbortController().signal;
  for (const [invoke, path, method, response, body] of [
    [
      () => f.ui.api.inboxUnread('token', signal),
      '/api/auth/inbox/unread',
      'GET',
      { unreadCount: 8 },
    ],
    [
      () =>
        f.ui.api.editPrivateMessage(
          'token',
          'peer /?',
          'message /?',
          { content: 'Corrected', expectedRevision: 2 },
          signal,
        ),
      '/api/auth/inbox/peer%20%2F%3F/messages/message%20%2F%3F',
      'PUT',
      { ...message('edited', 'Corrected'), revision: 3, editedAt: '2026-10-08T00:00:00Z' },
      { content: 'Corrected', expectedRevision: 2 },
    ],
    [() => f.ui.api.inbox('token', params, signal), `/api/auth/inbox?${params}`, 'GET', inbox()],
    [
      () => f.ui.api.privateHistory('token', 'peer /?', params, signal),
      `/api/auth/inbox/peer%20%2F%3F/messages?${params}`,
      'GET',
      history(),
    ],
    [
      () =>
        f.ui.api.sendPrivateMessage(
          'token',
          'peer /?',
          { clientMessageId: 'retry-id', content: 'Message' },
          signal,
        ),
      '/api/auth/inbox/peer%20%2F%3F/messages',
      'POST',
      message('sent'),
      { clientMessageId: 'retry-id', content: 'Message' },
    ],
    [
      () => f.ui.api.readPrivateMessages('token', 'peer /?', 'read-id', signal),
      '/api/auth/inbox/peer%20%2F%3F/read',
      'PUT',
      { readMessageId: 'read-id' },
      { messageId: 'read-id' },
    ],
  ]) {
    f.state.response = { ok: true, status: 200, json: async () => response };
    assert.deepEqual(await invoke(), response);
    const [actualPath, options] = f.state.requests.at(-1);
    assert.equal(actualPath, path);
    assert.equal(options.method, method);
    assert.equal(options.headers.Authorization, 'Bearer token');
    assert.equal(options.credentials, 'same-origin');
    assert.ok(options.signal instanceof AbortSignal);
    if (body) assert.deepEqual(JSON.parse(options.body), body);
    f.state.response = {
      ok: true,
      status: 200,
      json: async () => ({ privateField: 'invalid server detail' }),
    };
    await assert.rejects(invoke(), method === 'GET' ? /invalid data/ : f.ui.ApiOutcomeUnknownError);
  }
});

test('inbox/history/read decoders bound result counts and reject malformed required data', async () => {
  const modules = await loadContractModules();
  const { decodeInbox, decodeChatRead } = modules['./chat-history-validation'];
  const { decodeChatHistory } = modules['./protocol-validation'];
  for (const decode of [decodeInbox, decodeChatHistory, decodeChatRead]) {
    for (const invalid of [null, false, [], {}, 'untrusted']) assert.throws(() => decode(invalid));
  }
  const conversation = inbox().conversations[0];
  assert.equal(
    decodeInbox(inbox(Array.from({ length: 100 }, () => conversation))).conversations.length,
    100,
  );
  assert.throws(() => decodeInbox(inbox(Array.from({ length: 101 }, () => conversation))));
  assert.equal(
    decodeChatHistory(history(Array.from({ length: 100 }, () => message('id')))).messages.length,
    100,
  );
  assert.throws(() => decodeChatHistory(history(Array.from({ length: 101 }, () => message('id')))));
  for (const invalid of [-1, 0.5, NaN, Number.MAX_SAFE_INTEGER + 1, '2'])
    assert.throws(() => decodeInbox(inbox([{ ...conversation, unreadCount: invalid }])));
  for (const invalid of [-1, 2, 91, '30', null]) {
    assert.throws(() => decodeInbox({ ...inbox(), retentionDays: invalid }));
    assert.throws(() => decodeChatHistory({ ...history(), retentionDays: invalid }));
  }
  assert.throws(() => decodeInbox({ ...inbox(), nextCursor: 7 }));
  assert.throws(() => decodeChatHistory(history([{ ...message('bad'), participantName: 4 }])));
  assert.throws(() => decodeChatRead({ readMessageId: false }));
  assert.deepEqual(decodeChatRead({ readMessageId: null, privateField: 'discarded' }), {
    readMessageId: null,
  });
});

test('search opens the result with surrounding context and newer pages without marking search hits read', async (t) => {
  const f = await fixture(t);
  f.state.loadHistory = async (_token, _peer, params) =>
    params.has('around')
      ? {
          ...history([message('before'), message('match'), message('after')]),
          newerCursor: 'newer-page',
        }
      : params.has('q')
        ? history([message('match')])
        : history();
  const view = await f.openConversation();
  search(view, 'match');
  await flush();
  action(view, 'Show in conversation').click();
  await flush();
  assert.equal(calls(f, 'history').at(-1).args[2].get('around'), 'match');
  assert.deepEqual(rows(view), ['before', 'match', 'after']);
  assert.equal(calls(f, 'read').length, 1);
  action(view, 'Newer messages').click();
  await flush();
  assert.equal(calls(f, 'history').at(-1).args[2].get('after'), 'newer-page');
});

test('reopening an inbox conversation keeps its unsent draft separate from another account', async (t) => {
  const f = await fixture(t);
  let view = await f.openConversation();
  view.querySelector('textarea').value = 'Finish this later';
  view.querySelector('textarea').emit('input');
  action(view, 'Close').click();
  view = await f.openConversation();
  assert.equal(view.querySelector('textarea').value, 'Finish this later');
  action(view, 'Close').click();
  for (const dialog of [...f.document.querySelectorAll('dialog')]) dialog.close();
  f.state.account = 'another-account';
  view = await f.openConversation();
  assert.equal(view.querySelector('textarea').value, '');
});

test('resume requests the first unread context and does not mark messages beneath the viewport read', async (t) => {
  const f = await fixture(t);
  f.state.loadHistory = async () => ({
    ...history(Array.from({ length: 8 }, (_, id) => message(`resume-${id}`))),
    firstUnreadMessageId: 'resume-2',
    newerCursor: 'more',
  });
  const view = await f.openConversation();
  assert.equal(calls(f, 'history')[0].args[2].get('resume'), 'true');
  assert.match(view.textContent, /New messages/);
  assert.equal(calls(f, 'read').length, 0);
  const list = view.querySelector('.history-messages');
  list.scrollTop = list.scrollHeight;
  list.emit('scroll');
  await flush();
  assert.equal(calls(f, 'read').at(-1).args[2], 'resume-7');
  const loads = calls(f, 'history').length;
  await f.tick();
  assert.equal(
    calls(f, 'history').length,
    loads,
    'polling must not resume again past an unread page the reader has not opened',
  );
});

test('history edits update loaded text and quotes and defeat an older in-flight page', async (t) => {
  const f = await fixture(t);
  const original = message('editable', 'Original');
  const reply = {
    ...message('quote'),
    replyTo: {
      messageId: 'editable',
      participantId: 'peer',
      participantName: 'Peer',
      excerpt: 'Original',
    },
  };
  f.state.loadHistory = async () => history([structuredClone(original), structuredClone(reply)]);
  const view = await f.openConversation();
  const pending = deferred();
  f.state.loadHistory = () => pending.promise;
  const refresh = f.tick();
  f.notifyHistoryEdit({
    ...original,
    content: 'Corrected',
    revision: 1,
    editedAt: '2026-10-08T00:00:00Z',
  });
  pending.resolve(history([structuredClone(original), structuredClone(reply)]));
  await refresh;
  await flush();
  assert.doesNotMatch(view.textContent, /Original/);
  assert.match(view.textContent, /Corrected.*Corrected/s);
  assert.match(view.textContent, /edited/);
});
