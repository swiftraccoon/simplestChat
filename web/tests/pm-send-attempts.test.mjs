import assert from 'node:assert/strict';
import test from 'node:test';
import { loadContractModules, loadTypeScript } from './source-loader.mjs';

const modules = await loadContractModules();
const load = () => loadTypeScript('src/pm-send-attempts.ts', { modules });
const { PmSendAttempts, PM_SEND_STORAGE_KEY } = await load();
const DAY = 24 * 60 * 60 * 1000;
const file = {
  id: '11111111-1111-4111-8111-111111111111',
  name: 'Photo.png',
  contentType: 'image/png',
  size: 123,
};
const payload = (id = 'attempt-1', content = 'Send once', attachments = []) => ({
  id,
  content,
  attachments,
});
function fixture() {
  let raw = null;
  let now = DAY;
  const storage = {
    getItem: (key) => {
      assert.equal(key, PM_SEND_STORAGE_KEY);
      return raw;
    },
    setItem: (key, value) => {
      assert.equal(key, PM_SEND_STORAGE_KEY);
      raw = value;
    },
  };
  return {
    store: new PmSendAttempts(storage, () => now),
    storage,
    raw: () => raw,
    replace: (value) => {
      raw = value;
    },
    time: (value) => {
      now = value;
    },
  };
}

test('the original text and attachment payload survive new instances and module reload without replay', async () => {
  const f = fixture();
  const original = payload('attempt-1', '', [file]);
  const saved = f.store.begin('account', 'peer', original);
  assert.equal(saved.kind, 'pending');
  const reloaded = await load();
  const other = new reloaded.PmSendAttempts(f.storage, () => DAY + 1);
  assert.deepEqual(other.read('account', 'peer'), saved);
  assert.equal(other.read('another-account', 'peer').kind, 'empty');
  assert.equal(other.read('account', 'another-peer').kind, 'empty');
  original.attachments[0] = { ...file, name: 'Changed' };
  assert.equal(other.read('account', 'peer').attempt.attachments[0].name, 'Photo.png');
});

test('begin retains one immutable unresolved payload and preserves its creation time on exact retry', () => {
  const f = fixture();
  const original = payload();
  const first = f.store.begin('account', 'peer', original);
  const raw = f.raw();
  f.time(DAY + 1000);
  assert.deepEqual(f.store.begin('account', 'peer', original), first);
  for (const changed of [
    payload('replacement'),
    payload('attempt-1', 'Edited draft'),
    payload('attempt-1', 'Send once', [file]),
  ]) {
    assert.deepEqual(f.store.begin('account', 'peer', changed), {
      kind: 'conflict',
      attempt: first.attempt,
    });
    assert.equal(f.raw(), raw);
  }
});

test('serialized callers read current storage and an old receipt cannot clear a newer attempt', () => {
  const f = fixture();
  const older = f.store;
  const newer = new PmSendAttempts(f.storage, () => DAY);
  assert.equal(older.begin('account', 'peer', payload('old')).kind, 'pending');
  assert.equal(newer.begin('account', 'peer', payload('new')).kind, 'conflict');
  assert.equal(newer.clear('account', 'other-peer', 'old'), false);
  assert.equal(newer.clear('other-account', 'peer', 'old'), false);
  assert.equal(newer.clear('account', 'peer', 'old'), true);
  assert.equal(newer.begin('account', 'peer', payload('new')).kind, 'pending');
  assert.equal(older.clear('account', 'peer', 'old'), false);
  assert.equal(older.read('account', 'peer').attempt.id, 'new');
  assert.equal(older.begin('account', 'second-peer', payload('second')).kind, 'pending');
  assert.equal(newer.read('account', 'peer').attempt.id, 'new');
});

test('expiry and backwards clocks retain the unresolved record and never issue another attempt', () => {
  const f = fixture();
  const original = f.store.begin('account', 'peer', payload());
  const raw = f.raw();
  f.time(2 * DAY - 1);
  assert.equal(f.store.read('account', 'peer').kind, 'pending');
  for (const time of [2 * DAY, 100 * DAY, DAY - 1, NaN]) {
    f.time(time);
    assert.deepEqual(f.store.read('account', 'peer'), {
      kind: 'expired',
      attempt: original.attempt,
    });
    if (Number.isFinite(time))
      assert.equal(f.store.begin('account', 'peer', payload('new')).kind, 'expired');
    assert.equal(f.raw(), raw);
  }
  assert.equal(f.store.clear('account', 'peer', 'attempt-1'), true);
});

