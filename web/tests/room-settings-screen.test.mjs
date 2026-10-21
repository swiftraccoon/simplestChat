import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';
import { deferred, flush, uiFixture } from './ui-fixture.mjs';

async function fixture() {
  const f = await uiFixture();
  const { document, Node } = f;
  document.activeElement = null;
  Node.prototype.focus = function () {
    document.activeElement = this;
  };
  Node.prototype.scrollIntoView = function () {
    this.scrolled = true;
  };
  Node.prototype.contains = function (node) {
    for (let current = node; current; current = current.parentNode)
      if (current === this) return true;
    return false;
  };
  Node.prototype.checkValidity = function () {
    return this.valid !== false;
  };
  Node.prototype.reportValidity = function () {
    this.reported = true;
  };
  Object.defineProperties(Node.prototype, {
    dataset: {
      get() {
        this._dataset ??= {};
        return this._dataset;
      },
    },
    valueAsNumber: {
      get() {
        return this.value === '' ? NaN : Number(this.value);
      },
    },
    validity: {
      get() {
        return { badInput: this.badInput === true };
      },
    },
    classList: {
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
          toggle: (name, force) => {
            const next = force ?? !names().includes(name);
            node.className = (
              next ? [...new Set([...names(), name])] : names().filter((v) => v !== name)
            ).join(' ');
            return next;
          },
          contains: (name) => names().includes(name),
        };
      },
    },
  });
  const sheetModule = await loadTypeScript('src/settings-sheet.ts', {
    modules: { './ui': f.ui, './settings-sheet.css': {} },
    globals: { document },
  });
  const { RoomSettingsScreen } = await loadTypeScript('src/screens/room-settings.ts', {
    modules: { '../settings-sheet': sheetModule, '../ui': f.ui },
  });
  const requests = [];
  const notifications = [];
  const tasks = [];
  const failures = [];
  const makeRoom = () => ({
    role: 'owner',
    membershipVersion: 1,
    roomSettings: {
      guestsAllowed: true,
      requireRegistration: false,
      inviteOnly: false,
      secret: false,
      lobbyEnabled: false,
      passwordProtected: true,
      guestsCanBroadcast: true,
      moderated: false,
      allowVideo: true,
      allowChat: true,
      allowScreenSharing: true,
      pushToTalk: false,
      maxBroadcasters: 4,
      maxParticipants: null,
    },
    updateRoomSettings(patch) {
      const result = deferred();
      requests.push({ owner: this, patch, ...result });
      return result.promise;
    },
  });
  const state = { room: makeRoom() };
  const screen = new RoomSettingsScreen({
    store: { getRoom: () => state.room },
    notify: (message) => notifications.push(message),
    asyncAction: (action, message) => () => {
      tasks.push(action().catch((error) => failures.push({ message, error })));
    },
  });
  const host = document.createElement('div');
  document.body.append(host);
  const sheet = new sheetModule.SettingsSheet({
    host: () => host,
    sections: () => screen.sections(),
  });
  const control = (id) => sheet.element.querySelector(`#${id}`);
  const status = () => control('room-settings-result');
  const retry = () =>
    sheet.element.querySelectorAll('button').find((b) => b.textContent === 'Retry change');
  return {
    ...f,
    screen,
    sheet,
    state,
    makeRoom,
    requests,
    notifications,
    control,
    status,
    retry,
    open(section = 'room-access') {
      sheet.open({ segment: 'room', section });
    },
    password() {
      sheet.element
        .querySelectorAll('button')
        .find((b) => b.querySelector('.sheet-row-label')?.textContent === 'Password')
        .click();
      return control('rs-password');
    },
    async settled() {
      await Promise.all(tasks);
      assert.deepEqual(failures, [], 'detached actions have no unexpected rejection');
    },
    confirm(index) {
      const request = requests[index];
      const { password, ...settings } = request.patch;
      Object.assign(request.owner.roomSettings, settings);
      if (password !== undefined) request.owner.roomSettings.passwordProtected = password !== null;
      request.resolve();
    },
  };
}

test('room sections expose the existing access and participation vocabulary only to administrators', async () => {
  const f = await fixture();
  const [access, participation] = f.screen.sections();
  assert.deepEqual(
    [access, participation].map(({ id, label, segment }) => ({ id, label, segment })),
    [
      { id: 'room-access', label: 'Who can enter', segment: 'room' },
      { id: 'room-participation', label: 'Chat & broadcasting', segment: 'room' },
    ],
  );
  assert.equal(access.status(), 'Guests');
  Object.assign(f.state.room.roomSettings, {
    requireRegistration: true,
    inviteOnly: true,
    secret: true,
    lobbyEnabled: true,
    moderated: true,
    allowChat: false,
    pushToTalk: true,
  });
  assert.equal(access.status(), 'Accounts · Invite only · Unlisted · Waiting room');
  assert.equal(participation.status(), 'Moderated · Chat off · Hold to talk');
  for (const role of ['owner', 'admin', 'user', 'guest']) {
    f.state.room.role = role;
    assert.equal(access.available(), role === 'owner' || role === 'admin');
  }
  f.state.room = null;
  assert.equal(access.available(), false);
  assert.equal(access.status(), '');
});

