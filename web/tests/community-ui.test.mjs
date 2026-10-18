import { accountUpdatesFixture } from './account-updates-fixture.mjs';
import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
import { deferred, flush, uiFixture } from './ui-fixture.mjs';
import { loadAppearanceFixture } from './appearance-fixture.mjs';
import { loadDiscoveryFixture } from './discovery-fixture.mjs';

async function fixture() {
  const dom = await uiFixture();
  const auth = { isLoggedIn: true, userId: 'account-a', jwt: 'token-a' };
  const profile = {
    id: 'account-a',
    email: 'a@example.test',
    display_name: 'Alice',
    bio: '',
    avatar_url: null,
    recovery_enabled: false,
    profile_style: { color: null, style: 'accent' },
  };
  const state = {
    requests: [],
    unread: async () => ({ unreadCount: 0 }),
    notifications: [],
    updated: [],
    signedOut: 0,
    forgotten: 0,
    room: null,
    handle: async (path) =>
      path === '/api/auth/passkeys'
        ? { password_enabled: true, recovery_enabled: false, passkeys: [], maximum: 10 }
        : path === '/api/auth/sessions'
          ? [
              {
                id: 'current-session',
                current: true,
                created_at: '2026-10-07T10:00:00Z',
                refreshed_at: '2026-10-07T10:00:00Z',
                expires_at: '2026-10-14T10:00:00Z',
              },
            ]
          : path === '/api/auth/invites'
            ? []
            : { ...profile },
    upload: dom.ui.rasterUpload,
  };
  const request = async (...args) => {
    state.requests.push(args);
    return state.handle(...args);
  };
  const ui = {
    ...dom.ui,
    api: {
      ...dom.ui.api,
      administration: (token, id, signal) =>
        request(
          `/api/rooms/${encodeURIComponent(id)}/administration`,
          token,
          'GET',
          undefined,
          signal,
        ),
      administrationMembers: (token, id, offset, signal) =>
        request(
          `/api/rooms/${encodeURIComponent(id)}/administration/members?offset=${offset}`,
          token,
          'GET',
          undefined,
          signal,
        ),
      inboxUnread: (...args) => state.unread(...args),
      publicProfile: (id, token) => request(`/api/auth/profiles/${encodeURIComponent(id)}`, token),
      accountProfile: (token) => request('/api/auth/profile', token),
      accountSessions: (token, signal) =>
        request('/api/auth/sessions', token, 'GET', undefined, signal),
      revokeSession: (token, id, signal) =>
        request(`/api/auth/sessions/${id}`, token, 'DELETE', undefined, signal),
      revokeOtherSessions: (token, signal) =>
        request('/api/auth/sessions/others', token, 'DELETE', undefined, signal),
      registrationInvites: (token) => request('/api/auth/invites', token),
      updateProfile: (token, data) => request('/api/auth/profile', token, 'PATCH', data),
      changePassword: (token, data) => request('/api/auth/password', token, 'POST', data),
      passkeySettings: (token) => request('/api/auth/passkeys', token),
      passkeyAction: (token, data) => request('/api/auth/passkeys/start', token, 'POST', data),
      redeemRecovery: (data) => request('/api/auth/recovery/redeem', null, 'POST', data),
      ownRooms: (token) => request('/api/rooms/mine', token),
      savedRooms: async () => ({ rooms: [] }),
      updateRoomIdentity: (id, token, data) =>
        request(`/api/rooms/${encodeURIComponent(id)}/identity`, token, 'PATCH', data),
      deleteRoom: (id, token) => request(`/api/rooms/${encodeURIComponent(id)}`, token, 'DELETE'),
    },
    rasterUpload: (...args) => state.upload(...args),
  };
  const security = await loadTypeScript('src/account-security.ts', {
    modules: { './ui': ui, './auth': await loadTypeScript('src/auth.ts') },
    globals: {
      document: dom.document,
      navigator: {
        clipboard: {
          writeText: async (value) => {
            state.copied = value;
          },
        },
      },
    },
  });
  Object.defineProperties(dom.Node.prototype, {
    classList: {
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
          toggle(value, force) {
            const has = names().includes(value);
            const next = force ?? !has;
            if (next && !has) node.className = [...names(), value].join(' ');
            if (!next && has)
              node.className = names()
                .filter((n) => n !== value)
                .join(' ');
            return next;
          },
          contains: (value) => names().includes(value),
        };
      },
    },
    dataset: {
      configurable: true,
      get() {
        this._dataset ??= {};
        return this._dataset;
      },
    },
  });
  dom.Node.prototype.focus = function focus() {
    dom.document.activeElement = this;
  };
  const sheet = await loadTypeScript('src/settings-sheet.ts', {
    modules: { './ui': ui, './settings-sheet.css': {} },
    globals: { document: dom.document, setTimeout: () => 1, clearTimeout: () => {} },
  });
  const updates = accountUpdatesFixture();
  const api = await loadTypeScript('src/community-ui.ts', {
    modules: {
      './account-updates': updates.module,
      './ui': ui,
      './settings-sheet': sheet,
      './room-administration': await loadTypeScript('src/room-administration.ts', {
        modules: { './ui': ui, './settings-sheet': sheet },
        globals: { document: dom.document },
      }),
      './room-administration-api': await loadTypeScript('src/room-administration-api.ts', {
        modules: { './ui': ui },
      }),
      './discovery': await loadDiscoveryFixture({ ...dom, ui }),
      './appearance': await loadAppearanceFixture({ ...dom, ui }),
      './avatar-colors': await loadTypeScript('src/avatar-colors.ts'),
      './account-sessions': await loadTypeScript('src/account-sessions.ts', {
        modules: { './ui': ui },
        globals: { document: dom.document },
      }),
      './chat-history': {
        openPrivateInbox() {},
        onInboxChanged(listener) {
          state.inboxListener = listener;
        },
      },
      './account-security': {
        ...security,
        mountAccountSecurity: (options) => {
          state.securityCompleted = options.completed;
          return security.mountAccountSecurity(options);
        },
      },
    },
    globals: {
      document: dom.document,
      TextEncoder,
      URL,
      window: {
        location: { href: 'http://localhost:3000/?old=value#old-room' },
        confirm: () => true,
      },
      navigator: {
        clipboard: {
          writeText: async (value) => {
            state.copied = value;
          },
        },
      },
    },
  });
  state.openedSettings = [];
  state.accountChanges = 0;
  const community = new api.CommunityUI({
    openSettings: (section) => state.openedSettings.push(section),
    onAccountChanged: () => state.accountChanges++,
    auth,
    getRoom: () => state.room,
    notify: (value) => state.notifications.push(value),
    onProfileChanged: (value) => state.updated.push(value),
    onRoomsChanged() {},
    async onRoomDeleted() {},
    async onSignedOut() {
      state.signedOut++;
    },
    onForgetDevice() {
      state.forgotten++;
    },
  });
  return { ...dom, ...api, ui, auth, state, profile, community, updates };
}

