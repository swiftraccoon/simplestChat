import assert from 'node:assert/strict';
import test from 'node:test';
import { loadContractModules, loadTypeScript } from './native-fixtures.mjs';
import { uiFixture, flush, deferred } from './ui-fixture.mjs';

const ID = '00000000-0000-4000-8000-000000000001';
const metadata = (id = ID) => ({ id, name: 'photo.png', contentType: 'image/png', size: 3 });
async function fixture() {
  const f = await uiFixture();
  f.Node.prototype.removeEventListener = function (type, callback) {
    this.listeners.set(
      type,
      (this.listeners.get(type) ?? []).filter((entry) => entry.callback !== callback),
    );
  };
  const state = {
    current: true,
    token: 'account-token',
    notices: [],
    xhrs: [],
    fetches: [],
    revoked: [],
    blobs: [],
  };
  class XHR {
    upload = {};
    headers = {};
    status = 200;
    responseText = JSON.stringify(metadata());
    constructor() {
      state.xhrs.push(this);
    }
    open(...args) {
      this.args = args;
    }
    setRequestHeader(key, value) {
      this.headers[key] = value;
    }
    send(file) {
      this.file = file;
    }
    abort() {
      this.aborted = true;
      this.onabort?.();
    }
  }
  const response = () =>
    new Response(new Uint8Array([1, 2, 3]), { headers: { 'Content-Type': 'image/png' } });
  state.response = response;
  const module = await loadTypeScript('src/attachments.ts', {
    modules: { ...(await loadContractModules()), './ui': f.ui, './attachments.css': {} },
    globals: {
      XMLHttpRequest: XHR,
      fetch: async (...args) => {
        state.fetches.push(args);
        return state.response();
      },
      URL: {
        createObjectURL(blob) {
          state.blobs.push(blob);
          return `blob:${state.blobs.length}`;
        },
        revokeObjectURL(url) {
          state.revoked.push(url);
        },
      },
    },
  });
  const host = f.ui.el('div');
  const input = f.ui.el('textarea');
  host.append(input);
  f.document.body.append(host);
  const options = {
    host,
    input,
    getToken: () => state.token,
    current: () => state.current,
    notify: (text) => state.notices.push(text),
  };
  return { ...f, state, module, host, input, options };
}
function select(f, files = [new File(['abc'], 'photo.png', { type: 'image/png' })]) {
  const picker = f.host.querySelector('input');
  picker.files = files;
  picker.emit('change');
}
function click(host, name) {
  const control = host.querySelectorAll('button').find((button) => button.textContent === name);
  assert.ok(control, name);
  control.click();
}
/** A second composer on the same page; every composer shares the module's upload queue. */
function secondComposer(f, options = {}) {
  const host = f.ui.el('div');
  const input = f.ui.el('textarea');
  host.append(input);
  f.document.body.append(host);
  const composer = new f.module.AttachmentComposer({ ...f.options, host, input, ...options });
  return { host, composer };
}
const occurrences = (text, word) => text.split(word).length - 1;

test('binary uploads preserve Unicode names, report progress, and gate send until acknowledged', async () => {
  const f = await fixture();
  const composer = new f.module.AttachmentComposer(f.options);
  select(f, [new File(['abc'], 'café.png', { type: 'image/png' })]);
  const xhr = f.state.xhrs[0];
  assert.deepEqual(xhr.args, ['POST', '/api/auth/attachments']);
  assert.equal(xhr.headers.Authorization, 'Bearer account-token');
  assert.equal(Buffer.from(xhr.headers['X-File-Name'], 'base64url').toString(), 'café.png');
  assert.throws(() => composer.ready(), /Wait for uploads/);
  xhr.upload.onprogress({ lengthComputable: true, loaded: 1, total: 2 });
  assert.match(f.host.textContent, /50%/);
  xhr.onload();
  await flush();
  assert.deepEqual(composer.ready(), [metadata()]);
  composer.consume();
  assert.equal(composer.hasFiles, false);
  assert.equal(f.state.fetches.length, 0, 'consuming a sent attachment never deletes it');
  composer.dispose();
});

