import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

const accountA = '11111111-1111-4111-8111-111111111111';
const accountB = '22222222-2222-4222-8222-222222222222';
const peer = '33333333-3333-4333-8333-333333333333';

async function fixture({
  hash = '#call',
  ready = true,
  account = accountA,
  cryptoApi = globalThis.crypto,
} = {}) {
  const location = { pathname: '/', search: '', hash };
  const entries = [{ url: hash, state: null }];
  let cursor = 0;
  const errors = [];
  const opened = [];
  const state = { ready, account, room: hash === '#call' ? 'call' : '', busy: false };
  const apply = (entry) => {
    location.hash = entry.url;
    history.state = entry.state;
  };
  const history = {
    state: null,
    pushState(value, _title, url) {
      entries.splice(cursor + 1);
      entries.push({ state: value, url: new URL(url, 'https://site.invalid').hash });
      cursor++;
      apply(entries[cursor]);
    },
    replaceState(value, _title, url) {
      entries[cursor] = { state: value, url: new URL(url, 'https://site.invalid').hash };
      apply(entries[cursor]);
    },
    go(delta) {
      const next = Math.max(0, Math.min(entries.length - 1, cursor + delta));
      if (next === cursor) return;
      cursor = next;
      apply(entries[cursor]);
      navigation.locationChanged(state.room);
    },
  };
  const module = await loadTypeScript('src/app-navigation.ts', {
    globals: { window: { location, history }, URLSearchParams, crypto: cryptoApi },
  });
  function open(destination, { transient = false, group, restore } = {}) {
    const handle = { destination, closed: false };
    handle.close = () => {
      if (handle.closed) return;
      handle.closed = true;
      stop();
    };
    const stop = navigation.opened({
      ...(!transient && { destination, accountId: state.account }),
      ...(group && { group }),
      ...(restore && { restore }),
      canLeave: () => !state.busy,
      close: handle.close,
    });
    opened.push(handle);
    return handle;
  }
  const navigation = new module.AppNavigation({
    getRoom: () => state.room,
    getAccountId: () => state.account,
    ready: () => state.ready,
    restore: (destination) =>
      open(destination, destination.kind === 'settings' ? { group: 'settings' } : {}),
    error: (message) => errors.push(message),
  });
  return {
    module,
    navigation,
    location,
    history,
    state,
    opened,
    errors,
    open,
    entries,
    back: () => history.go(-1),
    forward: () => history.go(1),
    visit(hash) {
      history.pushState(null, '', hash);
      return navigation.locationChanged(state.room);
    },
  };
}

test('typed destinations round-trip with a room while legacy room links remain valid', async () => {
  const f = await fixture();
  for (const destination of [
    { kind: 'inbox' },
    { kind: 'conversation', peerId: peer },
    { kind: 'settings', section: 'privacy', segment: 'you' },
  ]) {
    const hash = f.module.appHash('call', destination);
    assert.deepEqual(f.module.parseAppLocation(hash), { room: 'call', destination });
  }
  assert.deepEqual(f.module.parseAppLocation('#legacy-room'), {
    room: 'legacy-room',
    destination: null,
  });
  for (const hash of [
    '#view=conversation&peer=nope',
    '#view=inbox&view=settings',
    '#view=settings&section=../../bad',
    '#room=bad%20room&view=inbox',
    '#invite=secret',
    '#view=inbox&token=secret',
  ])
    assert.equal(f.module.parseAppLocation(hash), null, hash);
});

test('Back closes conversation then inbox before changing the room; Forward reopens read surfaces', async () => {
  const f = await fixture();
  const inbox = f.open({ kind: 'inbox' });
  const conversation = f.open({ kind: 'conversation', peerId: peer, peerName: 'Peer' });
  f.back();
  assert.equal(conversation.closed, true);
  assert.equal(inbox.closed, false);
  assert.equal(f.state.room, 'call');
  f.back();
  assert.equal(inbox.closed, true);
  assert.equal(f.location.hash, '#call');
  f.forward();
  assert.equal(f.opened.at(-1).destination.kind, 'inbox');
  f.forward();
  assert.equal(f.opened.at(-1).destination.kind, 'conversation');
  assert.equal(f.opened.length, 4);
});

test('Close and Escape use the same previous entry, including nested transient confirmations', async () => {
  const f = await fixture();
  const inbox = f.open({ kind: 'inbox' });
  const confirmation = f.open(null, { transient: true });
  f.back();
  assert.equal(confirmation.closed, true);
  assert.equal(inbox.closed, false);
  inbox.close();
  assert.equal(f.location.hash, '#call');
});

test('a busy view refuses Back and leaves room membership unchanged', async () => {
  const f = await fixture();
  const conversation = f.open({ kind: 'conversation', peerId: peer });
  f.state.busy = true;
  f.back();
  assert.equal(conversation.closed, false);
  assert.equal(f.location.hash, `#room=call&view=conversation&peer=${peer}`);
  assert.equal(f.errors.length, 1);
});

