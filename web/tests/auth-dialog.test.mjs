import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

test('session creation remains visible through every dismissal route until completion', async () => {
  const { AuthDialogFlow } = await loadTypeScript('src/auth-dialog.ts');
  let retirements = 0;
  const flow = new AuthDialogFlow(() => retirements++);
  for (const route of ['close', 'backdrop', 'switch', 'Escape']) {
    const attempt = flow.begin(true);
    assert.ok(attempt, route);
    assert.equal(flow.dismiss(), false, route);
    assert.equal(flow.current(attempt), true);
    assert.equal(attempt.controller.signal.aborted, false);
    assert.equal(flow.begin(true), null, 'no overlapping request can set a different cookie');
    assert.equal(flow.finish(attempt), true);
    assert.equal(flow.dismiss(), true);
  }
  assert.equal(retirements, 0);
});

test('closing a ceremony aborts it and prevents late session creation or retiring a newer attempt', async () => {
  const { AuthDialogFlow } = await loadTypeScript('src/auth-dialog.ts');
  const flow = new AuthDialogFlow(() => {});
  const old = flow.begin(false);
  assert.equal(flow.dismiss(), true);
  assert.equal(old.controller.signal.aborted, true);
  const current = flow.begin(false);
  assert.equal(flow.establish(old), false);
  assert.equal(flow.finish(old), false);
  assert.equal(flow.current(current), true);
  assert.equal(flow.establish(current), true);
  assert.equal(
    flow.dismiss(),
    false,
    'finishing a passkey has the same cookie boundary as password login',
  );
});

test('idle dialog dismissal does not retire startup restoration and uncertain sessions require reload', async () => {
  const { AuthDialogFlow } = await loadTypeScript('src/auth-dialog.ts');
  let retirements = 0;
  const flow = new AuthDialogFlow(() => retirements++);
  assert.equal(flow.dismiss(), true);
  assert.equal(retirements, 0);
  const attempt = flow.begin(true);
  flow.markUncertain(attempt);
  assert.equal(flow.isUncertain(attempt), true);
  assert.equal(flow.dismiss(), false);
  assert.equal(flow.finish(attempt), false);
  assert.equal(flow.begin(true), null);
  assert.equal(retirements, 0);
});