test('confirmation locks controls, ignores duplicate events, and synchronizes the server result', async () => {
  const f = await fixture();
  f.open();
  const guests = f.control('rs-guests');
  guests.checked = false;
  guests.emit('change');
  guests.emit('change');
  assert.equal(f.requests.length, 1);
  assert.deepEqual(f.requests[0].patch, { guestsAllowed: false });
  assert.equal(f.screen.pending, true);
  assert.equal(f.status().textContent, 'Saving change…');
  assert.equal(guests.disabled, true);
  assert.equal(f.sheet.element.querySelector('.sheet-screen').getAttribute('aria-busy'), 'true');
  f.screen.sync();
  assert.equal(guests.checked, false, 'an in-flight draft is not replaced by the old snapshot');
  f.confirm(0);
  await f.settled();
  assert.equal(f.screen.pending, false);
  assert.equal(guests.disabled, false);
  assert.equal(guests.checked, false);
  assert.equal(f.status().textContent, 'Change saved');
  assert.equal(f.sheet.element.querySelector('.sheet-screen').getAttribute('aria-busy'), 'false');
});

test('failed changes retain drafts and explicit retry preserves the requested patch', async () => {
  const f = await fixture();
  f.open('room-participation');
  const limit = f.control('rs-max-broadcasters');
  limit.value = '12';
  limit.emit('change');
  f.requests[0].reject(new Error('The room setting was refused'));
  await f.settled();
  assert.equal(f.screen.pending, false);
  assert.equal(limit.disabled, false);
  assert.equal(
    limit.value,
    '12',
    'failure keeps the requested draft instead of the old server limit',
  );
  assert.equal(f.status().textContent, 'The room setting was refused');
  assert.equal(f.status().scrolled, true);
  assert.equal(f.retry().hidden, false);
  f.retry().onclick();
  assert.equal(f.requests.length, 2);
  assert.deepEqual(f.requests[1].patch, { maxBroadcasters: 12 });
  f.confirm(1);
  await f.settled();
  assert.equal(f.retry().hidden, true);
  assert.equal(limit.value, '12');
});

test('retirement fences late success and failure from replacement room and membership controls', async () => {
  for (const replacement of ['room', 'membership']) {
    for (const outcome of ['success', 'failure']) {
      const f = await fixture();
      f.open();
      f.control('rs-guests').checked = false;
      f.control('rs-guests').emit('change');
      f.sheet.close();
      f.screen.retire();
      if (replacement === 'room') f.state.room = f.makeRoom();
      else f.state.room.membershipVersion++;
      f.open();
      const secret = f.control('rs-secret');
      secret.checked = true;
      secret.emit('change');
      if (outcome === 'success') f.requests[0].resolve();
      else f.requests[0].reject(new Error('Old membership refusal'));
      await flush();
      assert.equal(f.screen.pending, true, `${replacement}/${outcome}`);
      assert.equal(secret.disabled, true);
      assert.equal(f.status().textContent, 'Saving change…');
      assert.equal(f.retry().hidden, true);
      f.confirm(1);
      await f.settled();
      assert.equal(f.screen.pending, false);
      assert.equal(secret.checked, true);
    }
  }
});

test('a retry cannot mutate another room, membership, or a view left since its failure', async () => {
  for (const retire of ['room', 'membership', 'view', 'attempt']) {
    const f = await fixture();
    f.open();
    f.control('rs-guests').emit('change');
    f.requests[0].reject(new Error('Refused'));
    await f.settled();
    const retry = f.retry().onclick;
    if (retire === 'room') f.state.room = f.makeRoom();
    else if (retire === 'membership') f.state.room.membershipVersion++;
    else if (retire === 'view') f.sheet.close();
    else f.screen.retire();
    retry();
    await f.settled();
    assert.equal(f.requests.length, 1, retire);
  }
});

test('exact room and membership guards reject late feedback before explicit retirement', async () => {
  for (const replacement of ['room', 'membership']) {
    for (const outcome of ['success', 'failure']) {
      const f = await fixture();
      f.open();
      f.control('rs-guests').emit('change');
      if (replacement === 'room') f.state.room = f.makeRoom();
      else f.state.room.membershipVersion++;
      if (outcome === 'success') f.requests[0].resolve();
      else f.requests[0].reject(new Error('Retired owner refusal'));
      await f.settled();
      assert.equal(f.status().textContent, 'Saving change…', `${replacement}/${outcome}`);
      assert.equal(f.retry().hidden, true);
      assert.equal(f.screen.pending, true, 'only the lifecycle owner retires the pending action');
      f.screen.retire();
      assert.equal(f.screen.pending, false);
    }
  }
});

