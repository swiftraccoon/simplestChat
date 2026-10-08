import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
import { deferred, flush, uiFixture } from './ui-fixture.mjs';

const chatModule = await loadTypeScript('src/chat-store.ts');
const colorModule = await loadTypeScript('src/avatar-colors.ts');
const { CHAT_PALETTE, chatColor } = colorModule;
const entry = (id, extra = {}) => ({
  messageId: `server-${id}`,
  clientMessageId: `client-${id}`,
  participantId: 'alice',
  participantName: 'Alice',
  content: `Message ${id}`,
  sentAt: '2026-01-01T00:00:00Z',
  ...extra,
});

async function fixture({ touch = false, touchPoints = 0 } = {}) {
  const f = await uiFixture();
  Object.defineProperties(f.Node.prototype, {
    options: {
      get() {
        return this.children;
      },
    },
    dataset: {
      get() {
        return (this._dataset ??= {});
      },
    },
    style: {
      get() {
        const properties = (this._style ??= new Map());
        return {
          setProperty: (name, value) => properties.set(name, value),
          getPropertyValue: (name) => properties.get(name) ?? '',
        };
      },
    },
    classList: {
      get() {
        const node = this;
        return {
          contains: (value) => (node.className ?? '').split(/\s+/).includes(value),
          add(...values) {
            for (const value of values) this.toggle(value, true);
          },
          toggle(value, enabled) {
            const classes = new Set((node.className ?? '').split(/\s+/).filter(Boolean));
            if (enabled ?? !classes.has(value)) classes.add(value);
            else classes.delete(value);
            node.className = [...classes].join(' ');
          },
        };
      },
    },
  });
  f.Node.prototype.prepend = function (...nodes) {
    const previous = [...this.children];
    this.replaceChildren(...nodes, ...previous);
  };
  f.Node.prototype.before = function (...nodes) {
    this.parentNode?.append(...nodes);
  };
  f.Node.prototype.focus = function () {
    f.document.activeElement = this;
  };
  f.Node.prototype.setRangeText = function (text, start, end) {
    this.value = this.value.slice(0, start) + text + this.value.slice(end);
    this.selectionStart = this.selectionEnd = start + text.length;
  };
  f.Node.prototype.scrollTop = 0;
  f.Node.prototype.scrollHeight = 100;
  f.Node.prototype.clientHeight = 100;
  const ids = [
    'chat-messages',
    'chat-input',
    'chat-send-btn',
    'chat-panel',
    'chat-input-row',
    'scroll-bottom-btn',
    'unread-badge',
    'room-screen',
  ];
  for (const id of ids) {
    const node = f.ui.el(id === 'chat-input' ? 'textarea' : 'div');
    node.id = id;
    f.document.body.append(node);
  }
  const input = f.document.getElementById('chat-input');
  input.clientWidth = 320;
  Object.defineProperties(input, {
    scrollHeight: {
      get: () => Math.max(36, input.value.split('\n').length * 20 + 16),
    },
    offsetHeight: {
      get: () =>
        Math.min(
          138,
          Math.max(38, Number.parseFloat(input.style.getPropertyValue('height')) || 38),
        ),
    },
    clientHeight: { get: () => input.offsetHeight - 2 },
  });
  Object.defineProperty(f.document.getElementById('chat-input-row'), 'offsetHeight', {
    get: () => input.offsetHeight + 16,
  });
  f.document.getElementById('chat-panel').className = 'active';
  const tab = f.ui.button('Chat', () => {});
  f.document.body.append(tab);
  const query = f.document.querySelector;
  f.document.querySelector = (selector) =>
    selector === '[data-tab="chat"]' ? tab : query(selector);
  f.document.createTextNode = (text) => f.ui.el('text', text);
  f.document.hidden = false;
  const documentListeners = new Map();
  f.document.addEventListener = (name, handler) => documentListeners.set(name, handler);
  // The UI fixture's fetch stub keeps its own record; the chat state shadows it below.
  const http = f.state;
  const state = {
    viewer: 'account-a',
    token: null,
    typing: [],
    requests: [],
    sent: [],
    notifications: [],
    actions: [],
    storage: new Map(),
    handle: async () => ({}),
    audioCreates: 0,
    audioResumes: 0,
    audioReject: false,
    notices: [],
    noticeAnswer: 'granted',
    pointerChange: () => {},
    resize: () => {},
  };
  const pointer = {
    matches: touch,
    addEventListener: (_name, handler) => {
      state.pointerChange = handler;
    },
  };
  class Notification {
    static permission = 'default';
    static requestPermission() {
      Notification.permission = state.noticeAnswer;
      return Promise.resolve(Notification.permission);
    }
    constructor(title, options) {
      Object.assign(this, { title, ...options, closed: false });
      state.notices.push(this);
    }
    close() {
      this.closed = true;
    }
  }
  const participants = new Map([['alice', { id: 'alice', name: 'Alice' }]]);
  state.room = {
    localParticipantId: 'local',
    currentRoomId: 'room',
    membershipVersion: 1,
    connected: true,
    canChat: true,
    nickname: 'Local',
    chatStyle: null,
    async setChatStyle(chatStyle) {
      const applied = (await this.requestSocial('setChatStyle', { chatStyle }))?.chatStyle;
      this.chatStyle = applied ?? chatStyle;
      return this.chatStyle;
    },
    getParticipants: () => participants,
    requestSocial(action, data) {
      state.requests.push({ action, data });
      return state.handle(action, data);
    },
    sendChat(...args) {
      state.sent.push(['public', ...args]);
    },
    sendPrivate(...args) {
      state.sent.push(['private', ...args]);
    },
    retryChat(message) {
      state.sent.push(['retry', message]);
    },
    sendTyping(target) {
      state.typing.push(target ?? 'public');
    },
  };
  class AudioContext {
    state = 'suspended';
    constructor() {
      state.audioCreates++;
    }
    resume() {
      state.audioResumes++;
      if (state.audioReject) return Promise.reject(new Error('Audio denied'));
      this.state = 'running';
      return Promise.resolve();
    }
    close() {
      this.state = 'closed';
      return Promise.resolve();
    }
    createOscillator() {
      throw new Error('Output device unavailable');
    }
  }
  let messageId = 0;
  const timers = new Map();
  const { SocialChat } = await loadTypeScript('src/social-chat.ts', {
    modules: {
      './chat-store': chatModule,
      './ui': f.ui,
      './avatar-colors': colorModule,
      './chat-history': { openRoomHistory() {}, notifyHistoryRemoval() {} },
    },
    globals: {
      document: f.document,
      window: { matchMedia: () => pointer },
      navigator: { maxTouchPoints: touchPoints },
      ResizeObserver: class {
        constructor(callback) {
          state.resize = callback;
        }
        observe() {}
      },
      TextEncoder,
      structuredClone,
      AudioContext,
      Notification,
      crypto: { randomUUID: () => `generated-${++messageId}` },
      localStorage: {
        getItem: (key) => state.storage.get(key) ?? null,
        setItem: (key, value) => state.storage.set(key, value),
      },
      setTimeout: (callback) => {
        const id = timers.size + 1;
        timers.set(id, callback);
        return id;
      },
      clearTimeout: (id) => timers.delete(id),
    },
  });
  const chat = new SocialChat({
    getRoom: () => state.room,
    getViewerKey: () => state.viewer,
    getToken: () => state.token,
    notify: (text) => state.notifications.push(text),
    bindParticipantName(anchor, id, name) {
      anchor.addEventListener('click', () => state.actions.push([id, name]));
    },
  });
  return { ...f, state, http, chat, tab, participants, documentListeners, timers, pointer };
}

test('reading retained public chat advances a saved cursor once and cancels stale work', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.state.token = 'account-token';
  f.state.room.roomSettings = { historyRetentionDays: 7 };
  f.chat.handleEvent(snapshot([entry('saved')]));
  assert.equal(f.timers.size, 1);
  const [read] = f.timers.values();
  f.timers.clear();
  read();
  await flush();
  assert.deepEqual(
    f.state.requests.filter((request) => request.action === 'markChatRead'),
    [{ action: 'markChatRead', data: { messageId: 'server-saved' } }],
  );
  f.chat.render();
  for (const timer of f.timers.values()) timer();
  assert.equal(f.state.requests.filter((request) => request.action === 'markChatRead').length, 1);
  f.chat.handleEvent(snapshot([entry('later')]));
  const stale = [...f.timers.values()];
  f.chat.reset();
  for (const timer of stale) timer();
  assert.equal(f.state.requests.filter((request) => request.action === 'markChatRead').length, 1);
});

test('saved read markers wait for an uncovered visible conversation', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.state.token = 'account-token';
  f.state.room.roomSettings = { historyRetentionDays: 7 };
  const dialog = f.ui.modal('Settings');
  f.chat.handleEvent(snapshot([entry('unseen')]));
  assert.equal(f.timers.size, 0);
  dialog.close();
  f.chat.render();
  assert.equal(f.timers.size, 1);
  f.document.hidden = true;
  for (const timer of f.timers.values()) timer();
  assert.equal(f.state.requests.filter((request) => request.action === 'markChatRead').length, 0);
  f.chat.reset();
});

function snapshot(messages) {
  return { type: 'socialResponse', action: 'getRoomSnapshot', data: { messages } };
}

function observeRows(f) {
  const changes = { renders: 0, insertions: 0 };
  const render = f.chat.render.bind(f.chat);
  f.chat.render = (...args) => {
    changes.renders++;
    return render(...args);
  };
  const insertBefore = f.chat.messages.insertBefore.bind(f.chat.messages);
  f.chat.messages.insertBefore = (...args) => {
    changes.insertions++;
    return insertBefore(...args);
  };
  f.chat.messages.replaceChildren = () => {
    throw new Error('Message reconciliation must not replace all rows');
  };
  f.chat.messages.append = () => {
    throw new Error('Message reconciliation must not reappend all rows');
  };
  return changes;
}

const rows = (f) => [...f.chat.messages.children];
const contents = (f) => rows(f).map((node) => node.querySelector('.msg-text').textContent);
const createdRows = (f) => f.created.filter((node) => node.classList.contains('chat-msg'));

function retrySession(f) {
  f.chat.handleEvent({
    ...snapshot([]),
    data: { messages: [], chatSessionId: 'c7c476a0-ea20-4357-b48a-23001edc0a03' },
  });
}

function expireSend(f) {
  const [id, callback] = f.timers.entries().next().value;
  f.timers.delete(id);
  callback();
}

