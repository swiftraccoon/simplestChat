import { accountUpdatesFixture } from './account-updates-fixture.mjs';
import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
import { createDOM, deferred, flush } from './ui-fixture.mjs';

const peer = '11111111-1111-4111-8111-111111111111';
const defaults = () => ({
  privateMessages: true,
  mentions: true,
  quietHours: null,
  conversations: [],
});
const validation = await loadTypeScript('src/notification-validation.ts');

test('notification response validation rejects malformed, unbounded and repeated conversation rules', () => {
  assert.deepEqual(validation.decodeNotificationPreferences(defaults()), defaults());
  for (const value of [
    null,
    {},
    { ...defaults(), privateMessages: 'yes' },
    { ...defaults(), quietHours: {} },
    { ...defaults(), quietHours: { startMinute: 1320, endMinute: 1320, timeZone: 'UTC' } },
    { ...defaults(), quietHours: { startMinute: 1320, endMinute: 420, timeZone: 'Not/AZone' } },
    { ...defaults(), conversations: [{ peerId: peer, muted: false, snoozedUntil: 'not-a-date' }] },
    {
      ...defaults(),
      conversations: [
        { peerId: peer, muted: true, snoozedUntil: null },
        { peerId: peer, muted: false, snoozedUntil: null },
      ],
    },
    {
      ...defaults(),
      conversations: Array.from({ length: 101 }, () => ({
        peerId: peer,
        muted: true,
        snoozedUntil: null,
      })),
    },
  ]) {
    assert.throws(() => validation.decodeNotificationPreferences(value));
  }
});

test('quiet hours wrap midnight with inclusive start, exclusive end and named-zone DST', () => {
  const quiet = { startMinute: 1320, endMinute: 420, timeZone: 'America/New_York' };
  for (const [instant, isQuiet] of [
    ['2026-07-01T01:59:59Z', false],
    ['2026-07-01T02:00:00Z', true],
    ['2026-07-01T04:00:00Z', true],
    ['2026-07-01T10:59:59Z', true],
    ['2026-07-01T11:00:00Z', false],
    ['2026-01-01T11:59:59Z', true],
    ['2026-01-01T12:00:00Z', false],
  ])
    assert.equal(validation.withinQuietHours(quiet, new Date(instant)), isQuiet, instant);
  quiet.startMinute = 60;
  quiet.endMinute = 120;
  for (const [instant, isQuiet] of [
    ['2026-11-01T04:59:59Z', false],
    ['2026-11-01T05:00:00Z', true],
    ['2026-11-01T06:00:00Z', true],
    ['2026-11-01T06:59:59Z', true],
    ['2026-11-01T07:00:00Z', false],
    ['2026-03-08T06:59:59Z', true],
    ['2026-03-08T07:00:00Z', false],
  ])
    assert.equal(validation.withinQuietHours(quiet, new Date(instant)), isQuiet, instant);
});

test('private, mention and room alert policy share quiet hours without mutating unread or messages', () => {
  const state = defaults();
  state.conversations.push({ peerId: peer, muted: true, snoozedUntil: null });
  assert.equal(validation.notificationAllowed(state, 'private', peer), false);
  assert.equal(validation.notificationAllowed(state, 'private', 'another'), true);
  state.conversations[0] = { peerId: peer, muted: false, snoozedUntil: '2026-10-09T12:00:00Z' };
  assert.equal(
    validation.notificationAllowed(state, 'private', peer, new Date('2026-10-09T11:59:59Z')),
    false,
  );
  assert.equal(
    validation.notificationAllowed(state, 'private', peer, new Date('2026-10-09T12:00:00Z')),
    true,
  );
  state.privateMessages = false;
  state.mentions = false;
  assert.equal(validation.notificationAllowed(state, 'private', 'another'), false);
  assert.equal(validation.notificationAllowed(state, 'mention'), false);
  assert.equal(validation.notificationAllowed(state, 'room'), true);
  state.quietHours = { startMinute: 0, endMinute: 60, timeZone: 'UTC' };
  assert.equal(
    validation.notificationAllowed(state, 'room', undefined, new Date('2026-10-09T00:15:00Z')),
    false,
  );
});

