import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';
import { createDOM, deferred, flush, uiFixture } from './ui-fixture.mjs';

const { ui } = await uiFixture();

async function fixture() {
  const { Node } = createDOM();
  Node.prototype.focus = function () {
    this.focused = true;
  };
  Node.prototype.removeEventListener = function (type, callback) {
    this.listeners.set(
      type,
      (this.listeners.get(type) ?? []).filter((entry) => entry.callback !== callback),
    );
  };
  const elements = {
    createRoomBtn: new Node('button'),
    createRoomModal: new Node('dialog'),
    createRoomClose: new Node('button'),
    crId: new Node('input'),
    crName: new Node('input'),
    crTopic: new Node('input'),
    crPassword: new Node('input'),
    crModerated: new Node('input'),
    crLobby: new Node('input'),
    crSecret: new Node('input'),
    crGuests: new Node('input'),
    createRoomSubmit: new Node('button'),
    createRoomError: new Node('div'),
  };
  elements.createRoomModal.hidden = true;
  elements.createRoomError.hidden = true;
  elements.createRoomSubmit.textContent = 'Create Room';
  elements.crGuests.checked = true;
  const store = {
    accountId: 'account-one',
    token: 'token-one',
    isLoggedIn: true,
    capabilities: { roomCreation: true },
    selectionVersion: 0,
  };
  const requests = [];
  const notifications = [];
  const created = [];
  const tasks = [];
  const failures = [];
  const { CreateRoomScreen, suggestRoomId } = await loadTypeScript('src/screens/create-room.ts', {
    modules: {
      '../ui': {
        ApiOutcomeUnknownError: ui.ApiOutcomeUnknownError,
        api: {
          createRoom(token, body) {
            const result = deferred();
            requests.push({ token, body, ...result });
            return result.promise;
          },
        },
      },
    },
  });
  const screen = new CreateRoomScreen({
    elements,
    store,
    notify: (...args) => notifications.push(args),
    asyncAction: (action, message) => () => {
      tasks.push(action().catch((error) => failures.push({ message, error })));
    },
    created: (id) => {
      assert.equal(elements.createRoomModal.open, false, 'close before applying the join intent');
      assert.equal(
        elements.crPassword.value,
        '',
        'clear the secret before applying the join intent',
      );
      created.push(id);
    },
  });
  return {
    screen,
    elements,
    store,
    requests,
    notifications,
    created,
    suggestRoomId,
    open() {
      elements.createRoomBtn.click();
    },
    fill() {
      elements.crId.value = 'design-review';
      elements.crName.value = 'Design Review';
    },
    async settled() {
      await Promise.all(tasks);
      assert.deepEqual(failures, [], 'the detached action boundary observed no unexpected failure');
    },
  };
}

test('room IDs use URL-safe name suggestions until the ID is edited', async () => {
  const f = await fixture();
  assert.equal(f.suggestRoomId('Design Review'), 'design-review');
  assert.equal(f.suggestRoomId('  Café — Crème!! '), 'cafe-creme');
  assert.equal(f.suggestRoomId('___'), '');
  assert.equal(f.suggestRoomId('a'.repeat(200)).length, 64);
  f.elements.crName.value = 'First room';
  f.elements.crName.emit('input');
  assert.equal(f.elements.crId.value, 'first-room');
  f.elements.crId.value = 'custom-id';
  f.elements.crId.emit('input');
  f.elements.crName.value = 'Second room';
  f.elements.crName.emit('input');
  assert.equal(f.elements.crId.value, 'custom-id');
  f.elements.crId.value = '';
  f.elements.crId.emit('input');
  f.elements.crName.emit('input');
  assert.equal(f.elements.crId.value, 'second-room');
});