test('an unconfirmed public send retries the same identity and reconciles a late ACK once', async () => {
  const f = await fixture();
  await f.chat.activate();
  retrySession(f);
  f.chat.input.value = 'original text';
  f.chat.send();
  const original = f.chat.store.messages[0];
  expireSend(f);
  assert.equal(original.status, 'unknown');
  assert.match(rows(f)[0].textContent, /Delivery not confirmed/);
  f.chat.input.value = 'new unsent draft';
  rows(f)[0]
    .querySelectorAll('button')
    .find((node) => node.textContent === 'Retry same message')
    .click();
  assert.equal(original.status, 'pending');
  assert.deepEqual(f.state.sent.at(-1), [
    'retry',
    {
      clientMessageId: original.clientMessageId,
      sequence: 1,
      chatSessionId: 'c7c476a0-ea20-4357-b48a-23001edc0a03',
      content: 'original text',
    },
  ]);
  assert.equal(f.chat.input.value, 'new unsent draft');
  f.chat.handleEvent({
    type: 'messageAck',
    message: { ...original, messageId: 'server-confirmed' },
  });
  assert.equal(original.status, 'sent');
  assert.equal(f.chat.store.messages.length, 1);
  assert.equal(f.timers.size, 0);
  f.chat.handleEvent({
    type: 'messageRetryResult',
    clientMessageId: original.clientMessageId,
    outcome: 'unknown',
    reason: 'receipt_expired',
  });
  assert.equal(original.status, 'sent', 'late uncertainty cannot downgrade confirmation');
});

test('an uncertain storage commit stays unknown and directs the sender to saved history', async () => {
  const f = await fixture();
  await f.chat.activate();
  retrySession(f);
  f.chat.input.value = 'durable original';
  f.chat.send();
  const original = f.chat.store.messages[0];
  f.chat.handleEvent({
    type: 'messageRetryResult',
    clientMessageId: original.clientMessageId,
    outcome: 'unknown',
    reason: 'storage_unconfirmed',
  });
  assert.equal(original.status, 'unknown');
  assert.equal(original.retry, undefined);
  assert.match(original.error, /Check saved history/);
  assert.equal(f.chat.store.messages.length, 1);
  f.chat.reset();
});

test('private retry retains original recipient even when another conversation is active', async () => {
  const f = await fixture();
  await f.chat.activate();
  retrySession(f);
  f.chat.openPrivate('alice', 'Alice');
  f.chat.input.value = 'private original';
  f.chat.send();
  const original = f.chat.store.messages[0];
  expireSend(f);
  f.chat.switchConversation('public');
  f.chat.input.value = 'public draft';
  f.chat.retry(original);
  assert.equal(f.state.sent.at(-1)[1].targetParticipantId, 'alice');
  assert.equal(f.state.sent.at(-1)[1].clientMessageId, original.clientMessageId);
  assert.equal(f.chat.input.value, 'public draft');
  f.chat.handleEvent({
    type: 'messageRetryResult',
    clientMessageId: original.clientMessageId,
    outcome: 'unknown',
    reason: 'recipient_unconfirmed',
  });
  assert.equal(original.status, 'unknown');
  assert.equal(original.retry, undefined);
  assert.match(original.error, /recipient session/);
  f.chat.reset();
});

for (const boundary of ['expiry', 'membership', 'snapshot-session', 'identity', 'reset']) {
  test(`uncertain send cannot retry across ${boundary}`, async () => {
    const f = await fixture();
    await f.chat.activate();
    retrySession(f);
    f.chat.input.value = 'old original';
    f.chat.send();
    const original = f.chat.store.messages[0];
    expireSend(f);
    if (boundary === 'expiry') original.retry.expiresAt = 0;
    if (boundary === 'membership') f.state.room.membershipVersion++;
    if (boundary === 'snapshot-session')
      f.chat.handleEvent({
        ...snapshot([]),
        data: { messages: [], chatSessionId: 'af77d00d-c653-4b54-a253-cd830e435f11' },
      });
    if (boundary === 'identity') f.state.viewer = 'another-viewer';
    if (boundary === 'reset') f.chat.reset();
    f.chat.retry(original);
    assert.equal(f.state.sent.length, 1);
    assert.equal(f.timers.size, 0);
  });
}

test('grace reconnect keeps retry identity but explicit membership activation retires it', async () => {
  const f = await fixture();
  await f.chat.activate();
  retrySession(f);
  f.chat.input.value = 'original';
  f.chat.send();
  const original = f.chat.store.messages[0];
  expireSend(f);
  f.state.room.connected = false;
  f.chat.retry(original);
  assert.equal(f.state.sent.length, 1);
  f.state.room.connected = true;
  f.chat.retry(original);
  assert.equal(f.state.sent.length, 2);
  f.state.room.membershipVersion++;
  await f.chat.activate();
  assert.equal(original.status, 'unknown');
  assert.equal(original.retry, undefined);
  assert.equal(f.timers.size, 0);
});

test('retry rejection stays unconfirmed and does not overwrite the next draft', async () => {
  const f = await fixture();
  await f.chat.activate();
  retrySession(f);
  f.chat.input.value = 'original';
  f.chat.send();
  const original = f.chat.store.messages[0];
  expireSend(f);
  f.chat.input.value = 'new draft';
  f.chat.retry(original);
  f.chat.retry(original);
  assert.equal(f.state.sent.length, 2, 'only one retry may be pending');
  f.chat.handleEvent({
    type: 'socialError',
    clientMessageId: original.clientMessageId,
    message: 'Room unavailable',
  });
  assert.equal(
    original.status,
    'unknown',
    'rejecting the check cannot prove the original was unsent',
  );
  assert.equal(f.chat.input.value, 'new draft');
  assert.equal(f.timers.size, 0);
  f.chat.reset();
});

test('a rejection arriving after a retry timeout cannot mark the original as unsent', async () => {
  const f = await fixture();
  await f.chat.activate();
  retrySession(f);
  f.chat.input.value = 'original';
  f.chat.send();
  const original = f.chat.store.messages[0];
  const retryIdentity = original.retry;
  expireSend(f);
  f.chat.retry(original);
  expireSend(f);
  f.chat.input.value = 'next draft';
  f.chat.handleEvent({
    type: 'socialError',
    clientMessageId: original.clientMessageId,
    message: 'Room unavailable',
  });
  assert.equal(original.status, 'unknown');
  assert.equal(original.retry, retryIdentity);
  assert.match(original.error, /Delivery not confirmed/);
  assert.equal(f.chat.input.value, 'next draft');
  assert.equal(f.state.sent.length, 2, 'no automatic resend');
  assert.equal(f.timers.size, 0);
  f.chat.reset();
});

test('a full receipt window allows later same-ID checking without automatic retry', async () => {
  const f = await fixture();
  await f.chat.activate();
  retrySession(f);
  f.chat.input.value = 'original';
  f.chat.send();
  const original = f.chat.store.messages[0];
  expireSend(f);
  f.chat.retry(original);
  f.chat.handleEvent({
    type: 'messageRetryResult',
    clientMessageId: original.clientMessageId,
    outcome: 'unknown',
    reason: 'capacity',
  });
  assert.equal(original.status, 'unknown');
  assert.ok(original.retry);
  assert.equal(f.timers.size, 0);
  assert.equal(f.state.sent.length, 2);
  f.chat.retry(original);
  assert.equal(f.state.sent.length, 3);
  assert.equal(f.state.sent.at(-1)[1].clientMessageId, original.clientMessageId);
  f.chat.reset();
});

test('a full snapshot renders once and unchanged replay performs no row allocation or insertion', async () => {
  const f = await fixture();
  await f.chat.activate();
  const changes = observeRows(f);
  const history = Array.from({ length: 300 }, (_, index) => entry(index));
  f.chat.handleEvent(snapshot(history));
  assert.equal(changes.renders, 1);
  assert.equal(changes.insertions, 300);
  assert.equal(f.chat.rows.size, 300);
  assert.equal(createdRows(f).length, 300);
  assert.deepEqual(
    contents(f),
    history.map((message) => message.content),
  );
  const retained = rows(f);
  f.chat.handleEvent(snapshot(history.map((message) => ({ ...message }))));
  assert.equal(changes.renders, 2);
  assert.equal(changes.insertions, 300);
  assert.equal(createdRows(f).length, 300);
  assert.deepEqual(rows(f), retained);
  f.chat.participantsChanged();
  assert.equal(changes.insertions, 300, 'roster changes leave unchanged chat rows attached');
  assert.deepEqual(rows(f), retained);
});

test('live append evicts only the oldest row and updates the new first-row grouping', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.handleEvent(snapshot(Array.from({ length: 300 }, (_, index) => entry(index))));
  const retained = rows(f);
  assert.equal(retained[0].classList.contains('grouped'), false);
  assert.equal(retained[1].classList.contains('grouped'), true);
  const changes = observeRows(f);
  f.chat.receive(entry(300));
  assert.equal(changes.insertions, 1);
  assert.equal(createdRows(f).length, 301);
  assert.equal(f.chat.rows.size, 300);
  assert.equal(retained[0].isConnected, false);
  assert.deepEqual(rows(f).slice(0, 299), retained.slice(1));
  assert.equal(retained[1].classList.contains('grouped'), false);
  assert.equal(rows(f).at(-1).classList.contains('grouped'), true);
  assert.deepEqual(
    contents(f),
    Array.from({ length: 300 }, (_, index) => `Message ${index + 1}`),
  );
});

test('chronological insertion and acknowledgement moves preserve row identity and adjacent grouping', async () => {
  const f = await fixture();
  await f.chat.activate();
  const instant = Date.now();
  const at = (offset) => new Date(instant + offset).toISOString();
  f.chat.receive(entry('later', { sentAt: at(120_000) }));
  const later = rows(f)[0];
  f.chat.receive(entry('earlier', { sentAt: at(-120_000) }));
  const earlier = rows(f)[0];
  assert.deepEqual(contents(f), ['Message earlier', 'Message later']);
  assert.equal(rows(f)[1], later);
  assert.equal(later.classList.contains('grouped'), false, 'two-minute gaps start a group');
  f.chat.input.value = 'pending between';
  f.chat.send();
  const pending = f.chat.store.messages.find((message) => message.status === 'pending');
  const pendingRow = rows(f)[1];
  assert.deepEqual(contents(f), ['Message earlier', 'pending between', 'Message later']);
  const changes = observeRows(f);
  f.chat.handleEvent({
    type: 'messageAck',
    message: { ...pending, messageId: 'ack-first', sentAt: at(-180_000) },
  });
  assert.deepEqual(rows(f), [pendingRow, earlier, later]);
  assert.equal(changes.insertions, 1, 'only the acknowledged row changes chronological position');
  assert.equal(pendingRow.dataset.messageId, 'ack-first');
  assert.equal(f.chat.pending.size, 0);
  assert.equal(f.timers.size, 0);
  assert.equal(
    earlier.classList.contains('grouped'),
    false,
    'different adjacent senders start groups',
  );
});