function dialog(fixture, title) {
  const node = fixture.document
    .querySelectorAll('dialog')
    .find((node) => node.children[0].children[0].textContent === title);
  assert.ok(node, `Expected ${title} dialog`);
  return node;
}

function control(view, label) {
  const wrapper = view
    .querySelectorAll('label')
    .find((node) => node.children[0].textContent === label);
  assert.ok(wrapper, `Expected ${label} field`);
  return wrapper.children.at(-1);
}

/** Opens one account screen the way the settings sheet does. */
async function accountScreen(fixture, id, { settle = true } = {}) {
  const section = fixture.community.sections().find((entry) => entry.id === id);
  assert.ok(section, `Expected ${id} section`);
  const screen = section.screen();
  const context = {
    open: true,
    push() {},
    pop() {},
    close() {
      context.open = false;
    },
    current: () => context.open,
  };
  const opening = screen.body(context);
  const view = {
    screen,
    context,
    opening,
    body: null,
    get open() {
      return context.open;
    },
    leave() {
      context.open = false;
      screen.leave?.();
    },
  };
  if (settle) {
    view.body = await opening;
    await flush();
  }
  return view;
}

function action(view, text) {
  const node = view.querySelectorAll('button').find((node) => node.textContent === text);
  assert.ok(node, `Expected ${text} action`);
  return node;
}

test('ban dismissal remains blocked until acknowledgement, then the owned dialog closes', async () => {
  const f = await fixture();
  const pending = deferred();
  const calls = [];
  f.state.room = {
    membershipVersion: 1,
    ban: (...args) => {
      calls.push(args);
      return pending.promise;
    },
  };
  f.community.ban('target', 'Participant');
  const view = dialog(f, 'Ban Participant');
  const reason = control(view, 'Reason (optional)');
  const duration = control(view, 'Duration');
  reason.value = 'Repeated interruption';
  duration.value = '3600';
  const submit = action(view, 'Ban from room');
  submit.click();
  submit.click();
  await flush();
  assert.deepEqual(calls, [['target', 'Repeated interruption', 3600]]);
  assert.equal(reason.disabled, true);
  assert.equal(duration.disabled, true);
  assert.equal(submit.disabled, true);
  action(view, 'Close').click();
  view.emit('click', { clientX: 0, clientY: 0 });
  let prevented = false;
  view.emit('cancel', {
    preventDefault: () => {
      prevented = true;
    },
  });
  assert.equal(prevented, true);
  assert.equal(view.open, true);
  pending.resolve();
  await flush();
  assert.equal(view.open, false);
  assert.equal(view.isConnected, false);
});

test('ban rejection retains reason and duration with a visible error and working retry', async () => {
  const f = await fixture();
  const first = deferred();
  const second = deferred();
  const calls = [];
  f.state.room = {
    membershipVersion: 1,
    ban: (...args) => {
      calls.push(args);
      return calls.length === 1 ? first.promise : second.promise;
    },
  };
  f.community.ban('target', 'Participant');
  const view = dialog(f, 'Ban Participant');
  const reason = control(view, 'Reason (optional)');
  const duration = control(view, 'Duration');
  const submit = action(view, 'Ban from room');
  reason.value = 'Keep this explanation';
  duration.value = '86400';
  submit.click();
  first.reject(new Error('The change was not confirmed'));
  await flush();
  assert.equal(view.open, true);
  assert.equal(view.children[1].hidden, false);
  assert.match(view.children[1].textContent, /not confirmed/);
  assert.equal(reason.value, 'Keep this explanation');
  assert.equal(duration.value, '86400');
  assert.equal(reason.disabled, false);
  assert.equal(duration.disabled, false);
  assert.equal(submit.disabled, false);
  submit.click();
  await flush();
  assert.deepEqual(calls, [
    ['target', 'Keep this explanation', 86400],
    ['target', 'Keep this explanation', 86400],
  ]);
  assert.equal(view.children[1].hidden, true);
  second.resolve();
  await flush();
  assert.equal(view.isConnected, false);
});

test('a stale ban form closes without submitting against a replacement membership', async () => {
  const f = await fixture();
  let requests = 0;
  f.state.room = {
    membershipVersion: 1,
    ban: async () => {
      requests++;
    },
  };
  f.community.ban('target', 'Participant');
  const view = dialog(f, 'Ban Participant');
  f.state.room.membershipVersion++;
  action(view, 'Ban from room').click();
  await flush();
  assert.equal(requests, 0);
  assert.equal(view.isConnected, false);
});

test('a late ban result retires only its old dialog and cannot dismiss a newer pending ban', async () => {
  const f = await fixture();
  const old = deferred();
  f.state.room = { membershipVersion: 1, ban: () => old.promise };
  f.community.ban('old-target', 'Old participant');
  const oldView = dialog(f, 'Ban Old participant');
  action(oldView, 'Ban from room').click();
  const current = deferred();
  f.state.room = { membershipVersion: 2, ban: () => current.promise };
  f.community.ban('new-target', 'New participant');
  const currentView = dialog(f, 'Ban New participant');
  action(currentView, 'Ban from room').click();
  old.reject(new Error('Old response'));
  await flush();
  assert.equal(oldView.isConnected, false);
  assert.equal(currentView.open, true);
  assert.equal(action(currentView, 'Ban from room').disabled, true);
  assert.equal(currentView.children[1].hidden, true);
  action(currentView, 'Close').click();
  assert.equal(currentView.open, true);
  current.resolve();
  await flush();
  assert.equal(currentView.isConnected, false);
});

test('recovery validates normalized character and byte bounds before sending', async () => {
  const f = await fixture();
  f.community.openRecovery();
  const view = dialog(f, 'Recover account');
  const password = control(view, 'New password');
  const confirm = control(view, 'Confirm new password');
  const submit = action(view, 'Reset password with key');
  for (const [value, confirmation, error] of [
    ['1234567', '1234567', /15 and 128 characters/],
    ['a'.repeat(129), 'a'.repeat(129), /15 and 128 characters/],
    ['😀'.repeat(129), '😀'.repeat(129), /15 and 128 characters/],
    ['long-password-one', 'long-password-two', /do not match/],
    ['long-password-123\t', 'long-password-123\t', /control characters/],
    ['long-password-123\u0085', 'long-password-123\u0085', /control characters/],
  ]) {
    password.value = value;
    confirm.value = confirmation;
    submit.click();
    await flush();
    assert.equal(f.state.requests.length, 0);
    assert.match(view.children[1].textContent, error);
    assert.equal(submit.disabled, false);
  }
  control(view, 'Email').value = ' a@example.test ';
  const key = control(view, 'Saved recovery key');
  key.value = ' saved-test-key ';
  password.value = confirm.value = 'é'.repeat(64); // Exactly 128 UTF-8 bytes.
  submit.click();
  await flush();
  assert.deepEqual(f.state.requests[0], [
    '/api/auth/recovery/redeem',
    null,
    'POST',
    {
      email: 'a@example.test',
      recovery_key: 'saved-test-key',
      new_password: 'é'.repeat(64),
    },
  ]);
  assert.equal(key.value, '');
  assert.equal(view.isConnected, false);
  assert.match(f.state.notifications[0], /Password reset/);
});