async function fixture(initial = {}) {
  const dom = createDOM();
  const calls = [];
  const state = {
    account: 'account-a',
    token: 'account-a-token',
    current: true,
    server: defaults(),
    now: Date.now(),
    ...initial,
  };
  const document = Object.assign(new EventTarget(), dom.document, { hidden: false });
  document.addEventListener = EventTarget.prototype.addEventListener;
  const window = new EventTarget();
  const timers = new Map();
  let timer = 0;
  const api = {
    async notificationPreferences(token, signal) {
      calls.push(['load', token, signal]);
      return structuredClone(state.load ? await state.load(token) : state.server);
    },
    async patchNotificationPreferences(token, policy, signal) {
      calls.push(['save', token, structuredClone(policy), signal]);
      return structuredClone(
        state.save
          ? await state.save(token, policy)
          : (state.server = { ...state.server, ...policy }),
      );
    },
    async saveConversationNotifications(token, id, policy, signal) {
      calls.push(['conversation', token, id, policy, signal]);
      state.server.conversations = [{ peerId: id, ...policy }];
      return structuredClone(state.server);
    },
  };
  const el = (tag, text, className) => {
    const node = document.createElement(tag);
    node.textContent = text ?? '';
    node.className = className ?? '';
    return node;
  };
  const ui = {
    api,
    el,
    button: (text, action) => {
      const node = el('button', text);
      node.addEventListener('click', action);
      return node;
    },
    field: (text, control) => {
      const node = el('label', text);
      node.append(control);
      return node;
    },
  };
  class Clock extends Date {
    static now() {
      return state.now;
    }
  }
  Object.defineProperty(dom.Node.prototype, 'classList', {
    configurable: true,
    get() {
      const node = this;
      const names = () => (node.className ?? '').split(/\s+/).filter(Boolean);
      return {
        add: (...values) => {
          node.className = [...new Set([...names(), ...values])].join(' ');
        },
        remove: (...values) => {
          node.className = names()
            .filter((name) => !values.includes(name))
            .join(' ');
        },
        contains: (value) => names().includes(value),
      };
    },
  });
  Object.defineProperty(dom.Node.prototype, 'dataset', {
    configurable: true,
    get() {
      this._dataset ??= {};
      return this._dataset;
    },
  });
  const sheet = await loadTypeScript('src/settings-sheet.ts', {
    modules: { './ui': ui, './settings-sheet.css': {} },
    globals: { document, setTimeout: () => 1, clearTimeout: () => {} },
  });
  const updates = accountUpdatesFixture();
  const { NotificationPreferences } = await loadTypeScript('src/notification-preferences.ts', {
    modules: {
      './account-updates': updates.module,
      './ui': ui,
      './notification-validation': validation,
      './settings-sheet': sheet,
    },
    globals: {
      document,
      window,
      Date: Clock,
      setInterval: (callback) => {
        timers.set(++timer, callback);
        return timer;
      },
      clearInterval: (id) => timers.delete(id),
    },
  });
  const controller = new NotificationPreferences({
    getAccountId: () => state.account,
    getToken: () => state.token,
    notify: (message) => calls.push(['error', message]),
  });
  const container = el('div');
  document.body.append(container);
  return { ...dom, updates, state, calls, document, window, timers, controller, container };
}

test('unknown signed-in policy suppresses alerts; focus refresh is single-flight and never requests OS permission', async () => {
  const wait = deferred();
  const f = await fixture({ load: () => wait.promise });
  assert.equal(f.controller.allows('private', peer), false);
  const first = f.controller.refresh();
  f.window.dispatchEvent(new Event('focus'));
  assert.equal(f.calls.filter(([kind]) => kind === 'load').length, 1);
  wait.resolve(defaults());
  await first;
  assert.equal(f.controller.allows('private', peer), true);
  f.state.now += 90000;
  assert.equal(
    f.controller.allows('private', peer),
    false,
    'stale policy cannot indefinitely bypass remote mutes',
  );
  f.controller.dispose();
  assert.equal(f.timers.size, 0);
});