test('mutable acknowledgement and failure fields update the retained row without touching drafts or focus', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.input.value = 'original draft';
  f.chat.send();
  const pending = f.chat.store.messages[0];
  const row = rows(f)[0];
  assert.match(row.textContent, /Sending/);
  f.chat.input.value = 'another unsent draft';
  f.chat.input.focus();
  f.chat.handleEvent({
    type: 'socialError',
    clientMessageId: pending.clientMessageId,
    message: 'Delivery rejected',
  });
  assert.equal(rows(f)[0], row);
  assert.match(row.textContent, /Delivery rejected/);
  assert.equal(row.querySelector('.delivery-error') !== null, true);
  assert.equal(f.chat.input.value, 'another unsent draft');
  assert.equal(f.document.activeElement, f.chat.input);
  row
    .querySelectorAll('button')
    .find((node) => node.textContent === 'Edit & resend')
    .click();
  assert.equal(
    f.chat.input.value,
    'another unsent draft',
    'copying does not overwrite another draft',
  );
  f.chat.input.value = '';
  row
    .querySelectorAll('button')
    .find((node) => node.textContent === 'Edit & resend')
    .click();
  assert.equal(f.chat.input.value, 'original draft');
  assert.equal(f.chat.composition.draft('public'), 'original draft');
  f.chat.handleEvent({
    type: 'messageAck',
    message: { ...pending, messageId: 'confirmed', participantName: 'Updated local name' },
  });
  assert.equal(rows(f)[0], row);
  assert.equal(row.dataset.messageId, 'confirmed');
  assert.doesNotMatch(row.textContent, /Delivery rejected|Edit & resend|Sending/);
  assert.equal(row.querySelector('.delivery-error'), null);
  assert.equal(row.querySelector('.sender').textContent, 'Updated local name');
  assert.equal(f.chat.input.value, 'original draft');
  assert.equal(f.document.activeElement, f.chat.input);
  row.querySelector('.sender').click();
  assert.deepEqual(f.state.actions.at(-1).slice(0, 2), ['local', 'Updated local name']);
  assert.equal(f.chat.pending.size, 0);
  assert.equal(f.timers.size, 0);
});

test('immutable view fingerprints refresh edited text, sender actions and local nickname mentions', async () => {
  const f = await fixture();
  await f.chat.activate();
  const message = entry('view', { content: 'Hello @Local' });
  f.chat.receive(message);
  const row = rows(f)[0];
  const stored = f.chat.store.messages[0];
  assert.equal(row.classList.contains('mentioned'), true);
  f.state.room.nickname = 'SomeoneElse';
  f.chat.participantsChanged();
  assert.equal(rows(f)[0], row);
  assert.equal(row.classList.contains('mentioned'), false);
  f.chat.receive({ ...message, content: 'Hello @SomeoneElse', participantName: 'Renamed Alice' });
  assert.equal(
    f.chat.store.messages[0],
    stored,
    'the store really mutates the cached message object',
  );
  assert.equal(rows(f)[0], row);
  assert.equal(row.classList.contains('mentioned'), true);
  assert.equal(row.querySelector('.msg-text').textContent, 'Hello @SomeoneElse');
  row.querySelector('.sender').click();
  assert.deepEqual(f.state.actions.at(-1).slice(0, 2), ['alice', 'Renamed Alice']);
});

test('snapshot batching preserves privacy, acknowledgement cleanup and replay sound suppression', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.preferences.allowPrivateMessages = false;
  f.chat.preferences.sounds = true;
  f.chat.preferences.ignored = [{ id: 'ignored', name: 'Ignored' }];
  f.chat.playSound = () => assert.fail('Replay must not play sounds');
  f.chat.input.value = 'own pending';
  f.chat.send();
  const pending = { ...f.chat.store.messages[0], messageId: 'acknowledged' };
  const publicMessage = entry('accepted');
  const changes = observeRows(f);
  f.chat.handleEvent(
    snapshot([
      pending,
      entry('ignored', { participantId: 'ignored' }),
      entry('private', { recipientId: 'local' }),
      publicMessage,
      publicMessage,
    ]),
  );
  assert.equal(changes.renders, 1);
  assert.equal(f.chat.pending.size, 0);
  assert.equal(f.timers.size, 0);
  assert.deepEqual(contents(f), ['Message accepted', 'own pending']);
  assert.equal(f.chat.rows.size, 2);
  assert.equal(f.chat.store.names.size, 0);
  f.chat.input.value = 'evicted pending';
  f.chat.send();
  const afterPending = Date.now() + 10_000;
  f.chat.handleEvent(
    snapshot(
      Array.from({ length: 305 }, (_, index) =>
        entry(`new-${index}`, {
          sentAt: new Date(afterPending + index).toISOString(),
        }),
      ),
    ),
  );
  assert.equal(f.chat.rows.size, 300);
  assert.equal(f.chat.pending.size, 0, 'batched eviction clears pending delivery timers');
  assert.equal(f.timers.size, 0);
  assert.equal(contents(f)[0], 'Message new-5');
});

test('hidden conversations, ignore changes and privacy resets discard cached DOM rows', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.receive(entry('public'));
  f.chat.receive(entry('bob', { participantId: 'bob', participantName: 'Bob' }));
  const [alice, bob] = rows(f);
  await f.chat.toggleIgnore('alice', 'Alice', true);
  assert.deepEqual(rows(f), [bob]);
  assert.equal(alice.isConnected, false);
  assert.equal(f.chat.rows.size, 1);
  await f.chat.toggleIgnore('alice', 'Alice', true);
  assert.notEqual(
    rows(f)[0],
    alice,
    'unignore builds a fresh row instead of retaining hidden nodes',
  );
  assert.equal(rows(f)[1], bob);
  const publicRows = rows(f);
  f.chat.receive(entry('private', { recipientId: 'local' }));
  f.chat.openPrivate('alice', 'Alice');
  const privateRow = rows(f)[0];
  assert.equal(f.chat.rows.size, 1);
  assert.ok(publicRows.every((node) => !node.isConnected));
  f.chat.closePrivate();
  assert.equal(privateRow.isConnected, false);
  assert.equal(f.chat.rows.size, 2);
  const beforeReset = rows(f);
  f.chat.reset();
  assert.equal(f.chat.rows.size, 0);
  assert.equal(f.chat.messages.children.length, 0);
  assert.ok(beforeReset.every((node) => !node.isConnected));
});

test('batched replay preserves unread counts, scroll position, input focus and drafts', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.messages.scrollHeight = 1000;
  f.chat.messages.scrollTop = 40;
  f.chat.messages.emit('scroll');
  f.chat.input.value = 'unsent draft while reading';
  f.chat.input.focus();
  const history = Array.from({ length: 300 }, (_, index) => entry(index));
  f.chat.handleEvent(snapshot(history));
  const retained = rows(f);
  assert.equal(f.chat.messages.scrollTop, 40);
  assert.equal(f.chat.store.unread.get('public'), 300);
  assert.equal(f.document.title, '(300) simplestChat');
  f.chat.handleEvent(snapshot(history));
  assert.equal(f.chat.store.unread.get('public'), 300);
  assert.deepEqual(rows(f), retained);
  assert.equal(f.chat.input.value, 'unsent draft while reading');
  assert.equal(f.document.activeElement, f.chat.input);
  f.document.getElementById('scroll-bottom-btn').click();
  assert.equal(f.chat.messages.scrollTop, 1000);
  assert.equal(f.chat.store.unread.size, 0);
  assert.equal(f.document.title, 'simplestChat');
  f.document.getElementById('chat-panel').className = '';
  f.chat.handleEvent(snapshot([entry(300)]));
  assert.equal(f.chat.store.unread.get('public'), 1, 'a hidden panel must not mark replay as read');
  f.document.getElementById('chat-panel').className = 'active';
  f.chat.participantsChanged();
  assert.equal(f.chat.store.unread.size, 0);
});

function composerKey(f, modifiers = {}) {
  let prevented = false;
  f.chat.input.emit('keydown', {
    key: 'Enter',
    preventDefault() {
      prevented = true;
    },
    stopPropagation() {},
    ...modifiers,
  });
  return prevented;
}

test('desktop composer sends multiline text with Enter and leaves Shift+Enter to add a line', async () => {
  const f = await fixture();
  await f.chat.activate();
  assert.equal(f.chat.sendButton.hidden, true);
  assert.equal(f.chat.input.getAttribute('enterkeyhint'), 'send');
  assert.match(f.chat.input.getAttribute('aria-description'), /Shift\+Enter/);
  f.chat.input.value = 'first line\nsecond line';
  f.chat.input.emit('input');
  assert.equal(f.chat.input.style.getPropertyValue('height'), '58px');
  assert.equal(composerKey(f, { shiftKey: true }), false);
  assert.deepEqual(f.state.sent, []);
  assert.equal(composerKey(f), true);
  assert.equal(f.state.sent[0][1], 'first line\nsecond line');
  assert.equal(f.chat.input.value, '');
  assert.equal(f.chat.input.style.getPropertyValue('height'), 'auto');
});

test('composition confirmation never sends or completes a mention, including Safari key code 229', async () => {
  const f = await fixture();
  await f.chat.activate();
  for (const value of ['unfinished message', '@Al']) {
    f.chat.input.value = value;
    f.chat.input.selectionStart = f.chat.input.selectionEnd = value.length;
    f.chat.input.emit('input');
    assert.equal(composerKey(f, { isComposing: true }), false);
    assert.equal(composerKey(f, { keyCode: 229 }), false);
    assert.equal(f.chat.input.value, value);
  }
  assert.deepEqual(f.state.sent, []);
  assert.equal(composerKey(f, { shiftKey: true }), false);
  assert.equal(f.chat.input.value, '@Al', 'Shift+Enter does not accept an open mention');
  assert.equal(composerKey(f), true);
  assert.equal(f.chat.input.value, '@Alice ');
  assert.deepEqual(f.state.sent, [], 'plain Enter still completes desktop mentions first');
});

test('touch and hybrid composers keep Send, with newline Enter and an explicit keyboard send', async () => {
  for (const device of [{ touch: true }, { touchPoints: 1 }]) {
    const f = await fixture(device);
    await f.chat.activate();
    assert.equal(f.chat.sendButton.hidden, false);
    assert.equal(f.chat.input.getAttribute('enterkeyhint'), 'enter');
    f.chat.input.value = 'line one\nline two';
    assert.equal(composerKey(f), false);
    assert.deepEqual(f.state.sent, []);
    f.chat.sendButton.click();
    assert.equal(f.state.sent[0][1], 'line one\nline two');
    for (const modifier of ['ctrlKey', 'metaKey']) {
      f.chat.input.value = 'keyboard send';
      assert.equal(composerKey(f, { [modifier]: true }), true);
      assert.equal(f.state.sent.at(-1)[1], 'keyboard send');
    }
    f.chat.reset();
  }
  const f = await fixture();
  f.pointer.matches = true;
  f.state.pointerChange();
  assert.equal(f.chat.sendButton.hidden, false, 'connecting a touch pointer reveals Send');
});

