import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
import { deferred, flush, uiFixture } from './ui-fixture.mjs';

const chatModule = await loadTypeScript('src/chat-store.ts');
const entry = (id, extra = {}) => ({
  messageId: `server-${id}`,
  clientMessageId: `client-${id}`,
  participantId: 'alice',
  participantName: 'Alice',
  content: `Message ${id}`,
  sentAt: '2026-01-01T00:00:00Z',
  ...extra,
});

async function fixture() {
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
  f.Node.prototype.focus = function () {};
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
    const node = f.ui.el(id === 'chat-input' ? 'input' : 'div');
    node.id = id;
    f.document.body.append(node);
  }
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
  const state = {
    viewer: 'account-a',
    requests: [],
    sent: [],
    notifications: [],
    storage: new Map(),
    handle: async () => ({}),
    audioCreates: 0,
    audioResumes: 0,
    audioReject: false,
  };
  const participants = new Map([['alice', { id: 'alice', name: 'Alice' }]]);
  state.room = {
    localParticipantId: 'local',
    currentRoomId: 'room',
    membershipVersion: 1,
    connected: true,
    canChat: true,
    nickname: 'Local',
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
    modules: { './chat-store': chatModule, './ui': f.ui },
    globals: {
      document: f.document,
      TextEncoder,
      structuredClone,
      AudioContext,
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
    notify: (text) => state.notifications.push(text),
    participantAction() {},
  });
  return { ...f, state, chat, tab, participants, documentListeners, timers };
}

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
  assert.equal(f.document.title, '(2) SimplestChat');
  f.document.getElementById('chat-panel').className = 'active';
  f.chat.participantsChanged();
  assert.equal(f.chat.store.unread.has('public'), false);
  assert.equal(f.chat.store.unread.get('alice'), 1);
  f.chat.openPrivate('alice', 'Alice');
  assert.equal(f.chat.store.unread.size, 0);
  assert.equal(f.document.title, 'SimplestChat');
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
  const controls = view.dialog.querySelectorAll('input');
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
