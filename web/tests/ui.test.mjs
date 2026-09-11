import assert from 'node:assert/strict';
import test from 'node:test';
import { deferred, flush, uiFixture } from './ui-fixture.mjs';

test('controls render supplied labels as plain text and never submit a surrounding form', async () => {
  const { ui } = await uiFixture();
  const label = '<b>Ordinary user-supplied text</b>';
  const node = ui.el('p', label, 'bio');
  assert.equal(node.textContent, label);
  assert.equal(node.children.length, 0);
  assert.equal(node.className, 'bio');
  let clicks = 0;
  const action = ui.button(label, () => clicks++);
  assert.equal(action.type, 'button');
  action.click();
  assert.equal(clicks, 1);
  const input = ui.input(label, 'text', 64);
  const field = ui.field(label, input);
  assert.equal(field.children[0].textContent, label);
  assert.equal(field.children[1].value, label);
  assert.equal(input.maxLength, 64);
});

test('native modal retains inside clicks, closes on backdrop, and removes its DOM on close', async () => {
  const { ui, document } = await uiFixture();
  const view = ui.modal('Account <settings>');
  assert.equal(view.dialog.open, true);
  assert.equal(view.dialog.isConnected, true);
  assert.equal(view.error.hidden, true);
  assert.equal(view.error.getAttribute('role'), 'alert');
  assert.equal(view.dialog.getAttribute('aria-labelledby'), view.dialog.children[0].children[0].id);
  view.dialog.emit('click', { clientX: 100, clientY: 100 });
  assert.equal(view.dialog.open, true);
  view.dialog.emit('click', { clientX: 0, clientY: 0, target: view.body });
  assert.equal(view.dialog.open, true);
  view.dialog.emit('click', { clientX: 0, clientY: 0 });
  assert.equal(view.dialog.open, false);
  assert.equal(view.dialog.isConnected, false);
  assert.equal(document.querySelectorAll('dialog').length, 0);
  const escaped = ui.modal('Escape or programmatic close');
  escaped.dialog.close();
  assert.equal(escaped.dialog.isConnected, false);
});

test('async buttons invoke work within the click gesture and route rejection exactly once', async () => {
  const { ui } = await uiFixture();
  const pending = deferred();
  const errors = [];
  let gesture = false;
  let started = false;
  const action = ui.asyncButton(
    'Copy',
    () => {
      assert.equal(gesture, true, 'the action must not move into a later microtask');
      started = true;
      return pending.promise;
    },
    (error) => errors.push(error),
  );
  gesture = true;
  action.click();
  gesture = false;
  assert.equal(started, true);
  const failure = new Error('Clipboard unavailable');
  pending.reject(failure);
  await flush();
  assert.deepEqual(errors, [failure]);
});

test('async buttons route a synchronous failure to their required contextual handler', async () => {
  const { ui } = await uiFixture();
  const failure = new Error('Action unavailable');
  const errors = [];
  const action = ui.asyncButton(
    'Unavailable',
    () => {
      throw failure;
    },
    (error) => errors.push(error),
  );
  assert.doesNotThrow(() => action.click());
  assert.deepEqual(errors, [failure]);
});

test('async busy buttons keep duplicate-submission guards, local errors, and retry cleanup', async () => {
  const { ui } = await uiFixture();
  const pending = deferred();
  const error = ui.el('p');
  let calls = 0;
  const unexpected = [];
  const action = ui.asyncButton(
    'Save',
    () =>
      ui.busy(action, error, async () => {
        calls++;
        if (calls === 1) await pending.promise;
      }),
    (failure) => unexpected.push(failure),
  );
  action.click();
  action.click();
  assert.equal(calls, 1);
  assert.equal(action.disabled, true);
  pending.reject(new Error('Please retry'));
  await flush();
  assert.equal(error.textContent, 'Please retry');
  assert.equal(error.hidden, false);
  assert.equal(action.disabled, false);
  action.click();
  await flush();
  assert.equal(calls, 2);
  assert.equal(error.hidden, true);
  assert.deepEqual(unexpected, []);
});

test('busy prevents double submissions and recovers with a plain-text error', async () => {
  const { ui } = await uiFixture();
  const submit = ui.button('Save', () => {});
  const error = ui.el('p');
  const pending = deferred();
  let calls = 0;
  const first = ui.busy(submit, error, async () => {
    calls++;
    await pending.promise;
  });
  await ui.busy(submit, error, async () => {
    calls++;
  });
  assert.equal(calls, 1);
  assert.equal(submit.disabled, true);
  pending.reject(new Error('<b>Could not save</b>'));
  await first;
  assert.equal(error.textContent, '<b>Could not save</b>');
  assert.equal(error.hidden, false);
  assert.equal(submit.disabled, false);
  await ui.busy(submit, error, async () => {});
  assert.equal(error.hidden, true);
});