test('count and UTF8 byte caps refuse new attempts without evicting unresolved records', () => {
  const f = fixture();
  for (let index = 0; index < 100; index++)
    assert.equal(f.store.begin('account', `peer-${index}`, payload()).kind, 'pending');
  const full = f.raw();
  assert.equal(f.store.begin('account', 'one-too-many', payload()).kind, 'full');
  assert.equal(f.raw(), full);
  assert.equal(f.store.read('account', 'peer-0').kind, 'pending');
  f.time(100 * DAY);
  assert.equal(f.store.begin('account', 'one-too-many', payload()).kind, 'full');
  assert.equal(f.raw(), full, 'expired uncertainty must not be evicted to make room');
  const unicode = fixture();
  let accepted = 0;
  while (
    accepted <= 100 &&
    unicode.store.begin('account', `peer-${accepted}`, payload('id', '界'.repeat(1000))).kind ===
      'pending'
  )
    accepted++;
  assert.ok(accepted > 0 && accepted < 100);
  assert.ok(new TextEncoder().encode(unicode.raw()).length <= 64 * 1024);
  assert.equal(unicode.store.read('account', 'peer-0').kind, 'pending');
});

test('a failed or unconfirmed cleanup does not report an unresolved attempt as cleared', () => {
  for (const discarding of [false, true]) {
    const f = fixture();
    f.store.begin('account', 'peer', payload());
    f.storage.setItem = () => {
      if (!discarding) throw new Error('Storage unavailable');
    };
    assert.equal(f.store.clear('account', 'peer', 'attempt-1'), false);
    assert.equal(f.store.read('account', 'peer').attempt.id, 'attempt-1');
  }
});

test('blocked, quota-exhausted or discarded storage refuses an untracked send', () => {
  for (const storage of [
    {
      getItem() {
        throw new Error('Blocked');
      },
      setItem() {},
    },
    {
      getItem: () => null,
      setItem() {
        throw new Error('Quota exhausted');
      },
    },
    { getItem: () => null, setItem() {} },
  ]) {
    const store = new PmSendAttempts(storage, () => DAY);
    assert.equal(store.begin('account', 'peer', payload()).kind, 'unavailable');
    assert.equal(store.clear('account', 'peer', 'attempt-1'), false);
  }
});

test('malformed storage is never silently overwritten or partially recovered', () => {
  const valid = ['account', 'peer', { ...payload(), createdAt: DAY }];
  for (const value of [
    'not JSON',
    '{}',
    '[null]',
    JSON.stringify([valid, valid]),
    JSON.stringify([['account', 'account', valid[2]]]),
    JSON.stringify([['bad:account', 'peer', valid[2]]]),
    JSON.stringify([['account', 'peer', { ...valid[2], token: 'never-adopt-extra-fields' }]]),
    JSON.stringify([['account', 'peer', { ...valid[2], createdAt: -1 }]]),
    JSON.stringify([['account', 'peer', { ...valid[2], createdAt: 0.5 }]]),
    JSON.stringify([['account', 'peer', { ...valid[2], createdAt: null }]]),
    JSON.stringify([['account', 'peer', { ...valid[2], id: '<invalid>' }]]),
    JSON.stringify([['account', 'peer', { ...valid[2], content: '界'.repeat(1366) }]]),
    JSON.stringify([['account', 'peer', { ...valid[2], attachments: [file, file] }]]),
    JSON.stringify([['account', 'peer', { ...valid[2], attachments: Array(5).fill(file) }]]),
    JSON.stringify([
      ['account', 'peer', { ...valid[2], attachments: [{ ...file, endpoint: 'hidden' }] }],
    ]),
    JSON.stringify([['account', 'peer', { ...valid[2], attachments: [{ ...file, size: 0 }] }]]),
    JSON.stringify([
      ['account', 'peer', { ...valid[2], attachments: [{ ...file, name: 'bad\nname' }] }],
    ]),
    JSON.stringify([
      ['account', 'peer', { ...valid[2], attachments: [{ ...file, contentType: 'text/html' }] }],
    ]),
    ' '.repeat(64 * 1024 + 1),
  ]) {
    const f = fixture();
    f.replace(value);
    assert.equal(f.store.read('account', 'peer').kind, 'unavailable');
    assert.equal(f.store.begin('account', 'another-peer', payload()).kind, 'unavailable');
    assert.equal(f.store.clear('account', 'peer', 'attempt-1'), false);
    assert.equal(f.raw(), value);
  }
});

test('invalid new payloads never reach durable storage', () => {
  for (const invalid of [
    payload('', 'Text'),
    payload('id', ''),
    payload('id', 'x'.repeat(2001)),
    payload('id', '界'.repeat(1366)),
    payload('id', '', [file, file]),
  ]) {
    const f = fixture();
    assert.equal(f.store.begin('account', 'peer', invalid).kind, 'unavailable');
    assert.equal(f.raw(), null);
  }
});