test('failed uploads can retry; context reset aborts and late success cannot restore selected files', async () => {
  const f = await fixture();
  const composer = new f.module.AttachmentComposer(f.options);
  select(f);
  f.state.xhrs[0].onerror();
  await flush();
  assert.match(f.host.textContent, /Upload interrupted/);
  click(f.host, 'Retry');
  assert.equal(f.state.xhrs.length, 2);
  f.state.xhrs[1].onload();
  composer.reset();
  await flush();
  assert.equal(composer.hasFiles, false);
  assert.equal(f.state.fetches.length, 1);
  assert.equal(f.state.fetches[0][1].method, 'DELETE');
  assert.equal(f.state.fetches[0][1].headers.Authorization, 'Bearer account-token');
  composer.dispose();
});

test('paste/drop, count and size bounds, sign-in and disabled send state apply to every picker path', async () => {
  const f = await fixture();
  const composer = new f.module.AttachmentComposer(f.options);
  select(
    f,
    Array.from({ length: 5 }, () => new File(['a'], 'a.txt')),
  );
  assert.equal(f.state.xhrs.length, 0);
  assert.match(f.state.notices.pop(), /four files/);
  select(f, [new File([], 'empty.txt')]);
  assert.equal(f.state.xhrs.length, 0);
  let prevented = false;
  f.input.emit('paste', {
    clipboardData: { files: [new File(['abc'], 'paste.png')] },
    preventDefault() {
      prevented = true;
    },
  });
  assert.equal(prevented, true);
  assert.equal(f.state.xhrs.length, 1);
  f.input.disabled = true;
  click(f.host, '×');
  assert.equal(composer.hasFiles, true);
  f.host.emit('drop', {
    dataTransfer: { files: [new File(['abc'], 'drop.png')] },
    preventDefault() {},
  });
  assert.equal(f.state.xhrs.length, 1);
  f.input.disabled = false;
  composer.reset();
  f.state.token = null;
  select(f);
  assert.match(f.state.notices.pop(), /Sign in/);
  assert.equal(f.state.xhrs.length, 1);
  composer.dispose();
});

test('the page sends one upload at a time in selection order; a file removed while queued never starts', async () => {
  const f = await fixture();
  const composer = new f.module.AttachmentComposer(f.options);
  const names = ['one.png', 'two.png', 'three.png', 'four.png'];
  select(
    f,
    names.map((name) => new File(['abc'], name, { type: 'image/png' })),
  );
  for (const [index, name] of names.entries()) {
    assert.equal(f.state.xhrs.length, index + 1, 'at most one upload is in flight');
    assert.equal(occurrences(f.host.textContent, 'Waiting'), names.length - index - 1);
    const xhr = f.state.xhrs[index];
    assert.equal(xhr.file.name, name, 'uploads start in selection order');
    if (index % 2 === 0) xhr.onload();
    else xhr.onerror();
    await flush();
  }
  assert.equal(f.state.xhrs.length, names.length);
  assert.equal(occurrences(f.host.textContent, 'Ready'), 2);
  assert.equal(occurrences(f.host.textContent, 'Upload interrupted'), 2);
  composer.consume();
  // A second composer on the page waits behind the first composer's upload.
  const other = secondComposer(f);
  select(f, [new File(['abc'], 'five.png', { type: 'image/png' })]);
  select(
    other,
    ['six.png', 'seven.png', 'eight.png'].map((name) => new File(['abc'], name)),
  );
  assert.equal(f.state.xhrs.length, 5);
  click(other.host, '×');
  // A queued upload reads the token when it starts, not when it was queued.
  f.state.token = 'refreshed-token';
  f.state.xhrs[4].onload();
  await flush();
  assert.equal(f.state.xhrs.length, 6);
  assert.equal(f.state.xhrs[5].file.name, 'seven.png');
  assert.equal(f.state.xhrs[5].headers.Authorization, 'Bearer refreshed-token');
  f.state.token = null;
  f.state.xhrs[5].onload();
  await flush();
  assert.equal(f.state.xhrs.length, 6, 'neither the removed nor the signed-out file starts');
  assert.match(other.host.textContent, /Sign in again to attach files/);
  assert.equal(f.state.fetches.length, 0);
  composer.dispose();
  other.composer.dispose();
});