test('API helper scopes credentials, serializes JSON, and accepts empty successful responses', async () => {
  const { ui, state } = await uiFixture();
  state.response = {
    ok: true,
    status: 204,
    json() {
      throw new Error('Unexpected JSON parse');
    },
  };
  assert.equal(
    await ui.api.changePassword('test-token', { current_password: 'old', new_password: 'new' }),
    undefined,
  );
  assert.deepEqual(state.requests[0], [
    '/api/auth/password',
    {
      method: 'POST',
      credentials: 'same-origin',
      headers: { Authorization: 'Bearer test-token', 'Content-Type': 'application/json' },
      body: '{"current_password":"old","new_password":"new"}',
    },
  ]);
  state.response = { ok: true, status: 200, json: async () => [] };
  assert.deepEqual(await ui.api.ownRooms(null), []);
  assert.deepEqual(state.requests[1][1].headers, {});
  assert.equal(Object.hasOwn(state.requests[1][1], 'body'), false);
});

test('API errors use server JSON text, cap long responses, and handle an empty body', async () => {
  const { ui, state } = await uiFixture();
  for (const [body, expected] of [
    ['{"error":"Invalid credentials"}', 'Invalid credentials'],
    ['x'.repeat(500), 'x'.repeat(400)],
    ['', 'Request failed (400)'],
  ]) {
    state.response = { ok: false, status: 400, text: async () => body };
    await assert.rejects(
      ui.api.accountProfile(null),
      (error) => error instanceof ui.ApiError && error.status === 400 && error.message === expected,
    );
  }
});

test('raster URL allowlist excludes remote, vector, malformed, and oversized image data', async () => {
  const { ui } = await uiFixture();
  for (const type of ['png', 'jpeg', 'webp'])
    assert.equal(ui.safeRasterUrl(`data:image/${type};base64,YQ==`), true);
  for (const value of [
    null,
    {},
    '',
    'https://example.com/avatar.png',
    'data:image/svg+xml;base64,YQ==',
    'data:text/html;base64,YQ==',
    'data:image/png,YQ==',
    'data:image/png;base64,Y Q==',
    'data:image/png;base64,YQ=="',
    'data:image/png;base64,' + 'A'.repeat(180_000),
  ]) {
    assert.equal(ui.safeRasterUrl(value), false);
  }
});

test('upload validates MIME and size before allocating image resources', async () => {
  const { ui, state } = await uiFixture();
  for (const file of [
    { type: 'image/svg+xml', size: 20 },
    { type: 'image/gif', size: 20 },
    { type: 'image/png', size: 8 * 1024 * 1024 + 1 },
  ]) {
    await assert.rejects(ui.rasterUpload(file), /Choose a PNG, JPEG or WebP/);
  }
  assert.equal(state.objectUrls.length, 0);
  assert.equal(state.revoked.length, 0);
});

test('upload center-crops and re-encodes an avatar as JPEG, then releases its object URL', async () => {
  const { ui, state, created } = await uiFixture();
  assert.equal(await ui.rasterUpload({ type: 'image/png', size: 1000 }), state.dataUrl);
  const canvas = created.find((node) => node.tagName === 'CANVAS');
  assert.equal(canvas.width, 192);
  assert.equal(canvas.height, 192);
  assert.deepEqual(state.draws[0].slice(1), [100, 0, 200, 200, 0, 0, 192, 192]);
  assert.deepEqual(state.encoded, [['image/jpeg', 0.8]]);
  assert.deepEqual(state.revoked, ['blob:test-upload']);
});

test('room upload produces the intended landscape crop', async () => {
  const { ui, state } = await uiFixture();
  state.imageWidth = 400;
  state.imageHeight = 400;
  await ui.rasterUpload({ type: 'image/webp', size: 1000 }, 480, 270);
  assert.deepEqual(state.draws[0].slice(1), [0, 87.5, 400, 225, 0, 0, 480, 270]);
});

for (const failure of ['decode', 'dimensions', 'context', 'output size']) {
  test(`upload releases its object URL after ${failure} failure`, async () => {
    const { ui, state } = await uiFixture();
    if (failure === 'decode')
      state.decode = async () => {
        throw new Error('Bad image');
      };
    if (failure === 'dimensions') state.imageWidth = 0;
    if (failure === 'context') state.contextAvailable = false;
    if (failure === 'output size') state.dataUrl = 'A'.repeat(170_001);
    await assert.rejects(ui.rasterUpload({ type: 'image/jpeg', size: 1000 }));
    assert.deepEqual(state.revoked, ['blob:test-upload']);
  });
}
