import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

const flush = async () => {
  for (let index = 0; index < 10; index++) await Promise.resolve();
};

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((accept, fail) => {
    resolve = accept;
    reject = fail;
  });
  return { promise, resolve, reject };
}

async function fixture(t, { hash = '', leave = async () => {} } = {}) {
  const events = new Map();
  const selections = [];
  const pending = [];
  const errors = [];
  const pushes = [];
  const replacements = [];
  let leaves = 0;
  let formValue = '';
  let historyBlocked = false;
  const location = { pathname: '/app', search: '?theme=dark', hash };
  const update = (url) => {
    if (historyBlocked) throw new Error('History unavailable');
    assert.ok(url.startsWith('/app?theme=dark'));
    location.hash = new URL(url, 'https://local.invalid').hash;
  };
  const { RoomNavigation } = await loadTypeScript('src/room-navigation.ts', {
    globals: {
      window: {
        location,
        history: {
          pushState: (_state, _title, url) => {
            update(url);
            pushes.push(url);
          },
          replaceState: (_state, _title, url) => {
            update(url);
            replacements.push(url);
          },
        },
        addEventListener: (name, listener) => events.set(name, listener),
        removeEventListener: (name) => events.delete(name),
      },
    },
  });
  const navigation = new RoomNavigation({
    leave: () => {
      leaves++;
      return leave();
    },
    select: (id) => {
      formValue = id;
      selections.push(id);
    },
    pending: (value) => pending.push(value),
    error: (error) => errors.push(error),
  });
  t.after(() => navigation.dispose());
  return {
    navigation,
    selections,
    pending,
    errors,
    pushes,
    replacements,
    location,
    events,
    leaves: () => leaves,
    formValue: () => formValue,
    editForm: (value) => {
      formValue = value;
    },
    blockHistory: () => {
      historyBlocked = true;
    },
    visit: (value) => {
      location.hash = value;
      events.get('popstate')?.();
      events.get('hashchange')?.();
    },
  };
}

test('initial room links decode safely and select without leaving or joining', async (t) => {
  const f = await fixture(t, { hash: '#room_%41-2' });
  assert.deepEqual(f.selections, ['room_A-2']);
  assert.equal(f.leaves(), 0);
  assert.deepEqual(f.pending, []);
  assert.deepEqual(f.errors, []);
});

test('reselecting the current destination restores an edited join form without leaving', async (t) => {
  for (const id of ['room-a', '']) {
    const f = await fixture(t, { hash: id ? `#${id}` : '' });
    f.editForm('room-b');
    f.navigation.selectRoom(id);
    assert.equal(f.formValue(), id, 'selection wins over a manually edited room name');
    assert.deepEqual(f.selections, [id, id]);
    assert.equal(f.leaves(), 0);
    assert.deepEqual(f.pending, []);
    assert.deepEqual(f.pushes, []);
  }
});

test('invalid initial hashes select home and never become room destinations', async (t) => {
  for (const hash of [
    '#%',
    '#%E0%A4%A',
    '#a/b',
    '#a%20b',
    '#room%0A',
    '#room%0D',
    '#日本',
    `#${'a'.repeat(129)}`,
    `#${'x'.repeat(385)}`,
  ]) {
    const f = await fixture(t, { hash });
    assert.deepEqual(f.selections, [''], hash);
    assert.equal(f.location.hash, '', hash);
    assert.equal(f.errors.length, 1, hash);
    assert.equal(f.leaves(), 0, hash);
  }
});

test('explicit joins update history without triggering leave and home releases the room once', async (t) => {
  const f = await fixture(t);
  assert.equal(f.navigation.join('room-1'), true);
  assert.equal(f.location.hash, '#room-1');
  assert.equal(f.leaves(), 0);
  assert.deepEqual(f.selections, ['']);
  f.events.get('hashchange')();
  assert.equal(f.leaves(), 0);
  f.navigation.home();
  await flush();
  assert.equal(f.leaves(), 1);
  assert.deepEqual(f.selections, ['', '']);
  assert.deepEqual(f.pending, [true, false]);
  assert.equal(f.location.hash, '');
  assert.deepEqual(f.pushes, ['/app?theme=dark#room-1', '/app?theme=dark']);
  f.navigation.home();
  await flush();
  assert.equal(f.leaves(), 1, 'repeated home is idempotent');
});