test('recovery accepts normalized equivalent confirmation without altering the request', async () => {
  const f = await fixture();
  f.community.openRecovery();
  const view = dialog(f, 'Recover account');
  control(view, 'New password').value = 'e\u0301'.repeat(15);
  control(view, 'Confirm new password').value = 'é'.repeat(15);
  action(view, 'Reset password with key').click();
  await flush();
  assert.equal(f.state.requests[0][3].new_password, 'e\u0301'.repeat(15));
});

test('profile text remains text and remote avatar URLs are not rendered', async () => {
  const f = await fixture();
  f.state.handle = async () => ({
    ...f.profile,
    display_name: '<b>Alice</b>',
    bio: '<p>Biography</p>',
    avatar_url: 'https://example.test/avatar.png',
  });
  await f.community.showProfile('account-a');
  const view = dialog(f, 'Profile');
  assert.equal(view.querySelectorAll('h3')[0].textContent, '<b>Alice</b>');
  assert.equal(view.querySelectorAll('p')[1].textContent, '<p>Biography</p>');
  assert.equal(view.querySelectorAll('img').length, 0);
});

test("the own avatar paints its initial on the name tone at once, then the profile's chosen color or picture", async () => {
  const f = await fixture();
  const { avatarColors } = await loadTypeScript('src/avatar-colors.ts');
  const pending = deferred();
  f.state.handle = () => pending.promise;
  const avatar = f.ui.el('span');
  f.document.body.append(avatar);
  f.community.decorateOwnAvatar(avatar, 'account-a', 'alice example');
  assert.equal(avatar.textContent, 'A');
  assert.deepEqual(
    { background: avatar.style.background, color: avatar.style.color },
    avatarColors('alice example'),
  );
  pending.resolve({
    ...f.profile,
    avatar_url: null,
    profile_style: { color: 'violet', style: 'accent' },
  });
  await flush();
  assert.equal(avatar.textContent, 'A');
  assert.deepEqual(
    { background: avatar.style.background, color: avatar.style.color },
    avatarColors('alice example', 'violet'),
  );
  assert.equal(f.state.requests.length, 1);
  const pictured = f.ui.el('span');
  f.document.body.append(pictured);
  f.community.profiles.clear();
  const picture = deferred();
  f.state.handle = () => picture.promise;
  f.community.decorateOwnAvatar(pictured, 'account-a', 'alice example');
  picture.resolve({ ...f.profile, avatar_url: 'data:image/jpeg;base64,YQ==' });
  await flush();
  assert.equal(pictured.children[0].tagName, 'IMG');
  assert.equal(pictured.children[0].className, 'account-avatar');
});

test('avatars decorate connected authenticated nodes only and retain text for guests', async () => {
  const f = await fixture();
  const pending = deferred();
  f.state.handle = () => pending.promise;
  const guest = f.ui.el('div', 'Guest');
  f.document.body.append(guest);
  f.community.decorateAvatar(guest, 'guest-id', false);
  assert.equal(f.state.requests.length, 0);
  const live = f.ui.el('div', 'A');
  f.document.body.append(live);
  const removed = f.ui.el('div', 'A');
  f.document.body.append(removed);
  f.community.decorateAvatar(live, 'account-a', true);
  f.community.decorateAvatar(removed, 'account-a', true);
  removed.remove();
  pending.resolve({ ...f.profile, avatar_url: 'data:image/jpeg;base64,YQ==' });
  await flush();
  assert.equal(f.state.requests.length, 1, 'profile lookups should be shared');
  assert.equal(live.children[0].tagName, 'IMG');
  assert.equal(live.children[0].alt, '');
  assert.equal(removed.textContent, 'A');
  assert.equal(guest.textContent, 'Guest');
});

test('closing a profile while it loads prevents detached content from being rendered', async () => {
  const f = await fixture();
  const pending = deferred();
  f.state.handle = () => pending.promise;
  const opening = f.community.showProfile('account-a');
  const view = dialog(f, 'Profile');
  view.close();
  pending.resolve({ ...f.profile, avatar_url: 'data:image/jpeg;base64,YQ==' });
  await opening;
  assert.equal(view.querySelectorAll('img').length, 0);
  assert.equal(view.querySelectorAll('h3').length, 0);
});

test('a room image picker reports no change until an upload or a removal', async () => {
  const f = await fixture();
  const error = f.ui.el('p');
  const initial = '/api/rooms/lobby/image?v=0123456789abcdef';
  const picker = f.community.imagePicker(initial, error, true);
  const preview = picker.wrapper.querySelectorAll('img')[0];
  assert.equal(preview.hidden, false, 'the current image shows from its endpoint path');
  assert.equal(preview.src, initial);
  assert.equal(picker.value(), initial);
  assert.equal(picker.change(), undefined, 'untouched: the server keeps the current image');
  action(picker.wrapper, 'Remove image').click();
  assert.equal(picker.change(), null, 'removed: the server clears the image');
  assert.equal(preview.hidden, true);
});

test('image removal wins over a pending upload and cannot be undone by its late result', async () => {
  const f = await fixture();
  const pending = deferred();
  f.state.upload = () => pending.promise;
  const error = f.ui.el('p');
  const picker = f.community.imagePicker('data:image/jpeg;base64,YQ==', error);
  const file = picker.wrapper.querySelectorAll('input')[0];
  file.files = [{ type: 'image/png', size: 1024 }];
  file.emit('change');
  assert.throws(() => picker.value(), /finish processing/);
  action(picker.wrapper, 'Remove image').click();
  pending.resolve('data:image/jpeg;base64,Yg==');
  await flush();
  assert.equal(picker.value(), null);
  assert.equal(picker.wrapper.querySelectorAll('img')[0].hidden, true);
  assert.equal(file.disabled, false);
});

test('invalid uploads preserve the current image and display the validation failure safely', async () => {
  const f = await fixture();
  const error = f.ui.el('p');
  const picker = f.community.imagePicker('data:image/jpeg;base64,YQ==', error);
  f.document.body.append(error, picker.wrapper);
  const file = picker.wrapper.querySelectorAll('input')[0];
  file.files = [{ type: 'image/svg+xml', size: 100 }];
  file.emit('change');
  await flush();
  assert.equal(picker.value(), 'data:image/jpeg;base64,YQ==');
  assert.match(error.textContent, /Choose a PNG, JPEG or WebP/);
  assert.equal(error.hidden, false);
  assert.equal(file.disabled, false);
});