test('account switch aborts prior reads and rejects their late policy or save responses', async () => {
  const wait = deferred();
  const f = await fixture({ load: () => wait.promise });
  const first = f.controller.refresh();
  const signal = f.calls[0][2];
  f.state.account = 'account-b';
  f.state.token = 'account-b-token';
  f.state.load = undefined;
  f.state.server = { ...defaults(), privateMessages: false };
  f.controller.reset();
  assert.equal(signal.aborted, true);
  await f.controller.refresh();
  wait.resolve(defaults());
  await first;
  assert.equal(f.controller.allows('private', peer), false);
  f.controller.mountAccount(f.container, () => f.state.current);
  await flush();
  const saveWait = deferred();
  f.state.save = () => saveWait.promise;
  const mentions = f.container.querySelectorAll('input')[1];
  mentions.checked = false;
  mentions.emit('change');
  f.state.account = 'account-c';
  f.state.token = 'account-c-token';
  f.controller.reset();
  saveWait.resolve(defaults());
  await flush();
  assert.equal(f.controller.allows('private', peer), false);
  assert.ok(!f.container.textContent.includes('Saved'));
  f.controller.dispose();
});

test('account rows persist each change and named quiet hours without implicitly enabling browser notifications', async () => {
  const f = await fixture();
  f.controller.mountAccount(f.container, () => f.state.current);
  await flush();
  const inputs = f.container.querySelectorAll('input');
  const change = async (index, apply) => {
    apply(inputs[index]);
    inputs[index].emit('change');
    await flush();
  };
  await change(0, (input) => {
    input.checked = false;
  });
  await change(1, (input) => {
    input.checked = false;
  });
  await change(2, (input) => {
    input.checked = true;
  });
  await change(3, (input) => {
    input.value = '22:00';
  });
  await change(4, (input) => {
    input.value = '07:00';
  });
  await change(5, (input) => {
    input.value = 'America/New_York';
  });
  const saves = f.calls.filter(([kind]) => kind === 'save');
  assert.equal(saves.length, 6, 'every row applies on its own');
  assert.deepEqual(saves.at(-1)[2], {
    quietHours: { startMinute: 1320, endMinute: 420, timeZone: 'America/New_York' },
  });
  assert.deepEqual(
    saves.slice(0, 2).map((call) => call[2]),
    [{ privateMessages: false }, { mentions: false }],
  );
  assert.ok(f.container.textContent.includes('Saved'));
  assert.equal(
    f.calls.some(([kind]) => kind === 'error'),
    false,
  );
  f.controller.dispose();
});

test('conversation mute, snooze and resume change only the account notification override', async () => {
  const f = await fixture();
  f.controller.mountConversation(f.container, peer, () => f.state.current);
  await flush();
  const click = async (text) => {
    f.container
      .querySelectorAll('button')
      .find((node) => node.textContent === text)
      .click();
    await flush();
  };
  await click('Mute');
  assert.equal(f.controller.allows('private', peer), false);
  await click('Unmute');
  assert.equal(f.controller.allows('private', peer), true);
  await click('Snooze 1 hour');
  assert.equal(f.controller.allows('private', peer), false);
  await click('Resume alerts');
  assert.equal(f.controller.allows('private', peer), true);
  assert.deepEqual(
    f.calls.filter(([kind]) => kind === 'conversation').map((call) => call[2]),
    [peer, peer, peer, peer],
  );
  f.controller.dispose();
});