test('opening requires the account capability and each dismissal route clears secrets and errors', async () => {
  const f = await fixture();
  for (const denied of ['guest', 'unavailable', 'disabled']) {
    f.store.isLoggedIn = denied !== 'guest';
    f.store.capabilities =
      denied === 'unavailable' ? null : { roomCreation: denied !== 'disabled' };
    f.open();
    assert.equal(f.elements.createRoomModal.open, false, denied);
  }
  f.store.isLoggedIn = true;
  f.store.capabilities = { roomCreation: true };
  for (const route of ['close', 'backdrop', 'Escape']) {
    f.open();
    assert.equal(f.elements.createRoomModal.open, true);
    assert.equal(f.elements.crName.focused, true);
    f.elements.createRoomModal.emit('click', { target: f.elements.crName });
    assert.equal(f.elements.createRoomModal.open, true, 'clicking a field keeps the dialog open');
    f.elements.crPassword.value = 'room-secret';
    f.elements.createRoomError.textContent = 'Previous error';
    f.elements.createRoomError.hidden = false;
    if (route === 'close') f.elements.createRoomClose.click();
    else if (route === 'backdrop') f.elements.createRoomModal.emit('click');
    else {
      let prevented = false;
      f.elements.createRoomModal.emit('cancel', {
        preventDefault: () => (prevented = true),
      });
      assert.equal(prevented, true);
    }
    assert.equal(f.elements.createRoomModal.open, false, route);
    assert.equal(f.elements.createRoomModal.hidden, true, route);
    assert.equal(f.elements.crPassword.value, '', route);
    assert.equal(f.elements.createRoomError.hidden, true, route);
    assert.equal(f.elements.createRoomError.textContent, '', route);
  }
});

test('validation counts password bytes and creation submits once, joins, and resets the form', async () => {
  const f = await fixture();
  f.open();
  f.elements.createRoomSubmit.click();
  await f.settled();
  assert.equal(f.requests.length, 0);
  assert.equal(f.elements.createRoomError.textContent, 'A room name and a room ID are required');
  f.fill();
  for (const password of ['short', 'é'.repeat(129)]) {
    f.elements.crPassword.value = password;
    f.elements.createRoomSubmit.click();
    await f.settled();
    assert.equal(f.requests.length, 0);
    assert.equal(f.elements.createRoomError.textContent, 'Room password must be 8-256 bytes');
  }
  f.elements.crId.value = ' design-review ';
  f.elements.crId.emit('input');
  f.elements.crName.value = ' Design Review ';
  f.elements.crTopic.value = ' Discuss designs ';
  f.elements.crPassword.value = 'é'.repeat(4);
  f.elements.crModerated.checked = true;
  f.elements.crLobby.checked = true;
  f.elements.crSecret.checked = true;
  f.elements.crGuests.checked = false;
  f.elements.createRoomSubmit.click();
  f.elements.createRoomSubmit.emit('click');
  assert.equal(f.requests.length, 1, 'duplicate events cannot overlap mutations');
  assert.equal(f.elements.createRoomSubmit.textContent, 'Creating...');
  assert.deepEqual(f.requests[0].body, {
    id: 'design-review',
    display_name: 'Design Review',
    topic: 'Discuss designs',
    password: 'é'.repeat(4),
    moderated: true,
    lobby_enabled: true,
    secret: true,
    guests_allowed: false,
  });
  assert.equal(f.requests[0].token, 'token-one');
  f.requests[0].resolve();
  await f.settled();
  assert.deepEqual(f.created, ['design-review']);
  for (const key of ['crId', 'crName', 'crTopic', 'crPassword'])
    assert.equal(f.elements[key].value, '', key);
  for (const key of ['crModerated', 'crLobby', 'crSecret'])
    assert.equal(f.elements[key].checked, false, key);
  assert.equal(f.elements.crGuests.checked, true);
  assert.equal(f.elements.createRoomSubmit.disabled, false);
  f.elements.crName.value = 'Next room';
  f.elements.crName.emit('input');
  assert.equal(
    f.elements.crId.value,
    'next-room',
    'successful reset restores automatic ID suggestions',
  );
});

test('late success after dismissal, reopening, or changed selection never replaces the join intent', async () => {
  for (const retire of ['dismiss', 'reopen', 'selection']) {
    const f = await fixture();
    f.open();
    f.fill();
    f.elements.createRoomSubmit.click();
    if (retire === 'selection') f.store.selectionVersion++;
    else {
      f.screen.dismiss();
      if (retire === 'reopen') f.open();
    }
    f.requests[0].resolve();
    await f.settled();
    assert.deepEqual(f.created, [], retire);
    assert.deepEqual(
      f.notifications,
      [['Room created. Open it from My rooms when ready.']],
      retire,
    );
    assert.equal(f.elements.crName.value, 'Design Review', retire);
    assert.equal(f.elements.createRoomSubmit.disabled, false, retire);
  }
});