test('composer height follows draft switches, generated text, recall, width and send reset', async () => {
  const f = await fixture();
  await f.chat.activate();
  const height = () => f.chat.input.style.getPropertyValue('height');
  const offset = () =>
    f.document.getElementById('chat-panel').style.getPropertyValue('--chat-composer-height');
  f.chat.insertText('public\ndraft\nwith lines', 0, 0);
  assert.equal(height(), '78px');
  assert.equal(offset(), '94px');
  f.chat.openPrivate('alice', 'Alice');
  assert.equal(height(), 'auto');
  f.chat.insertText('one\ntwo\nthree\nfour\nfive\nsix\nseven', 0, 0);
  assert.equal(height(), '158px');
  assert.equal(offset(), '154px', 'the newest-message button uses the capped composer height');
  f.chat.switchConversation('public');
  assert.equal(height(), '78px');
  f.chat.send();
  assert.equal(height(), 'auto');
  composerKey(f, { key: 'ArrowUp' });
  assert.equal(height(), '78px');
  f.chat.input.style.setProperty('height', '1px');
  f.state.resize([{ contentRect: { width: 240 } }]);
  assert.equal(height(), '78px', 'width changes remeasure the current draft');
  f.chat.messages.scrollTop = 12;
  f.chat.seenAtBottom = false;
  f.chat.insertText('\nmore', f.chat.input.value.length, f.chat.input.value.length);
  assert.equal(f.chat.messages.scrollTop, 12, 'growing a draft preserves the reading position');
  f.chat.reset();
  assert.equal(height(), 'auto');
});

test('private drafts, generated text, and sent recall stay isolated from public chat', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.input.value = 'public draft';
  f.chat.openPrivate('alice', 'Alice');
  assert.equal(f.chat.input.value, '');
  f.chat.insertText('private 😀', 0, 0);
  f.chat.switchConversation('public');
  assert.equal(f.chat.input.value, 'public draft');
  f.chat.input.value = '';
  f.chat.composition.save('public', '');
  f.chat.switchConversation('alice');
  assert.equal(f.chat.input.value, 'private 😀');
  f.chat.send();
  f.chat.switchConversation('public');
  f.chat.onKey({ key: 'ArrowUp', preventDefault() {} });
  assert.equal(f.chat.input.value, '');
  f.chat.switchConversation('alice');
  f.chat.onKey({ key: 'ArrowUp', preventDefault() {} });
  assert.equal(f.chat.input.value, 'private 😀');
});

test('a restarted guest keeps only the public draft within the same room and viewer', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.input.value = 'unsent public draft';
  f.chat.openPrivate('alice', 'Alice');
  f.chat.insertText('private draft', 0, 0);
  f.state.room.rejoiningAfterRestart = true;
  f.state.room.localParticipantId = 'replacement-guest';
  f.state.room.membershipVersion++;
  await f.chat.activate();
  assert.equal(f.chat.input.value, 'unsent public draft');
  assert.equal(f.chat.composition.draft('public'), 'unsent public draft');
  assert.equal(f.chat.composition.draft('alice'), '');
  assert.equal(f.chat.store.active, 'public');
  assert.deepEqual(f.state.sent, [], 'restoration never sends a draft');
});

for (const change of ['viewer', 'room', 'instance', 'leave', 'ordinary-rejoin']) {
  test(`restart draft retention does not cross ${change}`, async () => {
    const f = await fixture();
    await f.chat.activate();
    f.chat.input.value = 'private-to-this-intent draft';
    f.state.room.rejoiningAfterRestart = true;
    f.state.room.localParticipantId = 'replacement-guest';
    f.state.room.membershipVersion++;
    if (change === 'viewer') f.state.viewer = 'another-viewer';
    if (change === 'room') f.state.room.currentRoomId = 'another-room';
    if (change === 'instance') f.state.room = { ...f.state.room };
    if (change === 'leave') f.chat.reset();
    if (change === 'ordinary-rejoin') f.state.room.rejoiningAfterRestart = false;
    await f.chat.activate();
    assert.equal(f.chat.input.value, '');
    assert.equal(f.chat.composition.draft('public'), '');
  });
}

test('generated mentions and emoji obey character and byte bounds and reset recall state', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.composition.sent('public', 'previous');
  f.chat.composition.recall('public', 'up', '');
  f.chat.input.value = '@Al';
  f.chat.input.selectionStart = 3;
  f.chat.onKey({ key: 'Tab', preventDefault() {} });
  assert.equal(f.chat.input.value, '@Alice ');
  assert.equal(f.chat.composition.draft('public'), '@Alice ');
  assert.equal(f.chat.composition.isRecalling('public'), false);
  f.chat.input.value = 'x'.repeat(2000);
  f.chat.insertText('😀', 2000, 2000);
  assert.equal(f.chat.input.value.length, 2000);
  f.chat.input.value = '界'.repeat(1400);
  f.chat.insertText('😀', 1400, 1400);
  assert.equal(f.chat.input.value.length, 1400);
  assert.equal(f.state.notifications.length, 2);
});

test('disconnected and offline conversations cannot create pending messages', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.state.room.connected = false;
  f.chat.participantsChanged();
  assert.equal(f.chat.input.disabled, true);
  f.chat.input.value = 'offline text';
  f.chat.send();
  assert.equal(f.chat.pending.size, 0);
  assert.equal(f.state.sent.length, 0);
  f.state.room.connected = true;
  f.chat.openPrivate('departed', 'Departed');
  assert.equal(f.chat.input.disabled, true);
  f.chat.input.value = 'undeliverable';
  f.chat.send();
  assert.equal(f.state.sent.length, 0);
});

test('closing a pending PM clears timers and a late acknowledgement cannot reopen it', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.openPrivate('alice', 'Alice');
  f.chat.input.value = 'private';
  f.chat.send();
  const pending = { ...f.chat.store.messages[0] };
  assert.equal(f.chat.pending.size, 1);
  f.chat.closePrivate();
  assert.equal(f.chat.pending.size, 0);
  assert.equal(f.timers.size, 0);
  f.chat.handleEvent({ type: 'messageAck', message: { ...pending, messageId: 'server-ack' } });
  assert.equal(f.chat.store.names.size, 0);
  assert.equal(f.chat.store.messages.length, 0);
  f.chat.handleEvent({
    type: 'privateMessageReceived',
    message: entry('new', { recipientId: 'local' }),
  });
  assert.equal(f.chat.store.names.get('alice'), 'Alice');
});

test('early events initialize the viewer before activation requests finish and opt-out suppresses incoming PMs', async () => {
  const f = await fixture();
  const pending = deferred();
  f.state.handle = () => pending.promise;
  f.state.storage.set(
    'simplestchat.chat.v1.account-a',
    JSON.stringify({ allowPrivateMessages: false }),
  );
  f.chat.handleEvent({ type: 'chatReceived', ...entry('public') });
  f.chat.handleEvent({
    type: 'privateMessageReceived',
    message: entry('private', { recipientId: 'local' }),
  });
  f.chat.handleEvent({
    type: 'messageAck',
    message: entry('own', { participantId: 'local', recipientId: 'alice' }),
  });
  assert.deepEqual(
    f.chat.store.messages.map((message) => message.messageId),
    ['server-public', 'server-own'],
  );
  assert.equal(f.state.requests.length, 1);
  pending.resolve({});
  await f.chat.activate();
});

test('activation shares in-flight work, retries failure, and reapplies on a fresh same-account membership', async () => {
  const f = await fixture();
  const pending = deferred();
  f.state.handle = (action) =>
    action === 'setChatPreferences' ? pending.promise : Promise.resolve({});
  const first = f.chat.activate();
  const second = f.chat.activate();
  assert.equal(f.state.requests.length, 1);
  pending.reject(new Error('Temporary preference failure'));
  await Promise.all([first, second]);
  assert.deepEqual(
    f.state.requests.map((value) => value.action),
    ['setChatPreferences', 'getRoomSnapshot'],
  );
  assert.equal(f.chat.activationSynced, false);
  f.state.handle = async () => ({});
  await f.chat.activate();
  assert.equal(f.chat.activationSynced, true);
  assert.equal(f.state.requests.length, 4);
  await f.chat.activate();
  assert.equal(f.state.requests.length, 4);
  f.state.room.membershipVersion++;
  await f.chat.activate();
  assert.equal(f.state.requests.length, 6);
});

test('leave cancels activation continuation and clears private drafts, audio, and pending requests', async () => {
  const f = await fixture();
  const pending = deferred();
  f.state.handle = () => pending.promise;
  const activating = f.chat.activate();
  f.chat.openPrivate('alice', 'Alice');
  f.chat.input.value = 'secret draft';
  f.chat.preferences.sounds = true;
  f.chat.resumeSoundFromGesture();
  f.chat.reset();
  f.state.room = null;
  pending.resolve({});
  await activating;
  assert.equal(f.state.requests.length, 1, 'a departed viewer must not request replay');
  assert.equal(f.chat.store.localId, '');
  assert.equal(f.chat.input.value, '');
  assert.equal(f.chat.composition.draft('alice'), '');
  assert.equal(f.chat.audio, null);
});

test('late ignore success and failure cannot overwrite another viewer or persist under the wrong account', async () => {
  for (const succeeds of [true, false]) {
    const f = await fixture();
    await f.chat.activate();
    const pending = deferred();
    f.state.handle = () => pending.promise;
    const update = f.chat.toggleIgnore('alice', 'Alice', true);
    f.chat.reset();
    f.state.viewer = 'account-b';
    f.chat.viewerKey = 'account-b';
    f.chat.preferences = {
      allowPrivateMessages: false,
      sounds: false,
      largeText: true,
      ignored: [],
    };
    if (succeeds) pending.resolve({});
    else pending.reject(new Error('Old request rejected'));
    await update.catch(() => {});
    assert.equal(f.chat.preferences.allowPrivateMessages, false);
    assert.equal(f.chat.preferences.largeText, true);
    assert.equal(f.chat.preferences.ignored.length, 0);
    assert.equal(f.state.storage.size, 0);
  }
});

test('concurrent preference writes are rejected without corrupting confirmed ignore state', async () => {
  const f = await fixture();
  await f.chat.activate();
  const pending = deferred();
  f.state.handle = () => pending.promise;
  const first = f.chat.toggleIgnore('alice', 'Alice', true);
  await assert.rejects(f.chat.toggleIgnore('bob', 'Bob', true), /Please wait/);
  assert.equal(f.chat.isIgnored('alice'), false, 'failed writes need no optimistic rollback');
  pending.resolve({});
  await first;
  assert.equal(f.chat.isIgnored('alice'), true);
  assert.equal(f.chat.isIgnored('bob'), false);
});

test('public and PM unread counts survive hidden panels, deduplicate replay, and clear on actual reading', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.document.getElementById('chat-panel').className = '';
  f.chat.receive(entry('public'));
  f.chat.receive(entry('private', { recipientId: 'local' }));
  f.chat.receive(entry('private', { recipientId: 'local' }), true);
  assert.equal(f.chat.store.unread.get('public'), 1);
  assert.equal(f.chat.store.unread.get('alice'), 1);
  assert.equal(f.document.title, '(2) simplestChat');
  f.document.getElementById('chat-panel').className = 'active';
  f.chat.participantsChanged();
  assert.equal(f.chat.store.unread.has('public'), false);
  assert.equal(f.chat.store.unread.get('alice'), 1);
  f.chat.openPrivate('alice', 'Alice');
  assert.equal(f.chat.store.unread.size, 0);
  assert.equal(f.document.title, 'simplestChat');
});

