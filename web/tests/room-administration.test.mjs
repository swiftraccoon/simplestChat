import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
import { deferred, flush, uiFixture } from './ui-fixture.mjs';

async function fixture(role = 'owner') {
  const f = await uiFixture();
  const state = {
    current: true,
    role,
    calls: [],
    notifications: [],
    settings: {
      id: 'saved-room',
      displayName: 'Saved room',
      passwordProtected: false,
      requireRegistration: false,
      maxParticipants: 20,
      maxBroadcasters: 5,
      allowScreenSharing: true,
      allowChat: true,
      allowVideo: true,
      moderated: false,
      inviteOnly: false,
      secret: true,
      lobbyEnabled: false,
      pushToTalk: false,
      guestsAllowed: true,
      guestsCanBroadcast: true,
      historyRetentionDays: 7,
    },
    members: [
      { userId: 'owner', displayName: 'Owner', role: 'owner', online: false, authenticated: true },
      { userId: 'peer', displayName: '<Peer>', role: 'member', online: true, authenticated: true },
    ],
    lobby: [
      {
        participantId: 'waiting-peer',
        displayName: 'Waiting peer',
        authenticated: true,
        role: 'user',
      },
    ],
    handle: async () => {},
  };
  const api = {};
  for (const method of [
    'overview',
    'settings',
    'members',
    'bans',
    'reports',
    'history',
    'setRole',
    'removeBan',
    'resolveReport',
    'moderate',
    'lobby',
    'lobbyAction',
    'sanction',
  ]) {
    api[method] = async (...args) => {
      state.calls.push({ method, args });
      const override = await state.handle(method, args);
      if (override !== undefined) return override;
      if (method === 'overview' || method === 'settings') {
        if (method === 'settings') Object.assign(state.settings, args[2]);
        return { role: state.role, settings: { ...state.settings } };
      }
      if (method === 'members') return { members: state.members, hasMore: false };
      if (method === 'bans') return { bans: [], hasMore: false };
      if (method === 'reports') return { reports: [], hasMore: false };
      if (method === 'history') return { events: [], hasMore: false };
      if (method === 'lobby') return { lobby: state.lobby, hasMore: false };
    };
  }
  const { administrationSections } = await loadTypeScript('src/room-administration.ts', {
    modules: { './protocol': await loadTypeScript('src/protocol.ts'), './ui': f.ui },
  });
  const sections = administrationSections({
    roomId: 'saved-room',
    accountId: 'owner',
    token: () => 'token',
    current: () => state.current,
    initial: { role, settings: { ...state.settings } },
    api,
    notify: (message) => state.notifications.push(message),
  });
  const open = async (id) => {
    const section = sections.find((section) => section.id === id);
    assert.ok(section);
    const screen = section.screen();
    const context = { current: () => state.current, close() {}, push() {}, pop() {} };
    const body = await screen.body(context);
    f.document.body.append(body);
    return { screen, body };
  };
  return { ...f, state, sections, open };
}

const action = (body, text) => {
  const found = body.querySelectorAll('button').find((node) => node.textContent === text);
  assert.ok(found, `Expected ${text}`);
  return found;
};
const control = (body, label) => {
  const row = body
    .querySelectorAll('label')
    .find((node) => node.children[0]?.textContent === label);
  assert.ok(row, `Expected ${label}`);
  return row.children[1];
};

test('saved-room administration reads without joining and sends only changed settings', async () => {
  const f = await fixture();
  const { body } = await f.open('room-access');
  assert.deepEqual(
    f.state.calls.map((call) => call.method),
    ['overview'],
  );
  control(body, 'Allow guests').checked = false;
  action(body, 'Save room settings').click();
  await flush();
  const calls = f.state.calls.filter((call) => call.method === 'settings');
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].args.slice(0, 3), ['token', 'saved-room', { guestsAllowed: false }]);
  assert.equal(f.state.settings.secret, true);
  assert.equal(control(body, 'Allow guests').checked, false);
});