for (const [label, method] of [['My rooms', 'openRooms']]) {
  test(`${label} click failures are handled in context and ignored after an identity change`, async () => {
    const f = await fixture();
    const first = deferred();
    f.community[method] = () => first.promise;
    action(f.header, label).click();
    first.reject(new Error('Current action failed'));
    await flush();
    assert.deepEqual(f.state.notifications, ['Current action failed']);

    const second = deferred();
    f.community[method] = () => second.promise;
    action(f.header, label).click();
    f.auth.userId = 'account-b';
    f.auth.jwt = 'token-b';
    f.community.refresh();
    second.reject(new Error('Retired action failed'));
    await flush();
    assert.deepEqual(f.state.notifications, ['Current action failed']);
  });
}

test('a left Profile screen does not receive late load failures', async () => {
  const f = await fixture();
  const pending = deferred();
  f.state.handle = () => pending.promise;
  const view = await accountScreen(f, 'profile', { settle: false });
  view.leave();
  pending.reject(new Error('Late account failure'));
  const body = await view.opening;
  assert.equal(body.querySelector('.auth-error'), null);
});

test('passkey-only accounts retain invitations, sessions and forget-device controls', async () => {
  const f = await fixture();
  const handle = f.state.handle;
  f.state.handle = (path, ...args) =>
    path === '/api/auth/passkeys'
      ? {
          password_enabled: false,
          recovery_enabled: false,
          passkeys: [{ id: 'key', created_at: '2026-10-07T10:00:00Z' }],
          maximum: 10,
        }
      : handle(path, ...args);
  const security = await accountScreen(f, 'security');
  assert.ok(
    !security.body
      .querySelectorAll('button')
      .some((node) => node.textContent === 'Change password'),
  );
  action(security.body, 'Sign out this session');
  action(security.body, 'Sign out and forget this device').click();
  assert.equal(f.state.forgotten, 1);
  assert.equal(security.open, false);
  const invites = await accountScreen(f, 'invites');
  action(invites.body, 'New invite code');
});

test('the Account button opens the sheet at Profile and the list names the account', async () => {
  const f = await fixture();
  f.header
    .querySelectorAll('button')
    .find((node) => node.textContent === 'Account')
    .click();
  assert.deepEqual(f.state.openedSettings, ['profile']);
  const sections = f.community.sections();
  assert.deepEqual(
    sections.map((section) => [section.id, section.group]),
    [
      ['profile', 'Account'],
      ['security', 'Account'],
      ['invites', 'Account'],
    ],
  );
  f.auth.isLoggedIn = false;
  assert.equal(
    sections.every((section) => section.available() === false),
    true,
  );
});

test('session revocation uses refreshed credentials, owns the screen and signs out only its current identity', async () => {
  const f = await fixture();
  const view = await accountScreen(f, 'security');
  const pending = deferred();
  f.state.handle = () => pending.promise;
  f.auth.jwt = 'refreshed-token';
  const revoke = action(view.body, 'Sign out this session');
  revoke.click();
  revoke.click();
  const calls = f.state.requests.filter((request) => request[2] === 'DELETE');
  assert.equal(calls.length, 1);
  assert.equal(calls[0][1], 'refreshed-token');
  assert.equal(action(view.body, 'Change password').disabled, true);
  assert.equal(view.screen.canLeave(), false, 'a pending mutation refuses to leave');
  pending.resolve();
  await flush();
  assert.equal(f.state.signedOut, 1);
  assert.equal(view.open, false);
});

test('a late revoke completion cannot affect a new account', async () => {
  const f = await fixture();
  const view = await accountScreen(f, 'security');
  const pending = deferred();
  f.state.handle = () => pending.promise;
  action(view.body, 'Sign out this session').click();
  f.auth.userId = 'account-b';
  f.auth.jwt = 'token-b';
  f.community.refresh();
  assert.equal(f.state.accountChanges, 1, 'an identity change closes the sheet');
  view.leave();
  pending.resolve();
  await flush();
  assert.equal(f.state.signedOut, 0);
  assert.ok(!f.state.notifications.includes('Session signed out.'));
});

test('leaving Sign-in & security cancels the session list and discards its late response', async () => {
  const f = await fixture();
  const handle = f.state.handle;
  const pending = deferred();
  f.state.handle = (path, ...args) =>
    path === '/api/auth/sessions' ? pending.promise : handle(path, ...args);
  const view = await accountScreen(f, 'security');
  const request = f.state.requests.find((request) => request[0] === '/api/auth/sessions');
  view.leave();
  assert.equal(request[4].aborted, true);
  pending.resolve([
    {
      id: 'late-session',
      current: true,
      created_at: '2026-10-07T10:00:00Z',
      refreshed_at: '2026-10-07T10:00:00Z',
      expires_at: '2026-10-14T10:00:00Z',
    },
  ]);
  await flush();
  assert.equal(
    view.body
      .querySelectorAll('button')
      .filter((node) => node.textContent === 'Sign out this session').length,
    0,
  );
});

test('other-session sign-out keeps this session and refreshes the list', async () => {
  const f = await fixture();
  const handle = f.state.handle;
  let removed = false;
  f.state.handle = async (path, ...args) => {
    if (path === '/api/auth/sessions/others') {
      removed = true;
      return;
    }
    const result = await handle(path, ...args);
    if (path === '/api/auth/sessions' && !removed)
      return [...result, { ...result[0], id: 'other-session', current: false }];
    return result;
  };
  const view = await accountScreen(f, 'security');
  action(view.body, 'Sign out other sessions').click();
  await flush();
  assert.equal(removed, true);
  assert.equal(f.state.signedOut, 0);
  assert.equal(view.open, true);
  assert.equal(action(view.body, 'Sign out other sessions').disabled, true);
  assert.equal(
    view.body.querySelectorAll('button').filter((node) => node.textContent === 'Sign out session')
      .length,
    0,
  );
});