test('room limits distinguish blank, zero, complete numbers and invalid drafts', async () => {
  const f = await fixture();
  f.open('room-participation');
  const input = f.control('rs-max-participants');
  for (const [value, expected] of [
    ['', null],
    ['0', 0],
    ['3e1', 30],
    ['4294967295', 4294967295],
  ]) {
    input.value = value;
    input.emit('change');
    assert.deepEqual(f.requests.at(-1).patch, { maxParticipants: expected });
    f.confirm(f.requests.length - 1);
    await f.settled();
  }
  const count = f.requests.length;
  for (const value of ['2.5', '-1', '4294967296', '9007199254740992']) {
    input.value = value;
    input.emit('change');
    await f.settled();
    assert.equal(f.requests.length, count);
    assert.match(f.status().textContent, /Enter a whole number from 0 to 4294967295/);
    assert.equal(f.document.activeElement, input);
  }
  input.value = '2';
  input.valid = false;
  input.emit('change');
  await f.settled();
  assert.equal(f.requests.length, count, 'native invalidity cannot silently clear a limit');
  input.value = '';
  input.badInput = true;
  input.emit('change');
  await f.settled();
  assert.equal(f.requests.length, count, 'an incomplete numeric draft cannot clear a limit');
  assert.equal(input.reported, true);
});

test('password changes enforce byte limits, preserve a failed draft for retry, and clear on confirmation', async () => {
  const f = await fixture();
  f.open();
  const password = f.password();
  assert.equal(password.type, 'password');
  assert.equal(password.autocomplete, 'new-password');
  assert.equal(f.control('rs-password-remove').hidden, false);
  assert.match(f.sheet.element.querySelector('.sheet-note').textContent, /A password is set/);
  for (const value of ['short', 'é'.repeat(129)]) {
    password.value = value;
    password.emit('change');
    await f.settled();
    assert.equal(f.requests.length, 0);
    assert.equal(f.notifications.at(-1), 'Room password must be 8-256 bytes');
  }
  password.value = 'é'.repeat(4);
  password.emit('change');
  assert.deepEqual(f.requests[0].patch, { password: 'é'.repeat(4) });
  f.requests[0].reject(new Error('Password update refused'));
  await f.settled();
  assert.equal(password.value, 'é'.repeat(4));
  f.retry().onclick();
  f.confirm(1);
  await f.settled();
  assert.equal(password.value, '');
  f.control('rs-password-remove').click();
  assert.deepEqual(f.requests[2].patch, { password: null });
  f.confirm(2);
  await f.settled();
  assert.equal(f.control('rs-password-remove').hidden, true);
  assert.match(f.sheet.element.querySelector('.sheet-note').textContent, /No password/);
  password.emit('change');
  assert.deepEqual(
    f.requests[3].patch,
    { password: null },
    'a blank password explicitly removes it',
  );
  f.confirm(3);
  await f.settled();
});

test('popping Password clears retired secrets and restores the cached Access feedback owner', async () => {
  for (const outcome of ['success', 'failure']) {
    const f = await fixture();
    f.open();
    const accessBody = f.sheet.element.querySelector('.sheet-screen');
    const accessStatus = f.status();
    const password = f.password();
    password.value = 'room-secret';
    password.emit('change');
    f.sheet.pop();
    assert.equal(
      f.sheet.element.querySelector('.sheet-screen'),
      accessBody,
      'the sheet caches Access',
    );
    assert.equal(f.status(), accessStatus);
    assert.equal(password.value, '', 'leaving clears the detached password immediately');
    assert.equal(f.screen.isOpen, true, 'the parent Access screen remains open');
    if (outcome === 'success') f.confirm(0);
    else f.requests[0].reject(new Error('Late password refusal'));
    await f.settled();
    assert.equal(password.value, '', 'late failure never restores a retired secret');
    assert.equal(f.retry().hidden, true);
    assert.equal(f.screen.pending, false);
    f.control('rs-secret').checked = true;
    f.control('rs-secret').emit('change');
    assert.equal(accessStatus.textContent, 'Saving change…');
    assert.equal(accessBody.getAttribute('aria-busy'), 'true');
    f.confirm(1);
    await f.settled();
    assert.equal(
      accessStatus.textContent,
      'Change saved',
      'subsequent changes use Access feedback',
    );
    assert.equal(accessBody.getAttribute('aria-busy'), 'false');
  }
});

test('closing or disposing while Password is pending retires its drafts and retry action', async () => {
  for (const action of ['close', 'dispose']) {
    const f = await fixture();
    f.open();
    const password = f.password();
    password.value = 'room-secret';
    password.emit('change');
    const status = f.status();
    if (action === 'close') f.sheet.close();
    else {
      f.screen.dispose();
      f.screen.dispose();
    }
    assert.equal(password.value, '');
    assert.equal(f.screen.isOpen, false);
    f.requests[0].reject(new Error('Late refusal'));
    await f.settled();
    assert.equal(password.value, '');
    assert.notEqual(status.textContent, 'Late refusal');
    assert.equal(f.screen.pending, false);
    if (action === 'dispose') assert.equal(f.screen.sections()[0].available(), false);
  }
});