test('conversation pills preserve drafts, unread counts and focused nodes across incoming messages', async () => {
  const f = await fixture();
  await f.chat.activate();
  const publicChat = f.chat.conversationButtons.get('public').node;
  assert.equal(publicChat.dataset.conversationId, 'public');
  assert.equal(publicChat.getAttribute('aria-pressed'), 'true');
  assert.equal(f.chat.conversations.getAttribute('aria-label'), 'Conversations');
  f.chat.input.value = 'public draft';
  f.chat.input.emit('input');
  f.chat.openPrivate('alice', 'Alice');
  const privateChat = f.chat.conversationButtons.get('alice').node;
  assert.equal(privateChat.getAttribute('aria-pressed'), 'true');
  f.chat.input.value = 'private draft';
  f.chat.input.emit('input');
  publicChat.click();
  assert.equal(f.chat.input.value, 'public draft');
  publicChat.focus();
  f.chat.receive(entry('new-pm', { recipientId: 'local' }));
  assert.equal(f.chat.conversationButtons.get('public').node, publicChat);
  assert.equal(f.chat.conversationButtons.get('alice').node, privateChat);
  assert.equal(f.document.activeElement, publicChat);
  assert.equal(f.chat.input.value, 'public draft');
  assert.equal(privateChat.querySelector('.conversation-unread').textContent, '1');
  assert.match(privateChat.getAttribute('aria-label'), /1 unread/);
  privateChat.click();
  assert.equal(f.chat.input.value, 'private draft');
  assert.equal(privateChat.querySelector('.conversation-unread').hidden, true);
  assert.equal(publicChat.getAttribute('aria-pressed'), 'false');
  assert.equal(privateChat.getAttribute('aria-pressed'), 'true');
  f.chat.closeButton.focus();
  f.chat.closeButton.click();
  assert.equal(f.chat.conversationButtons.has('alice'), false);
  assert.equal(f.chat.input.value, 'public draft');
  assert.equal(f.document.activeElement, publicChat, 'closing PM restores reachable focus');
  assert.equal(f.chat.closeButton.hidden, true);
});

test('many conversation pills retain full accessible names and focus without rebuilding the list', async () => {
  const f = await fixture();
  await f.chat.activate();
  const longName = 'A very long participant name '.repeat(10);
  for (let index = 0; index < 20; index++) {
    const id = `person-${index}`;
    f.participants.set(id, { id, name: `${longName}${index}` });
    f.chat.openPrivate(id, `${longName}${index}`);
  }
  assert.equal(f.chat.conversations.children.length, 21);
  const focused = f.chat.conversationButtons.get('person-10').node;
  focused.focus();
  f.chat.conversations.scrollLeft = 100;
  f.chat.receive(entry('other-pm', { recipientId: 'local' }));
  assert.equal(f.document.activeElement, focused);
  assert.equal(f.chat.conversations.scrollLeft, 100, 'incoming PM does not scroll away from focus');
  assert.equal(f.chat.conversations.children.length, 22);
  assert.ok(focused.getAttribute('aria-label').includes(`${longName}10`));
  assert.equal(focused.title, `${longName}10`);
  const settings = f.document
    .getElementById('chat-panel')
    .querySelectorAll('button')
    .find((node) => node.getAttribute('aria-label') === 'Chat options');
  assert.equal(settings.textContent, '⋯');
  assert.equal(settings.classList.contains('chat-options-button'), true);
  settings.click();
  assert.equal(f.chat.preferencesDialog.dialog.open, true);
  f.chat.reset();
  assert.equal(f.chat.conversations.children.length, 1);
});

test('saved sound preferences unlock on the next gesture and audio failure is nonfatal', async () => {
  const f = await fixture();
  f.state.storage.set('simplestchat.chat.v1.account-a', JSON.stringify({ sounds: true }));
  await f.chat.activate();
  assert.equal(f.state.audioCreates, 0);
  f.state.audioReject = true;
  f.documentListeners.get('pointerdown')();
  await flush();
  assert.equal(f.state.audioCreates, 1);
  assert.equal(f.state.audioResumes, 1);
  f.chat.audio.state = 'running';
  assert.doesNotThrow(() => f.chat.playSound(440));
  f.chat.receive(entry('still-works'));
  assert.equal(f.chat.store.messages.length, 1);
});

test('preferences dialog unlocks audio before awaiting, and leave prevents late save from reviving privacy state', async () => {
  const f = await fixture();
  await f.chat.activate();
  const pending = deferred();
  f.state.handle = () => pending.promise;
  f.chat.openPreferences();
  const view = f.chat.preferencesDialog;
  const controls = view.dialog.querySelectorAll('input').filter((node) => node.type === 'checkbox');
  controls[0].checked = false;
  controls[1].checked = true;
  view.dialog
    .querySelectorAll('button')
    .find((node) => node.textContent === 'Save preferences')
    .click();
  assert.equal(f.state.audioResumes, 1);
  f.chat.reset();
  f.state.room = null;
  pending.resolve({});
  await flush();
  assert.equal(view.dialog.open, false);
  assert.equal(f.chat.preferences.allowPrivateMessages, true);
  assert.equal(f.chat.preferences.sounds, false);
  assert.equal(f.state.storage.size, 0);
});

test('timestamps are visible by default and follow the chosen format', async () => {
  const f = await fixture();
  await f.chat.activate();
  // Local-time parts, so the expected text holds in every timezone.
  const sentAt = new Date(2026, 8, 28, 14, 5, 9).toISOString();
  f.chat.handleEvent(snapshot([entry('1', { sentAt })]));
  const time = () => rows(f)[0].querySelector('.msg-time').textContent;
  const always = () => f.chat.messages.classList.contains('timestamps-always');
  assert.equal(always(), true, 'timestamps appear without hovering by default');
  assert.equal(time(), '14:05');
  for (const [format, expected] of [
    ['time', '14:05'],
    ['seconds', '14:05:09'],
    ['datetime', '2026-09-28 14:05:09'],
  ]) {
    f.chat.setTimestampFormat(format);
    assert.equal(time(), expected, format);
    assert.equal(always(), true);
  }
  f.chat.setTimestampFormat('time12');
  assert.match(time(), /^2:05\sPM$/);
  f.chat.setTimestampFormat('hover');
  assert.equal(always(), false);
});

test('the preferences dialog offers every timestamp format and saves the chosen one', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.openPreferences();
  const view = f.chat.preferencesDialog;
  const select = view.dialog
    .querySelectorAll('select')
    .find((node) => node.dataset.preference === 'timestamps');
  assert.deepEqual(
    select.options.map((option) => option.value),
    ['hover', 'time', 'time12', 'seconds', 'datetime'],
  );
  assert.equal(select.value, 'time');
  select.value = 'seconds';
  view.dialog
    .querySelectorAll('button')
    .find((node) => node.textContent === 'Save preferences')
    .click();
  await flush();
  assert.equal(f.chat.preferences.timestamps, 'seconds');
});

test('the timestamp format is remembered with the chat preferences, and nonsense is ignored', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.setTimestampFormat('datetime');
  const key = 'simplestchat.chat.v1.account-a';
  assert.equal(JSON.parse(f.state.storage.get(key)).timestamps, 'datetime');
  f.state.storage.set(key, JSON.stringify({ timestamps: 'seconds' }));
  f.chat.reset();
  await f.chat.activate();
  assert.equal(f.chat.preferences.timestamps, 'seconds');
  f.state.storage.set(key, JSON.stringify({ timestamps: 'hover' }));
  f.chat.reset();
  await f.chat.activate();
  assert.equal(f.chat.preferences.timestamps, 'hover', 'an explicit saved choice is preserved');
  f.state.storage.set(key, JSON.stringify({ timestamps: '<script>' }));
  f.chat.reset();
  await f.chat.activate();
  assert.equal(f.chat.preferences.timestamps, 'time');
});

test('join and leave notices show their own timestamp and follow the viewer format', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.system('Alice joined');
  f.chat.system('Alice left');
  assert.deepEqual(contents(f), ['Alice joined', 'Alice left']);
  assert.equal(f.chat.messages.classList.contains('timestamps-always'), true);
  for (const [index, row] of rows(f).entries()) {
    const time = row.querySelector('.msg-time');
    assert.equal(row.classList.contains('system'), true);
    assert.equal(time.tagName, 'TIME');
    assert.equal(time.getAttribute('datetime'), f.chat.store.messages[index].sentAt);
    assert.match(time.textContent, /^\d{2}:\d{2}$/);
    assert.equal(row.querySelector('.sender'), null);
    assert.equal(row.querySelector('.msg-actions'), null);
  }
  f.chat.setTimestampFormat('seconds');
  for (const row of rows(f))
    assert.match(row.querySelector('.msg-time').textContent, /^\d{2}:\d{2}:\d{2}$/);
  f.chat.setTimestampFormat('hover');
  assert.equal(f.chat.messages.classList.contains('timestamps-always'), false);
  assert.equal(rows(f).length, 2);
});

test('delivery state renders outside the hover-revealed timestamp so failures stay visible', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.input.value = 'status test';
  f.chat.send();
  const pending = f.chat.store.messages[0];
  const row = rows(f)[0];
  const sending = row.querySelector('.msg-status');
  assert.ok(sending, 'a pending message owns a status element');
  assert.match(sending.textContent, /Sending/);
  assert.doesNotMatch(row.querySelector('.msg-time').textContent, /Sending/);
  f.chat.handleEvent({
    type: 'socialError',
    clientMessageId: pending.clientMessageId,
    message: 'Delivery rejected',
  });
  const failed = row.querySelector('.msg-status');
  assert.equal(failed.classList.contains('delivery-error'), true);
  assert.match(failed.textContent, /Delivery rejected/);
  assert.doesNotMatch(row.querySelector('.msg-time').textContent, /Delivery rejected/);
  assert.ok(failed.querySelectorAll('button').find((node) => node.textContent === 'Edit & resend'));
  f.chat.handleEvent({ type: 'messageAck', message: { ...pending, messageId: 'confirmed' } });
  assert.equal(row.querySelector('.msg-status'), null, 'confirmed messages carry no status');
  assert.match(row.querySelector('.msg-time').textContent, /\d/);
});

test('public chat keeps its mention hint in the composer instead of a permanent status line', async () => {
  const f = await fixture();
  await f.chat.activate();
  assert.equal(f.chat.conversationStatus.hidden, true);
  assert.equal(f.chat.input.placeholder, 'Message · @mention');
  f.chat.openPrivate('alice', 'Alice');
  assert.equal(f.chat.conversationStatus.hidden, false);
  assert.match(f.chat.conversationStatus.textContent, /Private/);
  assert.doesNotMatch(f.chat.input.placeholder, /@mention/);
});

const look = (row) => ({
  style: ['accent', 'text', 'bubble'].find((kind) => row.classList.contains(`look-${kind}`)),
  color: row.style.getPropertyValue('--sender-color'),
});