test('back, forward, and edited hashes select after leaving, with duplicate events coalesced', async (t) => {
  const f = await fixture(t);
  f.navigation.join('room-a');
  f.visit('#room-b');
  await flush();
  assert.equal(f.leaves(), 1);
  assert.deepEqual(f.selections, ['', 'room-b']);
  f.visit('#room-a');
  await flush();
  assert.equal(f.leaves(), 2);
  assert.deepEqual(f.selections, ['', 'room-b', 'room-a']);
  assert.deepEqual(
    f.pushes,
    ['/app?theme=dark#room-a'],
    'browser navigation does not push another entry',
  );
});

test('rapid navigation serializes leave and selects only the newest destination', async (t) => {
  const leaving = deferred();
  const f = await fixture(t, { leave: () => leaving.promise });
  f.navigation.join('room-a');
  f.navigation.home();
  f.navigation.selectRoom('room-b');
  f.visit('#room-c');
  assert.equal(f.leaves(), 1);
  assert.equal(
    f.navigation.join('stale-room'),
    false,
    'a late join cannot overwrite pending navigation',
  );
  assert.deepEqual(f.pending, [true]);
  assert.deepEqual(f.selections, ['']);
  leaving.resolve();
  await flush();
  assert.deepEqual(f.selections, ['', 'room-c']);
  assert.deepEqual(f.pending, [true, false]);
  assert.equal(f.location.hash, '#room-c');
  assert.equal(f.navigation.join('room-c'), true);
});

test('invalid navigation preserves the active room and valid ID boundaries match the server', async (t) => {
  const f = await fixture(t);
  assert.equal(f.navigation.join('a'.repeat(128)), true);
  const hash = f.location.hash;
  for (const invalid of ['', 'a/b', 'two words', 'room\n', 'room\r', 'a'.repeat(129)]) {
    assert.equal(f.navigation.join(invalid), false);
  }
  f.navigation.selectRoom('a/b');
  f.visit('#%');
  assert.equal(f.location.hash, hash);
  assert.equal(f.leaves(), 0);
  assert.equal(f.errors.length, 8);
});

test('leave failure preserves the active selection, reports failure and permits retry', async (t) => {
  let fail = true;
  const error = new Error('Could not release room');
  const f = await fixture(t, {
    leave: async () => {
      if (fail) throw error;
    },
  });
  f.navigation.join('room-a');
  f.navigation.selectRoom('room-b');
  await flush();
  assert.equal(f.location.hash, '#room-a');
  assert.deepEqual(f.selections, ['']);
  assert.deepEqual(f.errors, [error]);
  assert.deepEqual(f.pending, [true, false]);
  fail = false;
  f.navigation.selectRoom('room-b');
  await flush();
  assert.deepEqual(f.selections, ['', 'room-b']);
  assert.equal(f.location.hash, '#room-b');
});

test('failed history writes do not leave a room or change the selected destination', async (t) => {
  const f = await fixture(t);
  f.navigation.join('room-a');
  f.blockHistory();
  f.navigation.home();
  assert.equal(f.navigation.join('room-b'), false);
  assert.equal(f.leaves(), 0);
  assert.equal(f.location.hash, '#room-a');
  assert.equal(f.errors.length, 2);
});

test('disposing during leave removes listeners and suppresses every late navigation callback', async (t) => {
  const leaving = deferred();
  const f = await fixture(t, { leave: () => leaving.promise });
  f.navigation.join('room-a');
  f.navigation.home();
  f.navigation.dispose();
  f.navigation.dispose();
  assert.equal(f.events.size, 0);
  assert.deepEqual(f.pending, [true, false]);
  f.navigation.selectRoom('room-b');
  assert.equal(f.navigation.join('room-b'), false);
  leaving.reject(new Error('Retired leave'));
  await flush();
  assert.deepEqual(f.selections, ['']);
  assert.deepEqual(f.errors, []);
  assert.deepEqual(f.pending, [true, false]);
});
