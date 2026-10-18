import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';

const source = await readFile(new URL('../e2e/settings-sheet.cjs', import.meta.url), 'utf8');

function fixture(phase, { heading = 'Sign-in & security', unavailable = false } = {}) {
  const failure = new Error('owned-secret-error');
  const logs = [];
  const history = {
    state: { simplestchatNavigation: 4, simplestchatNavigationSession: 'owned-secret-session' },
  };
  const region = {
    hidden: false,
    querySelector: () => ({ textContent: heading }),
    getByRole: () => ({
      async click() {
        if (phase === 'click') throw failure;
      },
    }),
    async waitFor() {
      if (phase === 'hidden') throw failure;
      this.hidden = true;
    },
  };
  const module = { exports: {} };
  runInNewContext(source, {
    module,
    console: { error: (message) => logs.push(JSON.parse(message)) },
    history,
    location: {
      hash: '#room=owned-secret-room&view=settings&section=security&segment=you&peer=owned-secret-peer&contact=owned-secret-contact',
    },
    document: { getElementById: () => region },
    URLSearchParams,
  });
  let evaluations = 0;
  const page = {
    getByRole: () => region,
    async evaluate(callback, argument) {
      evaluations++;
      if (unavailable && evaluations > 1) throw new Error('owned-secret-unavailable');
      return callback(argument);
    },
    async waitForFunction() {
      if (phase === 'history') throw failure;
    },
  };
  return { close: () => module.exports.closeSheet(page), logs, failure };
}

test('settings close diagnostics distinguish click, hidden and history failures without exposing account data', async () => {
  for (const phase of ['click', 'hidden', 'history']) {
    const f = fixture(phase);
    await assert.rejects(f.close(), (error) => error === f.failure);
    assert.deepEqual(f.logs, [
      {
        kind: 'settings-close',
        phase,
        sheetHidden: phase === 'history',
        sheetSection: 'security',
        historyEntry: 4,
        sameHistoryEntry: true,
        sameHistorySession: true,
        route: { view: 'settings', section: 'security', segment: 'you' },
      },
    ]);
    assert.doesNotMatch(JSON.stringify(f.logs), /owned-secret/);
  }
});

test('settings close diagnostics replace unknown headings and inaccessible page state with fixed labels', async () => {
  const unknown = fixture('hidden', { heading: 'owned-secret-heading' });
  await assert.rejects(unknown.close(), (error) => error === unknown.failure);
  assert.equal(unknown.logs[0].sheetSection, 'other');
  assert.doesNotMatch(JSON.stringify(unknown.logs), /owned-secret/);
  const unavailable = fixture('click', { unavailable: true });
  await assert.rejects(unavailable.close(), (error) => error === unavailable.failure);
  assert.deepEqual(unavailable.logs, [
    { kind: 'settings-close', phase: 'click', unavailable: true },
  ]);
});
