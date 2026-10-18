import assert from 'node:assert/strict';
import test from 'node:test';
import { setImmediate as nextTurn } from 'node:timers/promises';
import { attachmentFixture } from './attachment-fixture.mjs';
import { loadContractModules, loadTypeScript } from './source-loader.mjs';
import { deferred, flush, uiFixture } from './ui-fixture.mjs';

const modules = await loadContractModules();
const stores = await loadTypeScript('src/pm-send-attempts.ts', { modules });
const drafts = await loadTypeScript('src/chat-store.ts');
const file = {
  id: '11111111-1111-4111-8111-111111111111',
  name: 'Photo.png',
  contentType: 'image/png',
  size: 123,
};

function sharedState() {
  const values = new Map();
  let sequence = 0;
  const requests = [];
  const queue = [];
  let held = false;
  const drain = () => {
    if (held || !queue.length) return;
    const entry = queue.shift();
    entry.signal.removeEventListener('abort', entry.abort);
    if (entry.signal.aborted) {
      entry.reject(entry.signal.reason);
      drain();
      return;
    }
    held = true;
    Promise.resolve()
      .then(() => entry.callback())
      .then(entry.resolve, entry.reject)
      .finally(() => {
        held = false;
        drain();
      });
  };
  return {
    requests,
    storage: {
      getItem: (key) => values.get(key) ?? null,
      setItem: (key, value) => values.set(key, value),
    },
    crypto: { randomUUID: () => `attempt-${++sequence}` },
    locks: {
      request(name, { signal }, callback) {
        requests.push(name);
        return new Promise((resolve, reject) => {
          const entry = { signal, callback, resolve, reject };
          entry.abort = () => {
            const index = queue.indexOf(entry);
            if (index >= 0) queue.splice(index, 1);
            reject(signal.reason);
          };
          if (signal.aborted) {
            reject(signal.reason);
            return;
          }
          signal.addEventListener('abort', entry.abort, { once: true });
          queue.push(entry);
          drain();
        });
      },
    },
  };
}

async function fixture(t, shared = sharedState(), { locks = true } = {}) {
  const ui = await uiFixture();
  const attachments = attachmentFixture();
  const host = ui.ui.el('section');
  ui.document.body.append(host);
  const controller = new AbortController();
  const events = new Map();
  const state = { account: 'account', posts: [], failures: [], confirmed: 0 };
  const current = () => state.account === 'account';
  const receipt = (content, id) => ({
    messageId: `saved-${id}`,
    clientMessageId: id,
    participantId: 'account',
    participantName: 'Account',
    recipientId: 'peer',
    recipientName: 'Peer',
    content,
    revision: 0,
    sentAt: '2026-10-18T00:00:00Z',
    reactions: [],
  });
  state.send = async (content, id) => receipt(content, id);
  const module = await loadTypeScript('src/pm-composer.ts', {
    modules: {
      './attachments': attachments.module,
      './chat-store': drafts,
      './pm-send-attempts': stores,
      './ui': ui.ui,
    },
    globals: {
      localStorage: shared.storage,
      navigator: locks ? { locks: shared.locks } : {},
      crypto: shared.crypto,
      window: {
        matchMedia: () => ({ matches: false }),
        addEventListener: (name, callback) => events.set(name, callback),
        removeEventListener: (name) => events.delete(name),
      },
    },
  });
  const dispose = module.mountPmComposer(host, {
    account: 'account',
    peer: 'peer',
    current,
    getToken: () => (current() ? 'token' : null),
    signal: controller.signal,
    send: async (...args) => {
      state.posts.push(args);
      return state.send(...args);
    },
    confirmed: async () => {
      state.confirmed++;
    },
    fail: (error) => {
      if (current() && !controller.signal.aborted) state.failures.push(error.message);
    },
  });
  let closed = false;
  const close = () => {
    if (closed) return;
    closed = true;
    controller.abort();
    dispose();
    host.remove();
  };
  t.after(close);
  return {
    ...ui,
    host,
    state,
    attachments: attachments.composers[0],
    shared,
    controller,
    close,
    receipt,
    store: new stores.PmSendAttempts(shared.storage),
    input: host.querySelector('textarea'),
    storageChanged: () => events.get('storage')?.({ key: stores.PM_SEND_STORAGE_KEY }),
  };
}