test('closed and account-replaced menus do not mutate account preferences', async () => {
  const f = await fixture();
  f.controller.mountConversation(f.container, peer, () => f.state.current);
  await flush();
  f.state.current = false;
  f.container
    .querySelectorAll('button')
    .find((node) => node.textContent === 'Mute')
    .click();
  await flush();
  assert.ok(!f.calls.some(([kind]) => kind === 'conversation'));
  f.state.account = null;
  f.state.token = null;
  assert.equal(
    f.controller.allows('private', peer),
    true,
    'guest retains existing local opt-in rules',
  );
  f.controller.dispose();
});

test('live rules remain fresh without polls and invalidation gates alerts until a queued read settles', async () => {
  const f = await fixture();
  try {
    await f.controller.refresh();
    f.updates.live.add('account-a');
    f.state.now += 100000;
    assert.equal(f.controller.allows('private'), true);
    const loads = () => f.calls.filter(([kind]) => kind === 'load').length;
    for (const timer of f.timers.values()) timer();
    await flush();
    assert.equal(loads(), 1);
    const hold = deferred();
    f.state.load = () => hold.promise;
    f.updates.emit({ account: 'account-a', scope: 'notifications' });
    assert.equal(f.controller.allows('private'), false);
    f.updates.emit({ account: 'account-a', scope: 'notifications' });
    f.state.load = () => ({ ...defaults(), privateMessages: false });
    hold.resolve(defaults());
    await flush();
    assert.equal(loads(), 3, 'an event received during the read queues a new authoritative read');
    assert.equal(f.controller.allows('private'), false);
    f.updates.live.clear();
    for (const timer of f.timers.values()) timer();
    await flush();
    assert.equal(loads(), 4, 'disconnection restores bounded polling');
  } finally {
    f.controller.dispose();
  }
});

test('clean live notification rules skip passive focus reads without suppressing reconnect or explicit refresh', async () => {
  const f = await fixture();
  try {
    const loads = () => f.calls.filter(([kind]) => kind === 'load').length;
    f.updates.live.add('account-a');
    f.window.dispatchEvent(new Event('focus'));
    await flush();
    assert.equal(loads(), 1, 'unknown rules load even while the stream is healthy');
    f.window.dispatchEvent(new Event('focus'));
    f.document.dispatchEvent(new Event('visibilitychange'));
    await flush();
    assert.equal(loads(), 1);
    await f.controller.refresh();
    assert.equal(loads(), 2, 'explicit reads remain available');
    f.updates.emit({ account: 'account-a', scope: 'ready' });
    await flush();
    assert.equal(loads(), 3);
    f.updates.live.clear();
    f.document.dispatchEvent(new Event('visibilitychange'));
    await flush();
    assert.equal(loads(), 4, 'visibility repairs while the resumed stream is not ready');
  } finally {
    f.controller.dispose();
  }
});

test('a failed notification read still repairs on focus and polling with a healthy stream', async () => {
  const f = await fixture();
  try {
    await f.controller.refresh();
    f.updates.live.add('account-a');
    f.state.load = async () => {
      throw new Error('Temporary read failure');
    };
    await f.controller.refresh();
    f.state.load = async () => ({ ...defaults(), mentions: false });
    f.window.dispatchEvent(new Event('focus'));
    await flush();
    assert.equal(f.controller.allows('mention'), false);
    assert.equal(f.calls.filter(([kind]) => kind === 'load').length, 3);
    f.state.load = async () => {
      throw new Error('Another read failure');
    };
    await f.controller.refresh();
    f.state.load = async () => defaults();
    for (const callback of f.timers.values()) callback();
    await flush();
    assert.equal(f.calls.filter(([kind]) => kind === 'load').length, 5);
    assert.equal(f.controller.allows('mention'), true);
  } finally {
    f.controller.dispose();
  }
});