test('messages wear the look they were sent with, else the sender’s current one, else automatic', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.participants.get('alice').chatStyle = { color: 'lime', style: 'text' };
  f.chat.handleEvent(
    snapshot([
      entry('1', { chatStyle: { color: 'rose', style: 'bubble' } }),
      entry('2'),
      entry('3', { participantId: 'zed', participantName: 'Zed' }),
    ]),
  );
  const [stamped, current, automatic] = rows(f);
  assert.deepEqual(look(stamped), { style: 'bubble', color: CHAT_PALETTE.rose });
  assert.deepEqual(look(current), { style: 'text', color: CHAT_PALETTE.lime });
  assert.deepEqual(look(automatic), { style: 'accent', color: chatColor('Zed', null) });
  // A pending message of your own wears the look you have now.
  f.state.room.chatStyle = { color: 'violet', style: 'bubble' };
  f.chat.input.value = 'mine';
  f.chat.send();
  assert.deepEqual(look(rows(f).at(-1)), { style: 'bubble', color: CHAT_PALETTE.violet });
});

test('a mention of you is highlighted where it appears, and links stay whole', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.receive(entry('m', { content: 'Hi @local, see https://example.com/@Local and @LOCAL' }));
  const text = rows(f)[0].querySelector('.msg-text');
  assert.equal(rows(f)[0].classList.contains('mentioned'), true);
  assert.deepEqual(
    text.querySelectorAll('mark').map((node) => node.textContent),
    ['@local', '@LOCAL'],
  );
  assert.equal(text.querySelector('a').textContent, 'https://example.com/@Local');
  assert.equal(text.textContent, 'Hi @local, see https://example.com/@Local and @LOCAL');
  // A link whose text can read as another address stays plain text.
  f.chat.receive(entry('n', { content: 'see https://example.com/\u202Emoc.live\u202C now' }));
  const disguised = rows(f)
    .find((node) => node.querySelector('.msg-text').textContent.includes('moc.live'))
    .querySelector('.msg-text');
  assert.equal(disguised.querySelector('a'), null);
  assert.equal(disguised.textContent, 'see https://example.com/\u202Emoc.live\u202C now');
});

test('the preferences dialog previews a look and saves it for the room and the next join', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.state.room.chatStyle = { color: null, style: 'accent' };
  f.chat.openPreferences();
  const view = f.chat.preferencesDialog;
  const radios = (name) =>
    view.dialog.querySelectorAll('input').filter((node) => node.name === name);
  // Like a browser's radio group: checking one unchecks the rest.
  const choose = (name, value) => {
    for (const node of radios(name)) node.checked = node.value === value;
    radios(name)
      .find((node) => node.value === value)
      .emit('change');
  };
  assert.deepEqual(
    radios('chat-color').map((node) => node.value),
    ['', ...Object.keys(CHAT_PALETTE)],
  );
  assert.equal(radios('chat-color').find((node) => node.checked).value, '');
  assert.deepEqual(
    radios('chat-look').map((node) => node.value),
    ['accent', 'text', 'bubble'],
  );
  const preview = view.dialog.querySelector('.chat-msg');
  assert.deepEqual(look(preview), { style: 'accent', color: chatColor('Local', null) });
  choose('chat-color', 'violet');
  choose('chat-look', 'bubble');
  assert.deepEqual(look(preview), { style: 'bubble', color: CHAT_PALETTE.violet });
  view.dialog
    .querySelectorAll('button')
    .find((node) => node.textContent === 'Save preferences')
    .click();
  await flush();
  assert.deepEqual(f.state.requests.at(-1), {
    action: 'setChatStyle',
    data: { chatStyle: { color: 'violet', style: 'bubble' } },
  });
  assert.deepEqual(f.state.room.chatStyle, { color: 'violet', style: 'bubble' });
  assert.deepEqual(f.chat.savedLook(), { color: 'violet', style: 'bubble' });
  assert.equal(view.dialog.open, false);

  // Saving other preferences leaves an unchanged look alone.
  f.chat.openPreferences();
  const requests = f.state.requests.length;
  f.chat.preferencesDialog.dialog
    .querySelectorAll('button')
    .find((node) => node.textContent === 'Save preferences')
    .click();
  await flush();
  assert.deepEqual(
    f.state.requests.slice(requests).map((request) => request.action),
    ['setChatPreferences'],
  );
});

test('a saved look is checked before a join offers it', async () => {
  const f = await fixture();
  const key = 'simplestchat.chat.v1.account-a';
  assert.equal(f.chat.savedLook(), null);
  f.state.storage.set(key, JSON.stringify({ look: { color: 'teal', style: 'text' } }));
  assert.deepEqual(f.chat.savedLook(), { color: 'teal', style: 'text' });
  f.state.storage.set(key, JSON.stringify({ look: { color: null, style: 'bubble' } }));
  assert.deepEqual(f.chat.savedLook(), { color: null, style: 'bubble' });
  f.state.storage.set(key, JSON.stringify({ look: { color: 'mauve', style: 'glow' } }));
  assert.deepEqual(f.chat.savedLook(), { color: null, style: 'accent' });
  f.state.storage.set(key, JSON.stringify({ look: 'violet' }));
  assert.equal(f.chat.savedLook(), null);
});

test('reading resumes below a New messages divider that marks where unseen messages began', async () => {
  const f = await fixture();
  await f.chat.activate();
  const divider = () =>
    [...f.chat.messages.children].find((node) => node.className === 'chat-divider');
  f.chat.receive(entry('1'));
  f.chat.receive(entry('2'));
  assert.equal(divider(), undefined, 'messages read as they arrive need no divider');
  // Scroll up to read history; two more arrive unseen.
  f.chat.messages.scrollHeight = 1000;
  f.chat.messages.scrollTop = 0;
  f.chat.messages.emit('scroll');
  f.chat.receive(entry('3'));
  f.chat.receive(entry('4'));
  f.chat.render();
  assert.equal(divider(), undefined, 'no divider until reading resumes');
  f.chat.messages.scrollTop = 900;
  f.chat.messages.emit('scroll');
  const nodes = [...f.chat.messages.children];
  const at = nodes.indexOf(divider());
  assert.equal(divider().textContent, 'New messages');
  assert.equal(nodes[at + 1].querySelector('.msg-text').textContent, 'Message 3');
  assert.equal(nodes[at + 1].classList.contains('grouped'), false, 'the divider starts a block');
  f.chat.render();
  assert.equal([...f.chat.messages.children].indexOf(divider()), at, 'it stays while shown');
  f.chat.input.value = 'caught up';
  f.chat.send();
  assert.equal(divider(), undefined, 'sending clears it');
});

test('typing @ offers matching people to mention, chosen by keyboard or pointer without sending', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.participants.set('alfie', { id: 'alfie', name: 'Alfie' });
  f.participants.set('bob', { id: 'bob', name: 'Bob' });
  f.participants.set('sal', { id: 'sal', name: 'Sal' });
  const list = f.chat.mentionList;
  const type = (value) => {
    f.chat.input.value = value;
    f.chat.input.selectionStart = f.chat.input.selectionEnd = value.length;
    f.chat.input.emit('input');
  };
  const key = (name) =>
    f.chat.input.emit('keydown', { key: name, preventDefault() {}, stopPropagation() {} });
  type('hi @al');
  assert.equal(list.hidden, false);
  assert.deepEqual(
    list.children.map((option) => option.textContent),
    ['Alice', 'Alfie', 'Sal'],
    'names that start with the text come before names that contain it',
  );
  assert.equal(f.chat.input.getAttribute('role'), null, 'retain the native multiline textbox');
  assert.equal(f.chat.input.getAttribute('aria-autocomplete'), 'list');
  assert.equal(f.chat.input.getAttribute('aria-controls'), list.id);
  assert.equal(f.chat.input.getAttribute('aria-activedescendant'), 'mention-option-0');
  key('ArrowDown');
  assert.equal(f.chat.input.getAttribute('aria-activedescendant'), 'mention-option-1');
  key('Enter');
  assert.equal(f.chat.input.value, 'hi @Alfie ');
  assert.equal(list.hidden, true);
  assert.deepEqual(f.state.sent, [], 'choosing a name does not send the message');
  type('@b');
  list.children[0].emit('click');
  assert.equal(f.chat.input.value, '@Bob ');
  type('@zz');
  assert.equal(list.hidden, true, 'no match, no list');
  type('@a');
  key('Escape');
  assert.equal(list.hidden, true);
  assert.equal(f.chat.input.getAttribute('aria-activedescendant'), null);
});

test('opted-in desktop notices announce mentions and private messages only while out of sight', async () => {
  const f = await fixture();
  await f.chat.activate();
  const save = async (checked) => {
    f.chat.openPreferences();
    const view = f.chat.preferencesDialog;
    view.dialog
      .querySelectorAll('input')
      .find((node) => node.dataset.preference === 'notifications').checked = checked;
    view.dialog
      .querySelectorAll('button')
      .find((node) => node.textContent === 'Save preferences')
      .click();
    await flush();
  };
  await save(true);
  assert.equal(f.chat.preferences.notifications, true);
  f.document.hidden = true;
  f.chat.receive(entry('plain'));
  f.chat.receive(entry('mention', { content: 'hey @Local, look' }));
  f.chat.handleEvent({
    type: 'privateMessageReceived',
    message: entry('pm', { recipientId: 'local', recipientName: 'Local', content: 'psst' }),
  });
  assert.deepEqual(
    f.state.notices.map((notice) => [notice.title, notice.body]),
    [
      ['You were mentioned', 'Open SimplestChat to read it.'],
      ['New private message', 'Open SimplestChat to read it.'],
    ],
  );
  f.state.notices[1].onclick();
  assert.equal(f.chat.store.active, 'alice', 'a notice opens its conversation');
  assert.equal(f.state.notices[1].closed, true);
  f.document.hidden = false;
  f.chat.receive(entry('seen', { content: '@Local while watching' }));
  assert.equal(f.state.notices.length, 2, 'nothing while the tab is in view');
});

test('notification previews require an explicit preference and stale notices cannot open a replacement session', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.openPreferences();
  const view = f.chat.preferencesDialog;
  for (const preference of ['notifications', 'notificationPreviews'])
    view.dialog
      .querySelectorAll('input')
      .find((node) => node.dataset.preference === preference).checked = true;
  view.dialog
    .querySelectorAll('button')
    .find((node) => node.textContent === 'Save preferences')
    .click();
  await flush();
  f.document.hidden = true;
  f.chat.receive(entry('preview', { content: '@Local hello' }));
  const notice = f.state.notices[0];
  assert.equal(notice.title, 'Alice mentioned you');
  assert.equal(notice.body, '@Local hello');
  f.chat.reset();
  assert.equal(notice.closed, true);
  const active = f.chat.store.active;
  notice.onclick();
  assert.equal(f.chat.store.active, active);
  assert.equal(f.chat.preferences.notificationPreviews, false);
});

test('existing desktop-notification opt-in does not opt in to previews', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.state.storage.set(f.chat.preferenceKey(), JSON.stringify({ notifications: true }));
  f.chat.loadPreferences();
  assert.equal(f.chat.preferences.notifications, true);
  assert.equal(f.chat.preferences.notificationPreviews, false);
});

test('a browser that refuses notification permission leaves desktop notices off and says why', async () => {
  const f = await fixture();
  f.state.noticeAnswer = 'denied';
  await f.chat.activate();
  f.chat.openPreferences();
  const view = f.chat.preferencesDialog;
  view.dialog
    .querySelectorAll('input')
    .find((node) => node.dataset.preference === 'notifications').checked = true;
  view.dialog
    .querySelectorAll('button')
    .find((node) => node.textContent === 'Save preferences')
    .click();
  await flush();
  assert.equal(f.chat.preferences.notifications, false);
  assert.match(f.state.notifications.at(-1), /blocked for this site/);
});