test('moderator sees role/report/history tools while settings and bans require Admin', async () => {
  const f = await fixture('moderator');
  assert.deepEqual(
    f.sections.filter((section) => section.available()).map((section) => section.id),
    ['room-members', 'room-lobby', 'room-reports', 'room-history'],
  );
  const { body } = await f.open('room-members');
  const role = body.querySelector('select');
  assert.deepEqual(
    role.children.map((node) => node.textContent),
    ['user', 'member'],
  );
  assert.equal(body.querySelectorAll('select').length, 1, 'owner and self cannot be changed');
  assert.match(body.textContent, /<Peer>/, 'member names remain text');
});

test('a lost mutation response is not retried and explicit reload adopts committed settings', async () => {
  const f = await fixture();
  f.state.handle = async (method) => {
    if (method === 'settings') {
      f.state.settings.allowChat = false;
      throw new f.ui.ApiOutcomeUnknownError();
    }
  };
  const { body } = await f.open('room-participation');
  control(body, 'Allow chat').checked = false;
  action(body, 'Save room settings').click();
  await flush();
  assert.match(body.textContent, /response could not be confirmed/);
  assert.equal(action(body, 'Save room settings').disabled, true);
  assert.equal(f.state.calls.filter((call) => call.method === 'settings').length, 1);
  action(body, 'Reload room settings').click();
  await flush();
  assert.equal(control(body, 'Allow chat').checked, false);
  assert.equal(action(body, 'Save room settings').disabled, false);
  assert.equal(f.state.calls.filter((call) => call.method === 'settings').length, 1);
});

test('in-flight changes block duplicate submissions and screen dismissal until settlement', async () => {
  const f = await fixture();
  const response = deferred();
  f.state.handle = (method) => (method === 'settings' ? response.promise : Promise.resolve());
  const { screen, body } = await f.open('room-access');
  control(body, 'Allow guests').checked = false;
  const save = action(body, 'Save room settings');
  save.click();
  save.click();
  await flush();
  assert.equal(screen.canLeave(), false);
  assert.equal(save.disabled, true);
  assert.equal(f.state.calls.filter((call) => call.method === 'settings').length, 1);
  response.resolve({ role: 'owner', settings: { ...f.state.settings, guestsAllowed: false } });
  await flush();
  assert.equal(screen.canLeave(), true);
});

test('late account or screen results cannot render retired account controls', async () => {
  const f = await fixture();
  const response = deferred();
  f.state.handle = (method) => (method === 'overview' ? response.promise : Promise.resolve());
  const section = f.sections.find((section) => section.id === 'room-access');
  const screen = section.screen();
  const opening = screen.body({ current: () => true });
  f.state.current = false;
  response.resolve({ role: 'owner', settings: { ...f.state.settings } });
  const body = await opening;
  assert.equal(body.querySelectorAll('input').length, 0);
  assert.equal(f.state.notifications.length, 0);
  assert.equal(f.state.calls.filter((call) => call.method === 'settings').length, 0);
});

test('Back can abort an initial administration read without rendering its late result', async () => {
  const f = await fixture();
  const response = deferred();
  f.state.handle = (method) => (method === 'overview' ? response.promise : Promise.resolve());
  const screen = f.sections.find((section) => section.id === 'room-access').screen();
  const opening = screen.body({ current: () => true });
  assert.equal(screen.canLeave(), true, 'loading room settings must not trap navigation');
  const signal = f.state.calls.find((call) => call.method === 'overview').args.at(-1);
  screen.leave();
  assert.equal(signal.aborted, true);
  response.resolve({ role: 'owner', settings: { ...f.state.settings } });
  const body = await opening;
  assert.equal(body.querySelectorAll('input').length, 0);
  assert.equal(f.state.calls.filter((call) => call.method === 'settings').length, 0);
});

test('retry and reload reads remain dismissible and ignore results after leaving', async () => {
  for (const label of ['Retry loading room', 'Reload room settings']) {
    const f = await fixture();
    if (label === 'Retry loading room')
      f.state.handle = async () => {
        throw new Error('Unavailable');
      };
    const { screen, body } = await f.open('room-access');
    const response = deferred();
    f.state.handle = (method) => (method === 'overview' ? response.promise : Promise.resolve());
    action(body, label).click();
    await flush();
    assert.equal(screen.canLeave(), true, `${label} must not block Back`);
    const signal = f.state.calls.at(-1).args.at(-1);
    screen.leave();
    assert.equal(signal.aborted, true);
    response.resolve({ role: 'owner', settings: { ...f.state.settings, guestsAllowed: false } });
    await flush();
    if (label === 'Reload room settings')
      assert.equal(
        control(body, 'Allow guests').checked,
        true,
        'retired controls retain no late data',
      );
    else assert.equal(body.querySelectorAll('input').length, 0);
    assert.equal(f.state.calls.filter((call) => call.method === 'settings').length, 0);
  }
});