test('Profile saves the separate hovercard appearance and updates its live name preview', async () => {
  const f = await fixture();
  f.profile.profile_style = { color: 'teal', style: 'bubble' };
  const view = await accountScreen(f, 'profile');
  const picker = view.body.querySelector('.appearance-picker');
  const name = control(view.body, 'Name');
  name.value = 'Changed account name';
  name.emit('input');
  assert.equal(picker.querySelector('.appearance-preview').textContent, 'Changed account name');
  assert.equal(picker.querySelector('.appearance-preview').dataset.appearance, 'bubble');
  const radios = picker.querySelectorAll('input');
  for (const radio of radios) {
    radio.checked = radio.name.endsWith('-color')
      ? radio.value === 'violet'
      : radio.value === 'text';
  }
  radios.find((radio) => radio.value === 'violet').emit('change');
  action(view.body, 'Save profile').click();
  await flush();
  const request = f.state.requests.find((entry) => entry[2] === 'PATCH');
  assert.deepEqual(request[3], {
    display_name: 'Changed account name',
    bio: '',
    avatar_url: null,
    profile_style: { color: 'violet', style: 'text' },
  });
  assert.equal('chat_style' in request[3], false);
  assert.equal(f.state.updated.length, 1);
});

test('reopening a hovercard fetches current profile appearance without retaining stale cache', async () => {
  const f = await fixture();
  const first = await f.community.participantProfile('account-a');
  assert.deepEqual(first.profile_style, { color: null, style: 'accent' });
  f.profile.profile_style = { color: 'rose', style: 'text' };
  const second = await f.community.participantProfile('account-a');
  assert.deepEqual(second.profile_style, { color: 'rose', style: 'text' });
  assert.equal(f.state.requests.length, 2);
  assert.equal(f.community.profiles.has('account-a'), false);
});

test('identity changes close account screens and reject actions from stale controls', async () => {
  const f = await fixture();
  const view = await accountScreen(f, 'profile');
  const save = action(view.body, 'Save profile');
  f.auth.userId = 'account-b';
  f.auth.jwt = 'token-b';
  f.community.refresh();
  assert.equal(f.state.accountChanges, 1);
  view.leave();
  save.click();
  await flush();
  assert.equal(
    f.state.requests.filter((request) => request[2] && request[2] !== 'GET').length,
    0,
    'stale account controls must not submit mutations',
  );
  assert.equal(f.state.updated.length, 0);
});

test('leaving a room closes management dialogs without affecting auth identity', async () => {
  const f = await fixture();
  f.state.room = {
    currentRoomId: 'one',
    localParticipantId: 'account-a',
    nickname: 'Alice',
    role: 'owner',
  };
  f.community.refresh();
  f.community.openNickname();
  const view = dialog(f, 'Room nickname');
  f.state.room = null;
  f.community.refresh();
  assert.equal(view.open, false);
  assert.equal(f.auth.userId, 'account-a');
});

test('an initial account response cannot render after an identity change', async () => {
  const f = await fixture();
  const pending = deferred();
  f.state.handle = () => pending.promise;
  const view = await accountScreen(f, 'profile', { settle: false });
  f.auth.userId = 'account-b';
  f.auth.jwt = 'token-b';
  f.community.refresh();
  pending.resolve(f.profile);
  const body = await view.opening;
  assert.equal(body.querySelectorAll('input').length, 0);
});

test('recovery key is shown once, copies exactly, and clears when the screen is left', async () => {
  const f = await fixture();
  const account = await accountScreen(f, 'security');
  control(account.body, 'Current password for verification').value = 'current-password';
  const result = { kind: 'recovery_key', recovery_key: 'sc-recovery-test-only' };
  f.state.handle = async () => result;
  action(account.body, 'Generate recovery key').click();
  await flush();
  assert.deepEqual(f.state.requests.at(-1), [
    '/api/auth/passkeys/start',
    'token-a',
    'POST',
    { operation: { action: 'recovery_key' }, current_password: 'current-password' },
  ]);
  const key = control(account.body, 'Recovery key');
  assert.equal(key.readOnly, true);
  action(account.body, 'Copy recovery key').click();
  await flush();
  assert.equal(f.state.copied, 'sc-recovery-test-only');
  assert.equal(
    f.created.filter((node) => node.type === 'password').every((node) => node.value === ''),
    true,
  );
  account.leave();
  assert.equal(key.value, '');
  assert.equal(result.recovery_key, '');
});

for (const kind of ['removed', 'replaced']) {
  test(`confirmed passkey ${kind} closes the screen and reports the matching sign-in guidance`, async () => {
    const f = await fixture();
    const account = await accountScreen(f, 'security');
    f.state.securityCompleted(kind);
    await flush();
    assert.equal(account.open, false);
    assert.equal(f.state.signedOut, 1);
    assert.deepEqual(f.state.notifications, [
      kind === 'replaced'
        ? 'Passkey replaced. Sign in again with your new passkey.'
        : 'Passkey removed. Sign in again with a remaining sign-in method.',
    ]);
  });
}

for (const reason of ['leave', 'identity change']) {
  test(`a recovery-key response after ${reason} does not open another dialog`, async () => {
    const f = await fixture();
    const account = await accountScreen(f, 'security');
    control(account.body, 'Current password for verification').value = 'current-password';
    const pending = deferred();
    f.state.handle = () => pending.promise;
    action(account.body, 'Generate recovery key').click();
    await flush();
    if (reason === 'leave') account.leave();
    else {
      f.auth.userId = 'account-b';
      f.auth.jwt = 'token-b';
      f.community.refresh();
      account.leave();
    }
    const result = { kind: 'recovery_key', recovery_key: 'sc-recovery-test-only' };
    pending.resolve(result);
    await flush();
    assert.equal(f.document.querySelectorAll('dialog').length, 0);
    assert.equal(result.recovery_key, '');
  });
}

for (const changedIdentity of [false, true]) {
  test(`successful pending password change ${changedIdentity ? 'does not sign out a new identity' : 'blocks leaving until the same account is signed out'}`, async () => {
    const f = await fixture();
    const view = await accountScreen(f, 'security');
    control(view.body, 'Current password').value = 'current-password';
    control(view.body, 'New password').value = control(view.body, 'Confirm new password').value =
      'new-long-password';
    const pending = deferred();
    f.state.handle = () => pending.promise;
    action(view.body, 'Change password').click();
    await flush();
    assert.equal(f.state.requests.at(-1)[1], 'token-a');
    assert.equal(view.screen.canLeave(), false, 'a submitted password mutation owns leaving');
    if (changedIdentity) {
      f.auth.userId = 'account-b';
      f.auth.jwt = 'token-b';
      f.community.refresh();
      view.leave();
    }
    pending.resolve(undefined);
    await flush();
    assert.equal(f.state.signedOut, changedIdentity ? 0 : 1);
  });
}

test('room links preserve the current origin and path while removing unrelated query and fragment', async () => {
  const f = await fixture();
  assert.equal(f.roomLink('friendly room'), 'http://localhost:3000/#friendly%20room');
});