test('the offered reactions are exactly the server\u2019s, in its order', async () => {
  const rust = await readFile(new URL('../../src/signaling/protocol.rs', import.meta.url), 'utf8');
  const web = await readFile(new URL('../src/social-chat.ts', import.meta.url), 'utf8');
  const quoted = (source, pattern) =>
    [...pattern.exec(source)[1].matchAll(/["']([^"']+)["']/g)].map((match) => match[1]);
  assert.deepEqual(
    quoted(web, /CHAT_REACTIONS = \[([^\]]*)\]/),
    quoted(rust, /pub const REACTIONS: \[&str; 8\] = \[([^\]]*)\]/),
  );
});

test('reaction chips toggle your own, count what arrives, and leave out ignored people', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.participants.set('bob', { id: 'bob', name: 'Bob' });
  f.chat.receive(entry('1'));
  f.state.handle = async (action, data) =>
    action === 'reactToMessage'
      ? { messageId: data.messageId, reactions: [{ emoji: data.emoji, participantIds: ['local'] }] }
      : {};
  const row = () => rows(f)[0];
  const chips = () =>
    row()
      .querySelectorAll('.reaction-chip')
      .map((chip) => [chip.textContent, chip.getAttribute('aria-pressed'), chip.title]);
  row()
    .querySelectorAll('button')
    .find((node) => node.getAttribute('aria-label') === 'Add a reaction')
    .click();
  const picker = row().querySelector('.reaction-picker');
  assert.equal(picker.hidden, false);
  picker
    .querySelectorAll('button')
    .find((node) => node.textContent === '🎉')
    .click();
  await flush();
  assert.deepEqual(f.state.requests.at(-1), {
    action: 'reactToMessage',
    data: { messageId: 'server-1', emoji: '🎉' },
  });
  assert.deepEqual(chips(), [['🎉 1', 'true', 'Local']]);
  f.chat.handleEvent({
    type: 'messageReactions',
    messageId: 'server-1',
    reactions: [
      { emoji: '🎉', participantIds: ['local', 'bob'] },
      { emoji: '👍', participantIds: ['bob', 'gone'] },
    ],
  });
  assert.deepEqual(chips(), [
    ['🎉 2', 'true', 'Local, Bob'],
    ['👍 2', 'false', 'Bob, someone who left'],
  ]);
  row().querySelectorAll('.reaction-chip')[1].click();
  await flush();
  assert.deepEqual(f.state.requests.at(-1).data, { messageId: 'server-1', emoji: '👍' });
  await f.chat.toggleIgnore('bob', 'Bob', false);
  f.chat.handleEvent({
    type: 'messageReactions',
    messageId: 'server-1',
    reactions: [{ emoji: '👍', participantIds: ['bob'] }],
  });
  assert.deepEqual(chips(), [], 'an ignored person\u2019s reaction is not shown');
});

test('a reply quotes what it answers, travels with the send, and a quote finds the original', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.receive(entry('1', { content: 'Where are the  slides?' }));
  const replyButton = () =>
    rows(f)[0]
      .querySelectorAll('button')
      .find((node) => node.getAttribute('aria-label') === 'Reply');
  replyButton().click();
  assert.equal(f.chat.replyBar.hidden, false);
  assert.match(f.chat.replyBar.textContent, /^Replying to AliceWhere are the slides\?/);
  f.chat.input.value = 'Linked above';
  f.chat.send();
  assert.equal(f.state.sent.at(-1)[0], 'public');
  assert.equal(f.state.sent.at(-1).at(-1), 'server-1', 'the send names the message it answers');
  assert.equal(
    rows(f).at(-1).querySelector('.msg-reply').textContent,
    'Alice: Where are the slides?',
  );
  assert.equal(f.chat.replyBar.hidden, true, 'sending clears the reply');
  replyButton().click();
  f.chat.input.emit('keydown', { key: 'Escape', preventDefault() {}, stopPropagation() {} });
  assert.equal(f.chat.replyBar.hidden, true, 'Escape cancels it');
  f.chat.input.value = 'no reply';
  f.chat.send();
  assert.equal(f.state.sent.at(-1).at(-1), undefined);
  f.chat.receive(
    entry('3', {
      content: 'thanks',
      replyTo: {
        messageId: 'server-1',
        participantId: 'alice',
        participantName: 'Alice',
        excerpt: 'Where are the slides?',
      },
    }),
  );
  rows(f)
    .find((node) => node.querySelector('.msg-text').textContent === 'thanks')
    .querySelector('.msg-reply')
    .click();
  assert.equal(rows(f)[0].classList.contains('flash'), true, 'the original is brought into view');
  f.chat.receive(
    entry('4', {
      content: 'lost',
      replyTo: { messageId: 'evicted', participantId: 'x', participantName: 'X', excerpt: 'old' },
    }),
  );
  rows(f)
    .find((node) => node.querySelector('.msg-text').textContent === 'lost')
    .querySelector('.msg-reply')
    .click();
  assert.match(f.state.notifications.at(-1), /no longer in this chat/);
  f.chat.receive(
    entry('5', {
      content: 'yes you',
      replyTo: {
        messageId: 'mine',
        participantId: 'local',
        participantName: 'Local',
        excerpt: 'q',
      },
    }),
  );
  assert.equal(
    rows(f)
      .find((node) => node.querySelector('.msg-text').textContent === 'yes you')
      .querySelector('.msg-reply').textContent,
    'Local: q',
    'a quote of your own message keeps its sender name',
  );
});

const savedPreferences = {
  allowPrivateMessages: false,
  sounds: true,
  largeText: true,
  timestamps: 'seconds',
  ignored: [
    { id: '7d3f4d5a-1c2b-4e6f-8a9b-0c1d2e3f4a5b', name: 'Saved ignore' },
    { id: 'local', name: 'Never yourself' },
  ],
};

test('an account viewer loads its saved preferences before the room hears its ignore list', async () => {
  const f = await fixture();
  f.state.token = 'jwt';
  f.http.response = { ok: true, status: 200, json: async () => savedPreferences };
  await f.chat.activate();
  assert.equal(f.http.requests.length, 1);
  assert.equal(f.http.requests[0][0], '/api/auth/preferences');
  assert.equal(f.http.requests[0][1].method, 'GET');
  assert.equal(f.http.requests[0][1].headers.Authorization, 'Bearer jwt');
  assert.equal(f.chat.preferences.timestamps, 'seconds');
  assert.equal(f.chat.preferences.allowPrivateMessages, false);
  assert.equal(f.chat.preferences.largeText, true);
  assert.deepEqual(
    f.chat.preferences.ignored.map((entry) => entry.id),
    ['7d3f4d5a-1c2b-4e6f-8a9b-0c1d2e3f4a5b'],
    'the viewer is never on its own list',
  );
  const pushed = f.state.requests.find((request) => request.action === 'setChatPreferences');
  assert.deepEqual(pushed.data, {
    allowPrivateMessages: false,
    ignoredParticipantIds: ['7d3f4d5a-1c2b-4e6f-8a9b-0c1d2e3f4a5b'],
  });
  const stored = JSON.parse(f.state.storage.get('simplestchat.chat.v1.account-a'));
  assert.equal(stored.timestamps, 'seconds', 'the browser copy follows the account');
  assert.equal(f.state.notifications.length, 0);
});

test('guests and signed-out viewers never touch the account preference endpoints', async () => {
  const f = await fixture();
  f.state.viewer = 'guest';
  f.state.token = 'jwt';
  await f.chat.activate();
  f.chat.setTimestampFormat('datetime');
  assert.equal(f.http.requests.length, 0);
  f.chat.reset();
  f.state.viewer = 'account-a';
  f.state.token = null;
  await f.chat.activate();
  f.chat.setTimestampFormat('time');
  assert.equal(f.http.requests.length, 0);
});

test('a changed preference is written to the account, and only the newest failure is reported', async () => {
  const f = await fixture();
  f.state.token = 'jwt';
  f.http.response = { ok: true, status: 200, json: async () => savedPreferences };
  await f.chat.activate();
  f.http.requests.length = 0;
  f.chat.setTimestampFormat('datetime');
  assert.equal(f.http.requests[0][0], '/api/auth/preferences');
  assert.equal(f.http.requests[0][1].method, 'PUT');
  const body = JSON.parse(f.http.requests[0][1].body);
  assert.equal(body.timestamps, 'datetime');
  assert.deepEqual(Object.keys(body).sort(), [
    'allowPrivateMessages',
    'ignored',
    'largeText',
    'sounds',
    'timestamps',
  ]);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(f.state.notifications.length, 0);
  f.http.response = { ok: false, status: 500, text: async () => 'server fragment' };
  f.chat.setTimestampFormat('time');
  f.chat.setTimestampFormat('time12');
  await new Promise((resolve) => setImmediate(resolve));
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(f.state.notifications, ['Chat preferences could not be saved to your account']);
  assert.equal(f.chat.preferences.timestamps, 'time12', 'the browser copy is kept regardless');
});

test('a failed preference load keeps the browser copy and says so once', async () => {
  const f = await fixture();
  f.state.token = 'jwt';
  f.state.storage.set('simplestchat.chat.v1.account-a', JSON.stringify({ timestamps: 'seconds' }));
  f.http.response = { ok: false, status: 503, text: async () => 'down' };
  await f.chat.activate();
  assert.equal(f.chat.preferences.timestamps, 'seconds');
  assert.deepEqual(f.state.notifications, [
    'Saved chat preferences could not be loaded; using this browser’s copy',
  ]);
  assert.ok(f.state.requests.some((request) => request.action === 'setChatPreferences'));
});

test('composing sends a typing notice at most every 2.5 s, for the open conversation, while chat is allowed', async () => {
  const f = await fixture();
  await f.chat.activate();
  const sent = f.state.typing;
  const realNow = Date.now;
  let now = 1_000_000;
  Date.now = () => now;
  try {
    f.chat.input.value = '';
    f.chat.input.emit('input');
    assert.deepEqual(sent, [], 'an empty box is not composing');
    f.chat.input.value = 'hel';
    f.chat.input.emit('input');
    f.chat.input.value = 'hello';
    f.chat.input.emit('input');
    assert.deepEqual(sent, ['public']);
    now += 2600;
    f.chat.input.emit('input');
    assert.deepEqual(sent, ['public', 'public']);
    f.chat.openPrivate('alice', 'Alice');
    now += 2600;
    f.chat.input.value = 'psst';
    f.chat.input.emit('input');
    assert.deepEqual(sent, ['public', 'public', 'alice']);
    f.state.room.canChat = false;
    now += 2600;
    f.chat.input.emit('input');
    assert.deepEqual(sent.length, 3, 'a muted or disabled composer stays quiet');
  } finally {
    Date.now = realNow;
  }
});