test('known failures remain retryable and late failures do not mark a reopened dialog', async () => {
  const f = await fixture();
  f.open();
  f.fill();
  f.elements.createRoomSubmit.click();
  f.requests[0].reject(new Error('That room ID is already used'));
  await f.settled();
  assert.equal(f.elements.createRoomError.textContent, 'That room ID is already used');
  assert.equal(f.elements.createRoomError.hidden, false);
  assert.equal(f.elements.createRoomSubmit.disabled, false);
  f.elements.createRoomSubmit.click();
  f.screen.dismiss();
  f.open();
  f.requests[1].reject(new Error('Late refusal'));
  await f.settled();
  assert.equal(f.elements.createRoomError.hidden, true);
  assert.equal(f.elements.createRoomError.textContent, '');
  assert.deepEqual(f.notifications, []);
  assert.deepEqual(f.created, []);
});

test('unconfirmed outcomes disable retries across reopening and notify only the owning account', async () => {
  for (const stale of [false, true]) {
    const f = await fixture();
    f.open();
    f.fill();
    f.elements.createRoomSubmit.click();
    if (stale) {
      f.screen.dismiss();
      f.open();
    }
    const error = new ui.ApiOutcomeUnknownError();
    f.requests[0].reject(error);
    await f.settled();
    assert.equal(f.elements.createRoomSubmit.disabled, true);
    assert.equal(f.elements.createRoomSubmit.textContent, 'Response not confirmed');
    assert.equal(f.elements.createRoomError.hidden, stale);
    assert.deepEqual(f.notifications, stale ? [[error.message, 8000, 'error']] : []);
    f.screen.dismiss();
    f.open();
    f.elements.createRoomSubmit.emit('click');
    await f.settled();
    assert.equal(f.requests.length, 1, 'reopening cannot repeat an unconfirmed mutation');
    f.screen.resetIdentity();
    assert.equal(f.elements.createRoomSubmit.disabled, false);
    assert.equal(f.elements.createRoomSubmit.textContent, 'Create Room');
  }
});

test('identity reset retires old successes and failures before a replacement identity creates a room', async () => {
  for (const outcome of ['success', 'failure', 'unknown']) {
    const f = await fixture();
    f.open();
    f.fill();
    f.elements.crPassword.value = 'old-secret';
    f.elements.createRoomSubmit.click();
    f.store.accountId = 'account-two';
    f.store.token = 'token-two';
    f.screen.resetIdentity();
    assert.equal(f.elements.crPassword.value, '');
    assert.equal(f.elements.createRoomModal.open, false);
    assert.equal(f.elements.createRoomSubmit.disabled, false);
    f.open();
    f.elements.crId.value = 'replacement-room';
    f.elements.createRoomSubmit.click();
    if (outcome === 'success') f.requests[0].resolve();
    else
      f.requests[0].reject(
        outcome === 'unknown' ? new ui.ApiOutcomeUnknownError() : new Error('Old refusal'),
      );
    await flush();
    assert.deepEqual(f.notifications, [], outcome);
    assert.deepEqual(f.created, [], outcome);
    assert.equal(f.elements.createRoomError.hidden, true, outcome);
    assert.equal(f.elements.createRoomSubmit.disabled, true, outcome);
    assert.equal(f.elements.createRoomSubmit.textContent, 'Creating...', outcome);
    assert.equal(f.requests[1].token, 'token-two');
    f.requests[1].resolve();
    await f.settled();
    assert.deepEqual(f.created, ['replacement-room'], outcome);
    assert.equal(f.elements.createRoomSubmit.disabled, false, outcome);
  }
});

test('disposal removes listeners, clears secrets and ignores pending successes and failures', async () => {
  for (const outcome of ['success', 'unknown']) {
    const f = await fixture();
    f.open();
    f.fill();
    f.elements.crPassword.value = 'room-secret';
    f.elements.createRoomSubmit.click();
    f.screen.dispose();
    f.screen.dispose();
    assert.equal(f.elements.crPassword.value, '');
    assert.equal(f.elements.createRoomModal.open, false);
    for (const node of Object.values(f.elements))
      assert.equal([...node.listeners.values()].flat().length, 0, 'all owned listeners removed');
    f.elements.createRoomBtn.emit('click');
    f.elements.createRoomSubmit.emit('click');
    f.elements.crName.value = 'Changed after disposal';
    f.elements.crName.emit('input');
    assert.equal(f.elements.crId.value, 'design-review');
    if (outcome === 'success') f.requests[0].resolve();
    else f.requests[0].reject(new ui.ApiOutcomeUnknownError());
    await f.settled();
    assert.equal(f.requests.length, 1);
    assert.deepEqual(f.created, []);
    assert.deepEqual(f.notifications, []);
  }
});
