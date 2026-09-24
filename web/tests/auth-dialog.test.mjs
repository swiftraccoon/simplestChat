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

test('an external account change retires ceremony, creating, and uncertain attempts', async () => {
  const { AuthDialogFlow } = await loadTypeScript('src/auth-dialog.ts');
  for (const state of ['ceremony', 'creating', 'uncertain']) {
    let retirements = 0;
    const flow = new AuthDialogFlow(() => retirements++);
    const old = flow.begin(state !== 'ceremony');
    assert.ok(old, state);
    if (state === 'uncertain') flow.markUncertain(old);
    flow.retire();
    assert.equal(old.controller.signal.aborted, true, state);
    assert.equal(flow.current(old), false, state);
    assert.equal(flow.isUncertain(old), false, state);
    assert.equal(retirements, 1, state);
    const current = flow.begin(false);
    assert.ok(current, state);
    assert.equal(flow.establish(old), false, state);
    assert.equal(flow.finish(old), false, state);
    flow.markUncertain(old);
    assert.equal(flow.isUncertain(current), false, state);
    assert.equal(flow.current(current), true, state);
    assert.equal(flow.finish(current), true, state);
    flow.retire();
    assert.equal(retirements, 1, 'idle retirement does not cancel unrelated restoration');
  }
});

test('external retirement resets dialog ownership before abort callbacks run', async () => {
  const { AuthDialogFlow } = await loadTypeScript('src/auth-dialog.ts');
  const flow = new AuthDialogFlow(() => {});
  const old = flow.begin(true);
  let replacement;
  old.controller.signal.addEventListener('abort', () => {
    replacement = flow.begin(false);
  });
  flow.markUncertain(old);
  flow.retire();
  assert.ok(replacement);
  assert.equal(flow.current(replacement), true);
  assert.equal(flow.isUncertain(replacement), false);
  assert.equal(flow.finish(old), false);
});