test('a queued file whose composer stops being current never starts and never deletes', async () => {
  const f = await fixture();
  const composer = new f.module.AttachmentComposer(f.options);
  let current = true;
  const other = secondComposer(f, { current: () => current });
  select(f, [new File(['abc'], 'first.png', { type: 'image/png' })]);
  select(other, [new File(['abc'], 'queued.png')]);
  assert.equal(f.state.xhrs.length, 1);
  assert.match(other.host.textContent, /Waiting/);
  current = false;
  f.state.xhrs[0].onload();
  await flush();
  assert.equal(f.state.xhrs.length, 1, 'the stale file never starts an upload');
  assert.equal(f.state.fetches.length, 0, 'and nothing is deleted');
  composer.consume();
  composer.dispose();
  other.composer.dispose();
});

test('preview is explicitly requested, sends only a header grant and revokes blob on disposal', async () => {
  const f = await fixture();
  const grants = [];
  const cleanup = f.module.renderAttachments(f.host, [metadata()], {
    current: () => f.state.current,
    authorization: async (id) => {
      grants.push(id);
      return 'Attachment grant';
    },
  });
  assert.equal(f.state.fetches.length, 0);
  click(f.host, 'Preview');
  await flush();
  assert.deepEqual(grants, [ID]);
  assert.equal(f.state.fetches[0][0], `/api/auth/attachments/${ID}`);
  assert.equal(f.state.fetches[0][1].headers.Authorization, 'Attachment grant');
  assert.equal(f.state.blobs.length, 1);
  assert.equal(f.host.querySelector('img').hidden, false);
  cleanup();
  assert.deepEqual(f.state.revoked, ['blob:1']);
});

test('late authorization, expired access and mismatched response sizes never create a preview', async () => {
  const f = await fixture();
  const grant = deferred();
  const cleanup = f.module.renderAttachments(f.host, [metadata()], {
    current: () => f.state.current,
    authorization: () => grant.promise,
  });
  click(f.host, 'Preview');
  cleanup();
  grant.resolve('Attachment old');
  await flush();
  assert.equal(f.state.fetches.length, 0);
  for (const response of [
    () => new Response('', { status: 404 }),
    () => new Response(new Uint8Array(4), { headers: { 'Content-Type': 'image/png' } }),
  ]) {
    f.state.response = response;
    const dispose = f.module.renderAttachments(f.host, [metadata()], {
      current: () => true,
      authorization: async () => 'Bearer account-token',
    });
    click(f.host, 'Preview');
    await flush();
    assert.equal(f.state.blobs.length, 0);
    assert.match(f.host.textContent, /expired|size/);
    dispose();
  }
});

test('multiple open histories retain at most eight attachment blobs', async () => {
  const f = await fixture();
  const cleanups = [];
  for (let index = 0; index < 10; index++) {
    const host = f.ui.el('div');
    f.host.append(host);
    cleanups.push(
      f.module.renderAttachments(host, [metadata()], {
        current: () => true,
        authorization: async () => 'Bearer account-token',
      }),
    );
    click(host, 'Preview');
    await flush();
  }
  assert.equal(f.state.blobs.length, 10);
  assert.equal(f.state.revoked.length, 2);
  assert.equal(f.host.querySelector('img').hidden, true);
  for (const cleanup of cleanups) cleanup();
  assert.equal(new Set(f.state.revoked).size, 10);
});