test('password and passkey changes share ownership and stale controls cannot submit concurrently', async () => {
  const f = await fixture();
  const account = await accountScreen(f, 'security');
  const staleAdd = action(account.body, 'Add passkey');
  const change = action(account.body, 'Change password');
  control(account.body, 'Current password').value = 'old-password';
  control(account.body, 'New password').value = control(
    account.body,
    'Confirm new password',
  ).value = 'new-long-password';
  const pending = deferred();
  f.state.handle = () => pending.promise;
  change.click();
  staleAdd.click();
  await flush();
  assert.equal(f.state.requests.filter((request) => request[2] === 'POST').length, 1);
  assert.equal(f.state.requests.at(-1)[0], '/api/auth/password');
  assert.equal(change.disabled, true);
  f.auth.userId = 'account-b';
  f.community.refresh();
  account.leave();
  pending.resolve();
  await flush();
  assert.equal(f.state.signedOut, 0);
});

test('a profile save re-enables its control on completion while an uncertain security mutation keeps its screen locked', async () => {
  const f = await fixture();
  const profile = await accountScreen(f, 'profile');
  const save = action(profile.body, 'Save profile');
  const pending = deferred();
  f.state.handle = () => pending.promise;
  save.click();
  save.click();
  await flush();
  assert.equal(f.state.requests.at(-1)[0], '/api/auth/profile');
  assert.equal(f.state.requests.at(-1)[2], 'PATCH');
  assert.equal(f.state.requests.filter((request) => request[2] === 'PATCH').length, 1);
  assert.equal(profile.screen.canLeave(), false);
  pending.resolve({ ...f.profile });
  await flush();
  assert.equal(save.disabled, false);
  assert.equal(profile.screen.canLeave(), true);
  profile.leave();
  f.state.handle = async (path) =>
    path === '/api/auth/passkeys'
      ? { password_enabled: true, recovery_enabled: false, passkeys: [], maximum: 10 }
      : path === '/api/auth/sessions'
        ? []
        : path === '/api/auth/invites'
          ? []
          : { ...f.profile };
  const account = await accountScreen(f, 'security');
  control(account.body, 'Current password for verification').value = 'old-password';
  f.state.handle = async () => {
    throw new Error('Lost mutation response');
  };
  action(account.body, 'Generate recovery key').click();
  await flush();
  assert.equal(action(account.body, 'Change password').disabled, true);
  assert.equal(account.screen.canLeave(), false);
  assert.match(account.body.textContent, /may have completed/);
});

for (const phase of ['pending', 'uncertain', 'recovery']) {
  test(`same-account room membership changes preserve ${phase} account ownership`, async () => {
    const f = await fixture();
    const account = await accountScreen(f, 'security');
    control(account.body, 'Current password for verification').value = 'old-password';
    const pending = deferred();
    f.state.handle = () => pending.promise;
    action(account.body, 'Generate recovery key').click();
    await flush();
    if (phase === 'uncertain') pending.reject(new Error('Response lost'));
    if (phase === 'recovery') pending.resolve({ kind: 'recovery_key', recovery_key: 'save-once' });
    await flush();
    f.state.room = {
      currentRoomId: 'different-room',
      localParticipantId: 'new-membership',
      role: 'user',
    };
    f.community.refresh();
    assert.equal(f.state.accountChanges, 0, 'a room change keeps the account screen');
    assert.equal(account.open, true);
    if (phase === 'pending' || phase === 'uncertain')
      assert.equal(account.screen.canLeave(), false);
    else assert.equal(control(account.body, 'Recovery key').value, 'save-once');
    f.auth.userId = 'account-b';
    f.auth.jwt = 'token-b';
    f.community.refresh();
    assert.equal(f.state.accountChanges, 1);
    account.leave();
    if (phase === 'pending') pending.resolve({ kind: 'recovery_key', recovery_key: 'late-secret' });
    await flush();
    assert.ok(
      f.created
        .filter((node) => node.tagName === 'TEXTAREA' && node.readOnly)
        .every((node) => node.value === ''),
    );
  });
}

test('room-scoped actions mount in the room tools while account actions stay in the header', async () => {
  const dom = await uiFixture();
  const roomActions = dom.document.createElement('div');
  roomActions.id = 'room-actions';
  dom.document.body.append(roomActions);
  const security = await loadTypeScript('src/account-security.ts', {
    modules: { './ui': dom.ui, './auth': await loadTypeScript('src/auth.ts') },
    globals: { document: dom.document, navigator: { clipboard: { writeText: async () => {} } } },
  });
  const api = await loadTypeScript('src/community-ui.ts', {
    modules: {
      './account-updates': accountUpdatesFixture().module,
      './ui': { ...dom.ui, api: { ...dom.ui.api, inboxUnread: async () => ({ unreadCount: 0 }) } },
      './settings-sheet': {},
      './room-administration': {
        administrationSections() {
          return [];
        },
      },
      './room-administration-api': { roomAdministrationApi: {} },
      './chat-history': { openPrivateInbox() {}, onInboxChanged() {} },
      './discovery': await loadDiscoveryFixture(dom),
      './account-security': security,
      './account-sessions': await loadTypeScript('src/account-sessions.ts', {
        modules: { './ui': dom.ui },
      }),
      './appearance': await loadAppearanceFixture(dom),
      './avatar-colors': await loadTypeScript('src/avatar-colors.ts'),
    },
    globals: {
      document: dom.document,
      TextEncoder,
      URL,
      window: { location: { href: 'http://localhost:3000/' } },
      navigator: { clipboard: { writeText: async () => {} } },
    },
  });
  new api.CommunityUI({
    auth: { isLoggedIn: true, userId: 'account-a', jwt: 'token-a' },
    getRoom: () => null,
    notify() {},
    onProfileChanged() {},
    onRoomsChanged() {},
    async onRoomDeleted() {},
    async onSignedOut() {},
  });
  assert.deepEqual(
    dom.header.children.map((node) => node.textContent),
    ['Account', 'Messages', 'My rooms'],
  );
  assert.deepEqual(
    roomActions.children.map((node) => node.textContent),
    ['Nickname', 'Manage room'],
  );
});

test('roster profiles deduplicate above 100 people, bound concurrency, and evict departed members', async () => {
  const f = await fixture();
  const pending = deferred();
  f.state.handle = () => pending.promise;
  const ids = Array.from({ length: 300 }, (_, index) => `person-${index}`);
  const paint = () => {
    for (const id of ids) {
      const avatar = f.document.createElement('div');
      f.document.body.append(avatar);
      f.community.decorateAvatar(avatar, id, true);
    }
  };
  f.community.retainProfiles(ids);
  paint();
  paint();
  assert.equal(f.state.requests.length, 8, 'pending profile reads have a concurrency ceiling');
  assert.equal(f.community.profiles.size, 300, 'queued and active reads share the same cache');
  pending.resolve({ ...f.profile, avatar_url: null });
  await Promise.all(f.community.profiles.values());
  assert.equal(f.state.requests.length, 300, 'exactly one read per account');
  paint();
  await flush();
  assert.equal(f.state.requests.length, 300, 'unchanged redraws do not fetch profiles again');
  f.community.retainProfiles(ids.slice(1));
  assert.equal(f.community.profiles.has(ids[0]), false);
  assert.equal(f.community.profiles.size, 299);
});