test('mounted account rows follow remote mute and quiet hours, and another row sends only its explicit field', async () => {
  const f = await fixture();
  try {
    f.controller.mountAccount(f.container, () => f.state.current);
    await flush();
    const inputs = f.container.querySelectorAll('input');
    f.state.server = {
      ...defaults(),
      privateMessages: false,
      quietHours: { startMinute: 60, endMinute: 120, timeZone: 'UTC' },
    };
    f.updates.emit({ account: 'account-a', scope: 'notifications' });
    await flush();
    assert.equal(inputs[0].checked, false, 'a remote mute updates the visible account toggle');
    assert.equal(inputs[2].checked, true);
    assert.equal(inputs[3].value, '01:00');
    assert.equal(inputs[4].value, '02:00');
    assert.equal(inputs[5].value, 'UTC');
    inputs[1].checked = false;
    inputs[1].emit('change');
    await flush();
    assert.deepEqual(f.calls.filter(([kind]) => kind === 'save').at(-1)[2], { mentions: false });
    assert.equal(
      f.state.server.privateMessages,
      false,
      'changing mentions cannot undo a remote private-message mute',
    );
    assert.deepEqual(f.state.server.quietHours, {
      startMinute: 60,
      endMinute: 120,
      timeZone: 'UTC',
    });
    assert.equal(f.controller.allows('private', peer), false);
    f.state.server = { ...f.state.server, quietHours: null };
    f.updates.emit({ account: 'account-a', scope: 'notifications' });
    await flush();
    assert.equal(
      inputs[2].checked,
      false,
      'explicit remote null disables quiet hours in the open form',
    );
  } finally {
    f.controller.dispose();
  }
});

test('queued account choices retain the newest local field while a remote unrelated field changes', async () => {
  const held = deferred();
  const f = await fixture();
  try {
    f.controller.mountAccount(f.container, () => f.state.current);
    await flush();
    const inputs = f.container.querySelectorAll('input');
    f.state.save = () => held.promise;
    inputs[0].checked = false;
    inputs[0].emit('change');
    await flush();
    inputs[1].checked = false;
    inputs[1].emit('change');
    inputs[1].checked = true;
    inputs[1].emit('change');
    await flush();
    assert.equal(inputs[0].checked, false);
    assert.equal(
      inputs[1].checked,
      true,
      'older acknowledgements cannot repaint a newer queued choice',
    );
    assert.equal(f.calls.filter(([kind]) => kind === 'save').length, 1);
    assert.equal(
      f.controller.allows('private', peer),
      false,
      'alerts stay gated while an explicit mute is pending',
    );
    f.state.server = {
      ...defaults(),
      privateMessages: false,
      quietHours: { startMinute: 120, endMinute: 180, timeZone: 'UTC' },
    };
    f.updates.emit({ account: 'account-a', scope: 'notifications' });
    f.state.save = undefined;
    held.resolve(structuredClone(f.state.server));
    await flush();
    assert.deepEqual(
      f.calls.filter(([kind]) => kind === 'save').map((call) => call[2]),
      [{ privateMessages: false }, { mentions: false }, { mentions: true }],
    );
    assert.equal(inputs[1].checked, true);
    assert.equal(inputs[3].value, '02:00');
    assert.deepEqual(f.state.server.quietHours, {
      startMinute: 120,
      endMinute: 180,
      timeZone: 'UTC',
    });
  } finally {
    f.controller.dispose();
  }
});

test('remote quiet-hour changes preserve the focused draft and update its other inputs before a field change', async () => {
  const f = await fixture({
    server: { ...defaults(), quietHours: { startMinute: 60, endMinute: 120, timeZone: 'UTC' } },
  });
  try {
    f.controller.mountAccount(f.container, () => f.state.current);
    await flush();
    const inputs = f.container.querySelectorAll('input');
    f.document.activeElement = inputs[3];
    inputs[3].value = '03:00';
    f.state.server.quietHours = { startMinute: 120, endMinute: 240, timeZone: 'America/New_York' };
    f.updates.emit({ account: 'account-a', scope: 'notifications' });
    await flush();
    assert.equal(inputs[3].value, '03:00', 'a current unsubmitted input is preserved');
    assert.equal(inputs[4].value, '04:00');
    assert.equal(inputs[5].value, 'America/New_York');
    inputs[3].emit('change');
    await flush();
    assert.deepEqual(f.calls.filter(([kind]) => kind === 'save').at(-1)[2], {
      quietHours: { startMinute: 180, endMinute: 240, timeZone: 'America/New_York' },
    });
  } finally {
    f.controller.dispose();
  }
});