test('cold routes wait for verified restoration and never invoke room entry or message submission', async () => {
  const f = await fixture({ hash: `#view=conversation&peer=${peer}`, ready: false, account: null });
  f.navigation.synchronize();
  assert.equal(f.opened.length, 0);
  f.state.ready = true;
  f.navigation.synchronize();
  assert.equal(f.opened.length, 0);
  f.state.account = accountA;
  f.navigation.synchronize();
  assert.deepEqual(
    f.opened.map((handle) => handle.destination),
    [{ kind: 'conversation', peerId: peer }],
  );
  f.navigation.synchronize();
  assert.equal(f.opened.length, 1);
});

test('a different account retires open surfaces and history cannot restore the previous account', async () => {
  const f = await fixture();
  const inbox = f.open({ kind: 'inbox' });
  f.open({ kind: 'conversation', peerId: peer });
  f.state.account = accountB;
  f.navigation.synchronize();
  assert.equal(inbox.closed, true);
  f.back();
  assert.equal(f.opened.length, 2, 'old account history must not reopen on the new account');
});

test('a room-changing link closes views but delegates room departure to RoomNavigation', async () => {
  const f = await fixture();
  const inbox = f.open({ kind: 'inbox' });
  assert.equal(f.visit('#another-room'), false);
  assert.equal(inbox.closed, true);
});

test('closing settings skips its nested screens while browser Back restores each screen', async () => {
  const f = await fixture();
  const root = { kind: 'settings', segment: 'you' };
  const privacy = { kind: 'settings', section: 'privacy', segment: 'you' };
  f.open(root, { group: 'settings', restore: () => f.open(root, { group: 'settings' }) });
  const section = f.open(privacy, { group: 'settings' });
  f.back();
  assert.equal(section.closed, false, 'same-sheet traversal preserves the room context');
  assert.deepEqual(f.opened.at(-1).destination, root);
  f.forward();
  assert.deepEqual(f.opened.at(-1).destination, privacy);
  f.opened.at(-1).close();
  assert.equal(f.location.hash, '#call', 'Close settings returns before the whole sheet');
});

test('closing a cold restored route stays on this page instead of leaving for a previous site', async () => {
  const f = await fixture({ hash: '#view=inbox' });
  f.navigation.synchronize();
  assert.equal(f.opened.length, 1);
  f.opened[0].close();
  assert.equal(f.location.hash, '');
  assert.equal(f.entries.length, 1);
});

test("an old document history ID cannot collide with this document's saved surfaces", async () => {
  const f = await fixture();
  const inbox = f.open({ kind: 'inbox' });
  f.history.pushState(
    { ...f.history.state, simplestchatNavigationSession: 'previous-document' },
    '',
    `#room=call&view=conversation&peer=${peer}`,
  );
  f.navigation.locationChanged('call');
  assert.equal(inbox.closed, true);
  assert.equal(f.opened.at(-1).destination.kind, 'conversation');
});

test('an arriving offer scrubs its fragment and returns to the account view underneath', async () => {
  const f = await fixture();
  const inbox = f.open({ kind: 'inbox' });
  f.history.pushState(null, '', '#contact=offered-id');
  f.navigation.preserveLocation();
  assert.equal(f.location.hash, '#room=call&view=inbox');
  const offer = f.open(null, { transient: true });
  f.back();
  assert.equal(offer.closed, true);
  assert.equal(inbox.closed, false);
});

test('a queued close cannot undo a newer explicit room navigation', async () => {
  const f = await fixture();
  const rooms = f.open(null, { transient: true });
  f.history.pushState(null, '', '#new-room');
  rooms.close();
  assert.equal(f.location.hash, '#new-room');
});

test('a queued close cannot dismiss a newly opened settings surface', async () => {
  const f = await fixture();
  const old = f.open(null, { transient: true });
  const settings = f.open({ kind: 'settings', section: 'privacy' }, { group: 'settings' });
  old.close();
  assert.equal(settings.closed, false);
  assert.equal(f.location.hash, '#room=call&view=settings&section=privacy');
  settings.close();
  assert.equal(f.location.hash, '#call');
});

test('HTTP homepage navigation requires no secure-context randomUUID API', async () => {
  const f = await fixture({ cryptoApi: { getRandomValues: (array) => array.fill(123) } });
  f.open({ kind: 'inbox' });
  f.back();
  assert.equal(f.location.hash, '#call');
});

test('same-sheet Back and Forward rebuild screens without closing its managed-room context', async () => {
  const f = await fixture();
  const root = f.open({ kind: 'settings', segment: 'room' }, { group: 'settings' });
  const section = f.open(
    { kind: 'settings', section: 'room-access', segment: 'room' },
    { group: 'settings' },
  );
  f.back();
  assert.equal(
    section.closed,
    false,
    'rebuilding a settings frame must not close the whole context',
  );
  f.forward();
  assert.equal(root.closed, false);
});

test('cold settings followed by more sections closes to its page instead of reopening the cold sheet', async () => {
  const f = await fixture({ hash: '#view=settings&section=security&segment=you' });
  f.navigation.synchronize();
  f.open({ kind: 'settings', segment: 'you' }, { group: 'settings' });
  const section = f.open(
    { kind: 'settings', section: 'security', segment: 'you' },
    { group: 'settings' },
  );
  section.close();
  assert.equal(f.location.hash, '');
  assert.equal(section.closed, true);
  const closedCount = f.opened.length;
  f.forward();
  assert.equal(f.opened.length, closedCount + 1, 'later entries remain Forward-restorable');
  assert.equal(f.opened.at(-1).destination.kind, 'settings');
});