test('very large rosters keep bounded avatars without evicting active cached profiles on redraw', async () => {
  const f = await fixture();
  const ids = Array.from({ length: 700 }, (_, index) => `person-${index}`);
  const paint = () => {
    f.community.retainProfiles(ids);
    for (const id of ids) f.community.decorateAvatar(f.document.createElement('div'), id, true);
  };
  paint();
  await Promise.all(f.community.profiles.values());
  assert.equal(f.state.requests.length, 512);
  paint();
  await flush();
  assert.equal(f.state.requests.length, 512);
  assert.equal(f.community.profiles.size, 512);
});

test('retired queued profile reads do not run against a replacement room', async () => {
  const f = await fixture();
  const pending = deferred();
  f.state.handle = () => pending.promise;
  const ids = Array.from({ length: 20 }, (_, index) => `old-person-${index}`);
  f.community.retainProfiles(ids);
  for (const id of ids) f.community.decorateAvatar(f.document.createElement('div'), id, true);
  const work = Promise.all(f.community.profiles.values());
  f.state.room = { currentRoomId: 'new-room', localParticipantId: 'local', role: 'user' };
  f.community.refresh();
  pending.resolve(null);
  await work;
  assert.equal(f.state.requests.length, 8, 'only reads already sent may settle after replacement');
  assert.equal(f.community.profiles.size, 0);
});

test('explicit profile visits take the next slot before queued avatar decoration', async () => {
  const f = await fixture();
  const active = Array.from({ length: 8 }, () => deferred());
  let index = 0;
  f.state.handle = () => active[index++]?.promise ?? Promise.resolve(f.profile);
  const ids = Array.from({ length: 30 }, (_, number) => `avatar-${number}`);
  f.community.retainProfiles(ids);
  for (const id of ids) f.community.decorateAvatar(f.document.createElement('div'), id, true);
  const shown = f.community.showProfile('requested-account');
  assert.equal(f.state.requests.length, 8);
  active[0].resolve(f.profile);
  await flush();
  assert.equal(f.state.requests[8][0], '/api/auth/profiles/requested-account');
  for (const pending of active.slice(1)) pending.resolve(f.profile);
  await shown;
  await Promise.all(f.community.profiles.values());
});

test('membership pages append unique rooms and keep the cursor on a failed read', async () => {
  const f = await fixture();
  const requests = [];
  let failNext = false;
  const member = (id) => ({ id, display_name: id, role: 'member', participant_count: 1 });
  // Named endpoint contracts are tested separately; this fixture observes dialog ownership.
  f.community.options.onJoinRoom = (id) => requests.push(['join', id]);
  const original = f.community.openRooms;
  assert.equal(typeof original, 'function');
  f.state.handle = async (path) => {
    if (path === '/api/rooms/mine') return [];
    throw new Error('Unexpected fixture request');
  };
  // The shared fixture's API object is supplied by reference to this instance.
  f.community.options.auth.jwt = 'token';
  f.ui.api.memberships = async (_token, cursor) => {
    requests.push(['page', cursor]);
    if (failNext) {
      failNext = false;
      throw new Error('Temporary read failure');
    }
    return cursor
      ? { items: [member('first'), member('second')], next_cursor: null }
      : { items: [member('first')], next_cursor: 'first' };
  };
  await f.community.openRooms();
  const view = dialog(f, 'My rooms');
  const more = view
    .querySelectorAll('button')
    .find((node) => node.textContent === 'Load more memberships');
  assert.ok(more);
  failNext = true;
  more.click();
  await flush();
  assert.equal(more.disabled, false);
  more.click();
  await flush();
  assert.equal(more.hidden, true);
  assert.equal(view.querySelectorAll('.owned-room').length, 2);
  assert.deepEqual(requests, [
    ['page', undefined],
    ['page', 'first'],
    ['page', 'first'],
  ]);
});

test('My rooms manages the selected saved room while another call stays joined', async () => {
  const f = await fixture();
  const joined = {
    currentRoomId: 'active-call',
    role: 'user',
    requestSocial() {
      assert.fail('Saved-room administration must use its selected HTTP room');
    },
  };
  f.state.room = joined;
  f.ui.api.memberships = async () => ({ items: [], next_cursor: null });
  const selected = { id: 'saved-room', display_name: 'Saved room', participant_count: 0 };
  const overview = {
    role: 'owner',
    settings: {
      id: 'saved-room',
      displayName: 'Saved room',
      guestsAllowed: true,
      requireRegistration: false,
      inviteOnly: false,
      secret: false,
      lobbyEnabled: false,
      passwordProtected: false,
    },
  };
  f.state.handle = async (path) => {
    if (path === '/api/rooms/mine') return [selected];
    if (path === '/api/rooms/saved-room/administration') return overview;
    throw new Error(`Unexpected selected-room request: ${path}`);
  };
  await f.community.openRooms();
  const rooms = dialog(f, 'My rooms');
  action(rooms, 'Manage room').click();
  await flush();
  assert.equal(rooms.open, false);
  assert.equal(f.community.hasRoomAdministration, true);
  assert.deepEqual(f.state.openedSettings, ['room-access']);
  const section = f.community.roomSections().find((entry) => entry.id === 'room-access');
  const screen = section.screen();
  assert.match(screen.subtitle, /Saved room · saved-room/);
  const body = await screen.body({ current: () => true });
  await flush();
  assert.match(body.textContent, /Allow guests/);
  assert.equal(f.state.room, joined);
  assert.equal(
    f.state.requests.filter(([path]) => path === '/api/rooms/saved-room/administration').length,
    2,
    'Each screen revalidates saved authorization',
  );
  screen.leave();
  f.community.closeRoomContext();
  assert.equal(f.community.hasRoomAdministration, false);
  assert.equal(f.state.room, joined);
});

test('a late administration overview cannot reopen a closed My rooms dialog or replace accounts', async () => {
  const f = await fixture();
  f.ui.api.memberships = async () => ({ items: [], next_cursor: null });
  const read = deferred();
  f.state.handle = async (path) =>
    path === '/api/rooms/mine'
      ? [{ id: 'saved-room', display_name: 'Saved room', participant_count: 0 }]
      : read.promise;
  await f.community.openRooms();
  const rooms = dialog(f, 'My rooms');
  action(rooms, 'Manage room').click();
  await flush();
  rooms.close();
  f.auth.userId = 'account-b';
  read.resolve({ role: 'owner', settings: { id: 'saved-room' } });
  await flush();
  assert.equal(f.community.hasRoomAdministration, false);
  assert.deepEqual(f.state.openedSettings, []);
});