test('a failed mutation detaches an obsolete GET so fresh read repair cannot be absorbed by it', async () => {
  const obsolete = deferred();
  const f = await fixture();
  try {
    f.controller.mountAccount(f.container, () => f.state.current);
    await flush();
    f.state.load = () => obsolete.promise;
    const oldRead = f.controller.refresh();
    f.state.load = undefined;
    f.state.server = { ...defaults(), privateMessages: false };
    f.state.save = async () => {
      throw new Error('Response lost after commit');
    };
    const mentions = f.container.querySelectorAll('input')[1];
    mentions.checked = false;
    mentions.emit('change');
    await flush();
    assert.equal(f.controller.allows('private', peer), false);
    assert.equal(
      f.calls.filter(([kind]) => kind === 'load').length,
      3,
      'initial, obsolete and fresh repair reads are distinct',
    );
    obsolete.resolve(defaults());
    await oldRead;
    assert.equal(f.controller.allows('private', peer), false, 'late old GET cannot restore alerts');
    assert.equal(
      f.calls.filter(([kind]) => kind === 'save').length,
      1,
      'unknown outcome never replays a mutation',
    );
  } finally {
    f.controller.dispose();
  }
});

test('open conversation commands follow live mute and snooze snapshots without sending writes', async () => {
  const f = await fixture();
  try {
    f.controller.mountConversation(f.container, peer, () => f.state.current);
    await flush();
    f.state.server = {
      ...defaults(),
      conversations: [{ peerId: peer, muted: true, snoozedUntil: null }],
    };
    f.updates.emit({ account: 'account-a', scope: 'notifications' });
    await flush();
    assert.ok(f.container.querySelectorAll('button').some((node) => node.textContent === 'Unmute'));
    assert.match(f.container.textContent, /Muted/);
    f.state.server.conversations[0] = {
      peerId: peer,
      muted: false,
      snoozedUntil: new Date(f.state.now + 3600000).toISOString(),
    };
    f.updates.emit({ account: 'account-a', scope: 'notifications' });
    await flush();
    assert.ok(
      f.container.querySelectorAll('button').some((node) => node.textContent === 'Resume alerts'),
    );
    assert.match(f.container.textContent, /Snoozed until/);
    assert.equal(
      f.calls.filter(([kind]) => kind === 'conversation').length,
      0,
      'live snapshots only reconcile controls',
    );
    f.state.current = false;
    for (const timer of f.timers.values()) timer();
    assert.equal(
      f.controller.views.size,
      0,
      'retired mounted callbacks are pruned even when healthy streams suppress GET polling',
    );
  } finally {
    f.controller.dispose();
  }
});

test('account replacement retires queued field writes before a different token can execute them', async () => {
  const held = deferred();
  const f = await fixture();
  try {
    f.controller.mountAccount(f.container, () => f.state.current);
    await flush();
    f.state.save = () => held.promise;
    const inputs = f.container.querySelectorAll('input');
    inputs[0].checked = false;
    inputs[0].emit('change');
    await flush();
    inputs[1].checked = false;
    inputs[1].emit('change');
    f.state.account = 'account-b';
    f.state.token = 'account-b-token';
    f.controller.reset();
    held.resolve(defaults());
    await flush();
    assert.equal(f.calls.filter(([kind]) => kind === 'save').length, 1);
    assert.equal(
      f.controller.allows('private', peer),
      false,
      'old completed and queued writes cannot establish a new account policy',
    );
  } finally {
    f.controller.dispose();
  }
});