test("a peer's typing shows in its own conversation only and expires on its own", async () => {
  const f = await fixture();
  await f.chat.activate();
  f.participants.set('bob', { id: 'bob', name: 'Bob' });
  const realNow = Date.now;
  let now = 5_000_000;
  Date.now = () => now;
  try {
    assert.equal(f.chat.typingLine.hidden, true);
    f.chat.handleEvent({ type: 'participantTyping', participantId: 'alice' });
    assert.equal(f.chat.typingLine.hidden, false);
    assert.equal(f.chat.typingLine.textContent, 'Alice is typing…');
    f.chat.handleEvent({ type: 'participantTyping', participantId: 'bob' });
    assert.equal(f.chat.typingLine.textContent, 'Alice and Bob are typing…');
    f.chat.handleEvent({ type: 'participantTyping', participantId: 'local' });
    assert.equal(f.chat.typingLine.textContent, 'Alice and Bob are typing…', 'never yourself');
    // Private composing reaches only the private conversation with that person.
    f.chat.handleEvent({
      type: 'participantTyping',
      participantId: 'alice',
      targetParticipantId: 'local',
    });
    assert.equal(f.chat.typingLine.textContent, 'Bob is typing…');
    f.chat.openPrivate('alice', 'Alice');
    assert.equal(f.chat.typingLine.textContent, 'Alice is typing…');
    f.chat.switchConversation('public');
    assert.equal(f.chat.typingLine.textContent, 'Bob is typing…');
    // Notices expire four seconds after they arrived.
    now += 4100;
    const [id, callback] = [...f.timers.entries()].at(-1);
    f.timers.delete(id);
    callback();
    assert.equal(f.chat.typingLine.hidden, true);
    f.chat.handleEvent({ type: 'participantTyping', participantId: 'alice' });
    assert.equal(f.chat.typingLine.hidden, false);
    f.chat.reset();
    assert.equal(f.chat.typingLine.hidden, true);
  } finally {
    Date.now = realNow;
  }
});

test('same-looking display names hide IDs and account status but keep distinct action and PM targets', async () => {
  const f = await fixture();
  await f.chat.activate();
  const first = '11111111-1111-4111-8111-111111111111';
  const second = '22222222-2222-4222-8222-222222222222';
  f.participants.set(first, { id: first, name: 'Sam', authenticated: true });
  f.participants.set(second, { id: second, name: 'Sam', authenticated: false });
  f.chat.receive(entry('one', { participantId: first, participantName: 'Sam' }));
  f.chat.receive(entry('two', { participantId: second, participantName: 'Sam' }));
  const messages = rows(f);
  for (const message of messages) {
    assert.equal(message.querySelector('.identity-badge'), null);
    assert.equal(message.querySelector('.chat-sender-button').textContent, 'Sam');
    assert.doesNotMatch(message.textContent, /11111111|22222222|account|guest/);
    message.querySelector('.chat-sender-button').click();
  }
  assert.deepEqual(
    f.state.actions.map((action) => action.slice(0, 2)),
    [
      [first, 'Sam'],
      [second, 'Sam'],
    ],
  );
  f.chat.openPrivate(first, 'Sam');
  f.chat.openPrivate(second, 'Sam');
  for (const id of [first, second]) {
    const conversation = f.chat.conversationButtons.get(id).node;
    assert.equal(conversation.textContent, 'Sam');
    assert.equal(conversation.title, 'Sam');
    assert.equal(conversation.getAttribute('aria-label'), 'Sam');
    conversation.click();
    f.chat.input.value = `Hello ${id}`;
    f.chat.send();
    assert.equal(f.state.sent.at(-1)[0], 'private');
    assert.equal(f.state.sent.at(-1)[1], id);
  }
  f.participants.delete(first);
  f.chat.render();
  const offline = f.chat.conversationButtons.get(first).node;
  assert.equal(offline.textContent, 'Sam · offline');
  assert.equal(offline.title, 'Sam · offline');
  assert.equal(offline.getAttribute('aria-label'), 'Sam · offline');
});

test('own messages and the reply composer keep the recorded room nickname', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.state.room.nickname = 'Current nickname';
  f.chat.receive(entry('self', { participantId: 'local', participantName: 'Earlier nickname' }));
  const message = rows(f)[0];
  assert.equal(message.querySelector('.chat-sender-button').textContent, 'Earlier nickname');
  assert.equal(message.querySelector('.identity-badge'), null);
  message
    .querySelectorAll('button')
    .find((node) => node.getAttribute('aria-label') === 'Reply')
    .click();
  assert.equal(
    f.chat.replyBar.querySelector('.reply-bar-label').textContent,
    'Replying to Earlier nickname',
  );
  f.chat.input.value = 'New message';
  f.chat.send();
  assert.equal(rows(f).at(-1).querySelector('.chat-sender-button').textContent, 'Current nickname');
});

test('moderators confirm public removal; private messages never offer removal', async () => {
  const f = await fixture();
  f.state.room.role = 'moderator';
  await f.chat.activate();
  f.chat.handleEvent({ type: 'chatReceived', ...entry('remove') });
  let control = f.document
    .getElementById('chat-messages')
    .querySelectorAll('button')
    .find((node) => node.getAttribute('aria-label') === 'Remove message');
  assert.ok(control);
  control.click();
  assert.equal(
    f.state.requests.filter((request) => request.action === 'removeChatMessage').length,
    0,
  );
  const dialog = f.document.querySelector('dialog');
  f.state.handle = async (action, data) =>
    action === 'removeChatMessage'
      ? { messageId: data.messageId, removedAt: '2026-10-07T12:00:00Z' }
      : {};
  dialog
    .querySelectorAll('button')
    .find((node) => node.textContent === 'Remove message')
    .click();
  await flush();
  assert.equal(f.chat.store.messages[0].content, '');
  assert.ok(f.document.getElementById('chat-messages').textContent.includes('Message removed'));
  f.chat.handleEvent({
    type: 'privateMessageReceived',
    message: entry('private', { recipientId: 'local' }),
  });
  f.chat.store.active = 'alice';
  f.chat.participantsChanged();
  control = f.document
    .getElementById('chat-messages')
    .querySelectorAll('button')
    .find((node) => node.getAttribute('aria-label') === 'Remove message');
  assert.equal(control, undefined);
});

test('ordinary members lack removal controls and live deletion cancels a quote draft', async () => {
  const f = await fixture();
  f.state.room.role = 'user';
  await f.chat.activate();
  f.chat.handleEvent({ type: 'chatReceived', ...entry('remove') });
  assert.equal(
    f.document
      .getElementById('chat-messages')
      .querySelectorAll('button')
      .find((node) => node.getAttribute('aria-label') === 'Remove message'),
    undefined,
  );
  f.chat.startReply(f.chat.store.messages[0]);
  f.chat.handleEvent({
    type: 'chatMessageRemoved',
    messageId: 'server-remove',
    removedAt: '2026-10-07T12:00:00Z',
  });
  assert.equal(f.chat.replyingTo, null);
  assert.equal(f.chat.store.messages[0].content, '');
});

test('retained reconnect drops pre-recovery public rows outside replay while preserving new live rows and PMs', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.handleEvent(snapshot([]));
  f.chat.handleEvent({ type: 'chatReceived', ...entry('gone') });
  f.chat.handleEvent({ type: 'chatReceived', ...entry('retained') });
  f.chat.handleEvent({
    type: 'privateMessageReceived',
    message: entry('private', { recipientId: 'local' }),
  });
  f.chat.input.value = 'Pending public';
  f.chat.send();
  const pending = f.chat.store.messages.find((message) => message.status === 'pending');
  f.chat.startReply(f.chat.store.messages.find((message) => message.messageId === 'server-gone'));
  const membership = f.state.room.membershipVersion;
  f.chat.prepareSnapshotRecovery();
  f.chat.handleEvent({ type: 'chatReceived', ...entry('new-live') });
  f.chat.prepareSnapshotRecovery();
  f.chat.handleEvent(snapshot([entry('retained')]));
  assert.equal(
    f.state.room.membershipVersion,
    membership,
    'a grace reconnect retains membership identity',
  );
  assert.equal(
    f.chat.store.messages.some((message) => message.messageId === 'server-gone'),
    false,
  );
  assert.ok(f.chat.store.messages.some((message) => message.messageId === 'server-new-live'));
  assert.ok(f.chat.store.messages.some((message) => message.messageId === 'server-private'));
  assert.ok(f.chat.store.messages.includes(pending));
  assert.equal(pending.status, 'pending');
  assert.equal(f.chat.replyingTo, null);
  assert.equal(f.chat.snapshotPublicIds, null);
  f.chat.handleEvent(snapshot([]));
  assert.ok(
    f.chat.store.messages.some((message) => message.messageId === 'server-retained'),
    'ordinary later snapshots do not prune cache',
  );
  f.chat.reset();
});

test('membership activation captures once before its snapshot and reset retires recovery pruning', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.handleEvent(snapshot([]));
  f.chat.handleEvent({ type: 'chatReceived', ...entry('old') });
  f.state.room.membershipVersion++;
  await f.chat.activate();
  f.chat.handleEvent({ type: 'chatReceived', ...entry('during-recovery') });
  f.chat.handleEvent(snapshot([]));
  assert.equal(
    f.chat.store.messages.some((message) => message.messageId === 'server-old'),
    false,
  );
  assert.ok(
    f.chat.store.messages.some((message) => message.messageId === 'server-during-recovery'),
  );
  f.chat.prepareSnapshotRecovery();
  f.chat.reset();
  assert.equal(f.chat.snapshotPublicIds, null);
  await f.chat.activate();
  f.chat.handleEvent({ type: 'chatReceived', ...entry('during-recovery') });
  f.chat.handleEvent(snapshot([]));
  assert.ok(
    f.chat.store.messages.some((message) => message.messageId === 'server-during-recovery'),
    'the previous viewer/room capture cannot prune a replacement',
  );
});

test('private conversation status distinguishes saved account PMs, guest sessions and offline peers', async () => {
  const f = await fixture();
  await f.chat.activate();
  f.chat.openPrivate('alice', 'Alice');
  assert.equal(
    f.chat.conversationStatus.textContent,
    'Private · available only during this room session',
  );
  f.state.token = 'account-token';
  f.participants.get('alice').authenticated = true;
  f.chat.participantsChanged();
  assert.equal(f.chat.conversationStatus.textContent, 'Private · saved for 90 days in Messages');
  f.participants.get('alice').authenticated = false;
  f.chat.participantsChanged();
  assert.equal(
    f.chat.conversationStatus.textContent,
    'Private · available only during this room session',
  );
  f.participants.get('alice').authenticated = true;
  f.state.token = null;
  f.chat.participantsChanged();
  assert.equal(
    f.chat.conversationStatus.textContent,
    'Private · available only during this room session',
  );
  f.participants.delete('alice');
  f.chat.participantsChanged();
  assert.equal(f.chat.conversationStatus.textContent, 'Private · this person is offline');
  f.chat.switchConversation('public');
  assert.equal(f.chat.conversationStatus.textContent, '');
  assert.equal(f.chat.conversationStatus.hidden, true);
});