function action(host, name) {
  const control = host.querySelectorAll('button').find((node) => node.textContent === name);
  assert.ok(control, `Missing ${name}`);
  return control;
}
function type(f, text) {
  f.input.value = text;
  f.input.emit('input');
}
function confirmation(f) {
  return f.document.querySelectorAll('dialog').find((dialog) => dialog.open);
}

for (const refusal of ['no-locks', 'quota', 'discarded-write', 'blocked-read']) {
  test(`${refusal} prevents a POST and preserves the unsent draft and uploads`, async (t) => {
    const shared = sharedState();
    if (refusal === 'blocked-read')
      shared.storage.getItem = () => {
        throw new Error('Blocked');
      };
    const f = await fixture(t, shared, { locks: refusal !== 'no-locks' });
    type(f, 'Still unsent');
    f.attachments.files = [file];
    const write = shared.storage.setItem;
    if (refusal === 'quota' || refusal === 'discarded-write')
      shared.storage.setItem = (key, value) => {
        if (key === stores.PM_SEND_STORAGE_KEY) {
          if (refusal === 'quota') throw new Error('Quota');
          return;
        }
        write(key, value);
      };
    action(f.host, 'Send').click();
    await flush();
    assert.equal(f.state.posts.length, 0);
    assert.equal(f.input.value, 'Still unsent');
    assert.deepEqual(f.attachments.files, [file]);
    assert.equal(f.attachments.consumed, 0);
    if (refusal === 'blocked-read') assert.match(f.host.textContent, /Restore browser storage/);
    else assert.equal(f.state.failures.length, 1);
  });
}

test('the durable original is saved and uploads detached before POST, separate from the next draft', async (t) => {
  const f = await fixture(t);
  const pending = deferred();
  type(f, 'Original send');
  f.attachments.files = [file];
  f.state.send = (content, id) => {
    assert.equal(f.store.read('account', 'peer').attempt.id, id);
    assert.equal(f.input.value, '');
    assert.equal(new drafts.SavedPmDrafts(f.shared.storage).get('account', 'peer'), '');
    assert.equal(f.attachments.consumed, 1);
    assert.deepEqual(f.attachments.files, []);
    assert.equal(content, 'Original send');
    return pending.promise;
  };
  action(f.host, 'Send').click();
  await flush();
  pending.reject(new Error('Response lost'));
  await flush();
  type(f, 'The next draft');
  f.attachments.files = [{ ...file, id: '22222222-2222-4222-8222-222222222222' }];
  f.state.send = async (content, id, _signal, files) => {
    assert.equal(content, 'Original send');
    assert.deepEqual(files, [file.id]);
    return f.receipt(content, id);
  };
  action(f.host, 'Retry unconfirmed send').click();
  await flush();
  assert.equal(f.state.posts.length, 2);
  assert.equal(f.state.posts[1][1], f.state.posts[0][1]);
  assert.equal(f.store.read('account', 'peer').kind, 'empty');
  assert.equal(f.input.value, 'The next draft');
  assert.equal(f.attachments.files.length, 1);
  assert.equal(f.attachments.consumed, 1);
  assert.equal(f.state.confirmed, 1);
});