test('failed initial reads have a bounded explicit retry and permission loss hides edit controls', async () => {
  const f = await fixture();
  let failed = false;
  f.state.handle = async (method) => {
    if (method === 'overview' && !failed) {
      failed = true;
      throw new Error('Unavailable');
    }
  };
  const { body } = await f.open('room-access');
  assert.equal(body.querySelectorAll('input').length, 0);
  assert.equal(action(body, 'Retry loading room').hidden, false);
  f.state.role = 'moderator';
  action(body, 'Retry loading room').click();
  await flush();
  assert.match(body.textContent, /permissions changed/);
  assert.equal(body.querySelectorAll('input').length, 0);
  assert.equal(f.state.calls.length, 2);
});

test('ban requires explicit confirmation and keeps the exact participant and report target', async () => {
  const f = await fixture();
  const { body, screen } = await f.open('room-members');
  action(body, 'Ban…').click();
  assert.equal(f.state.calls.filter((call) => call.method === 'moderate').length, 0);
  const dialog = f.document.querySelectorAll('dialog').at(-1);
  assert.ok(dialog);
  control(dialog, 'Reason').value = 'Abuse';
  control(dialog, 'Duration').value = '3600';
  action(dialog, 'Confirm ban').click();
  await flush();
  const call = f.state.calls.find((call) => call.method === 'moderate');
  assert.deepEqual(call.args.slice(0, 3), [
    'token',
    'saved-room',
    {
      action: 'ban',
      targetParticipantId: 'peer',
      reason: 'Abuse',
      durationSeconds: 3600,
    },
  ]);
  assert.equal(dialog.open, false);
  screen.leave();
});

test('blank limits clear explicit limits while zero is refused before HTTP', async () => {
  const f = await fixture();
  const { body } = await f.open('room-participation');
  control(body, 'Participant limit').value = '0';
  action(body, 'Save room settings').click();
  await flush();
  assert.equal(f.state.calls.filter((call) => call.method === 'settings').length, 0);
  control(body, 'Participant limit').value = '';
  action(body, 'Save room settings').click();
  await flush();
  assert.equal(
    f.state.calls.find((call) => call.method === 'settings').args[2].maxParticipants,
    null,
  );
});

test('revoked permissions clear already-rendered administration content', async () => {
  const f = await fixture();
  const { body } = await f.open('room-members');
  assert.ok(body.querySelector('select'));
  f.state.handle = async (method) => {
    if (method === 'setRole') throw new f.ui.ApiError('Access revoked', 403);
  };
  action(body, 'Update role').click();
  await flush();
  assert.equal(body.querySelectorAll('select').length, 0);
  assert.match(body.textContent, /room access changed/);
  assert.equal(
    f.sections.some((section) => section.available()),
    false,
  );
});

test('waiting-room admission and restriction controls target the selected room over HTTP', async () => {
  const f = await fixture('moderator');
  const waiting = await f.open('room-lobby');
  action(waiting.body, 'Admit').click();
  await flush();
  const admitted = f.state.calls.find((call) => call.method === 'lobbyAction');
  assert.deepEqual(admitted.args.slice(0, 5), [
    'token',
    'saved-room',
    'waiting-peer',
    'admit',
    undefined,
  ]);
  waiting.screen.leave();
  const people = await f.open('room-members');
  action(people.body, 'Restrict camera').click();
  await flush();
  const restricted = f.state.calls.find((call) => call.method === 'sanction');
  assert.deepEqual(restricted.args.slice(0, 3), [
    'token',
    'saved-room',
    { targetParticipantId: 'peer', kind: 'camera', enabled: true },
  ]);
  action(people.body, 'Restore chat').click();
  await flush();
  assert.deepEqual(f.state.calls.filter((call) => call.method === 'sanction').at(-1).args[2], {
    targetParticipantId: 'peer',
    kind: 'chat',
    enabled: false,
  });
  assert.match(people.screen.subtitle, /Saved room.*saved-room/);
});