test('Edit room keeps the room chosen from My rooms even while joined elsewhere', async () => {
  const f = await fixture();
  f.state.room = { currentRoomId: 'active-call', role: 'user', localParticipantId: 'account-a' };
  const selected = {
    id: 'saved-room',
    display_name: 'Saved room',
    topic: null,
    description: '',
    image_url: null,
    name_style: { color: null, style: 'accent' },
    topic_style: { color: null, style: 'accent' },
  };
  f.ui.api.memberships = async () => ({ items: [], next_cursor: null });
  f.state.handle = async (path) => {
    if (path === '/api/rooms/mine') return [selected];
    if (path === '/api/rooms/saved-room/identity') return selected;
    throw new Error(`Unexpected identity request: ${path}`);
  };
  await f.community.openRooms();
  action(dialog(f, 'My rooms'), 'Edit room').click();
  const section = f.community.roomSections().find((entry) => entry.id === 'room-identity');
  assert.equal(
    section.available(),
    true,
    'the other call’s role does not replace ownership of the selected room',
  );
  assert.equal(f.community.hasSelectedRoom, true);
  const screen = section.screen();
  assert.match(screen.subtitle, /Saved room · saved-room/);
  const body = await screen.body({ current: () => true, close() {} });
  const name = control(body, 'Name');
  assert.equal(name.value, 'Saved room');
  name.value = 'Renamed saved room';
  name.emit('change');
  await flush();
  assert.equal(f.state.requests.at(-1)[0], '/api/rooms/saved-room/identity');
  assert.equal(f.state.room.currentRoomId, 'active-call');
});

test('invitation links use only kind-specific fragments and discard the current query', async () => {
  const f = await fixture();
  const code = 'a'.repeat(32);
  for (const [kind, fragment] of [
    ['room', 'invite'],
    ['registration', 'register-invite'],
  ]) {
    const link = new URL(f.inviteLink(code, kind));
    assert.equal(link.search, '');
    assert.equal(link.hash, `#${fragment}=${code}`);
  }
});

test('issued invitation secrets appear only in a one-time share panel; lists revoke by metadata', async () => {
  const f = await fixture();
  f.Node.prototype.focus = function () {};
  f.Node.prototype.select = function () {};
  const view = f.ui.modal('Invitation test');
  const surface = { body: view.body, error: view.error, isOpen: () => view.dialog.open };
  const code = 'a'.repeat(32);
  f.community.showCreatedInvite(code, surface, 'room');
  const panel = view.body.querySelector('.created-invitation');
  assert.equal(panel.querySelector('.invite-code').textContent, code);
  action(panel, 'Copy link').click();
  await flush();
  assert.equal(new URL(f.state.copied).hash, `#invite=${code}`);
  let revoked = 0;
  const row = f.community.inviteRow(
    {
      id: '11111111-1111-4111-8111-111111111111',
      uses_left: 1,
      expires_at: '2026-10-05',
      created_at: '2026-09-30',
      role: 'member',
    },
    surface,
    async () => {},
    async () => {
      revoked++;
    },
  );
  view.body.append(row);
  assert.equal(row.querySelector('.invite-code'), null);
  assert.equal(row.textContent.includes(code), false);
  assert.equal(
    row.querySelectorAll('button').some((node) => node.textContent === 'Copy link'),
    false,
  );
  action(row, 'Revoke').click();
  await flush();
  assert.equal(revoked, 1);
  view.close();
  assert.equal(panel.isConnected, false);
});

test('Messages badge follows confirmed unread counts and fences a replaced account', async () => {
  const f = await fixture();
  await flush();
  f.state.unread = async () => ({ unreadCount: 5 });
  await f.community.refreshMessages();
  const button = f.header.children.find((node) => node.textContent.startsWith('Messages'));
  assert.equal(button.textContent, 'Messages (5)');
  assert.equal(button.getAttribute('aria-label'), 'Messages, 5 unread');
  const pending = deferred();
  f.state.unread = () => pending.promise;
  const previous = f.community.refreshMessages();
  f.auth.userId = 'account-b';
  f.auth.jwt = 'token-b';
  f.state.unread = async () => ({ unreadCount: 2 });
  f.community.refresh();
  await flush();
  pending.resolve({ unreadCount: 700 });
  await previous;
  assert.equal(button.textContent, 'Messages (2)');
  f.state.unread = async () => ({ unreadCount: 0 });
  f.state.inboxListener();
  await flush();
  assert.equal(button.textContent, 'Messages');
});

test('a healthy account feed handles unread invalidation once without duplicate legacy reads', async () => {
  const f = await fixture();
  await flush();
  f.updates.live.add(f.auth.userId);
  const pending = deferred();
  let reads = 0;
  f.state.unread = () => {
    reads++;
    return pending.promise;
  };
  f.state.inboxListener();
  assert.equal(reads, 0, 'legacy socket/local notification relies on the healthy durable feed');
  f.updates.emit({ account: f.auth.userId, scope: 'inbox' });
  f.state.inboxListener();
  assert.equal(reads, 1, 'the same committed change cannot queue a second unread GET');
  pending.resolve({ unreadCount: 3 });
  await flush();
  assert.equal(reads, 1);
  assert.equal(f.community.messagesNeedRefresh, false);
});

test('legacy unread invalidation remains immediate without a healthy current-account feed', async () => {
  const f = await fixture();
  await flush();
  f.updates.live.add('another-account');
  const tokens = [];
  f.state.unread = async (token) => {
    tokens.push(token);
    return { unreadCount: 1 };
  };
  f.state.inboxListener();
  assert.deepEqual(tokens, ['token-a']);
  await flush();
  f.updates.live.add('account-a');
  f.auth.userId = 'account-b';
  f.auth.jwt = 'token-b';
  f.state.inboxListener();
  assert.deepEqual(
    tokens,
    ['token-a', 'token-b'],
    'an old account feed cannot suppress the new account fallback',
  );
  await flush();
});

test('a prior unread failure permits legacy repair even while the account feed stays healthy', async () => {
  const f = await fixture();
  await flush();
  f.updates.live.add(f.auth.userId);
  f.state.unread = async () => {
    throw new Error('Unread unavailable');
  };
  await f.community.refreshMessages();
  assert.equal(f.community.messagesNeedRefresh, true);
  let reads = 0;
  f.state.unread = async () => {
    reads++;
    return { unreadCount: 4 };
  };
  f.state.inboxListener();
  assert.equal(reads, 1, 'a healthy stream cannot conceal a failed authoritative unread read');
  await flush();
  assert.equal(f.community.messagesNeedRefresh, false);
  f.state.inboxListener();
  await flush();
  assert.equal(reads, 1, 'successful repair restores healthy-feed deduplication');
});