test('competing views serialize under the same Web Lock and cannot replace an unresolved send', async (t) => {
  const shared = sharedState();
  const first = await fixture(t, shared);
  const second = await fixture(t, shared);
  const pending = deferred();
  first.state.send = () => pending.promise;
  type(first, 'First payload');
  type(second, 'Second draft');
  action(first.host, 'Send').click();
  action(second.host, 'Send').click();
  await flush();
  assert.equal(first.state.posts.length, 1);
  assert.equal(second.state.posts.length, 0);
  assert.equal(first.store.read('account', 'peer').attempt.content, 'First payload');
  pending.reject(new Error('Response lost'));
  await flush();
  await nextTurn();
  assert.equal(second.state.posts.length, 0);
  assert.equal(second.state.failures.length, 1);
  assert.equal(second.input.value, 'Second draft');
  assert.equal(second.store.read('account', 'peer').attempt.content, 'First payload');
  assert.deepEqual(shared.requests, [stores.PM_SEND_LOCK, stores.PM_SEND_LOCK]);
});

for (const mismatch of ['clientMessageId', 'participantId', 'recipientId']) {
  test(`a receipt with the wrong ${mismatch} retains the original send for recovery`, async (t) => {
    const f = await fixture(t);
    type(f, 'Original payload');
    f.state.send = async (content, id) => ({ ...f.receipt(content, id), [mismatch]: 'wrong' });
    action(f.host, 'Send').click();
    await flush();
    assert.equal(f.store.read('account', 'peer').kind, 'pending');
    assert.match(f.state.failures[0], /not confirmed/);
    assert.equal(f.state.confirmed, 0);
    assert.equal(action(f.host, 'Send').disabled, true);
    assert.equal(action(f.host, 'Retry unconfirmed send').disabled, false);
  });
}

test('a receipt after account replacement cannot clear old recovery or update the new account UI', async (t) => {
  const f = await fixture(t);
  const pending = deferred();
  type(f, 'Old account send');
  f.state.send = () => pending.promise;
  action(f.host, 'Send').click();
  await flush();
  const id = f.state.posts[0][1];
  f.state.account = 'replacement';
  pending.resolve(f.receipt('Old account send', id));
  await flush();
  assert.equal(f.store.read('account', 'peer').attempt.id, id);
  assert.equal(f.store.read('replacement', 'peer').kind, 'empty');
  assert.equal(f.state.confirmed, 0);
  assert.deepEqual(f.state.failures, []);
});

test('dismissing requires confirmation and a stale confirmation cannot clear a replacement attempt', async (t) => {
  const f = await fixture(t);
  f.store.begin('account', 'peer', { id: 'original', content: 'Unconfirmed', attachments: [] });
  f.storageChanged();
  action(f.host, 'Dismiss unconfirmed send').click();
  assert.equal(f.store.read('account', 'peer').attempt.id, 'original');
  action(confirmation(f), 'Close').click();
  assert.equal(f.store.read('account', 'peer').attempt.id, 'original');
  action(f.host, 'Dismiss unconfirmed send').click();
  f.store.clear('account', 'peer', 'original');
  f.store.begin('account', 'peer', {
    id: 'replacement',
    content: 'New unconfirmed',
    attachments: [],
  });
  action(confirmation(f), 'Dismiss').click();
  await flush();
  assert.equal(f.store.read('account', 'peer').attempt.id, 'replacement');
  assert.match(f.state.failures[0], /changed or could not be cleared/);
  f.storageChanged();
  action(f.host, 'Dismiss unconfirmed send').click();
  action(confirmation(f), 'Dismiss').click();
  await flush();
  assert.equal(f.store.read('account', 'peer').kind, 'empty');
  assert.equal(f.state.posts.length, 0);
});

test('closing a queued view cancels lock admission without persisting or posting its draft', async (t) => {
  const shared = sharedState();
  const holder = deferred();
  const holding = shared.locks.request(
    stores.PM_SEND_LOCK,
    { signal: new AbortController().signal },
    () => holder.promise,
  );
  const f = await fixture(t, shared);
  type(f, 'Queued draft');
  action(f.host, 'Send').click();
  await flush();
  f.close();
  holder.resolve();
  await holding;
  await flush();
  assert.equal(f.store.read('account', 'peer').kind, 'empty');
  assert.equal(f.state.posts.length, 0);
  assert.equal(new drafts.SavedPmDrafts(shared.storage).get('account', 'peer'), 'Queued draft');
});
