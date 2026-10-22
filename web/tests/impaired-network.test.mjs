import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import impairment from '../e2e/impaired-network.cjs';

function fixture(t, environment = {}, execute = () => '') {
  const artifacts = fs.mkdtempSync(path.join(os.tmpdir(), 'impaired-suite-fixture.'));
  t.after(() => fs.rmSync(artifacts, { recursive: true, force: true }));
  const commands = [];
  const contexts = [];
  const browser = {
    version: () => 'inert-fixture',
    async newContext() {
      const page = {
        setDefaultTimeout() {},
        async addInitScript() {},
        on() {},
        async goto(url) {
          assert.equal(url, 'http://127.0.0.1:3109');
        },
        locator() {
          return {
            async fill() {},
            async click() {},
            async waitFor() {},
            async isVisible() {
              return false;
            },
          };
        },
        async waitForFunction() {},
        async waitForTimeout() {},
        async evaluate() {
          assert.fail('these control-flow fixtures must never collect browser media');
        },
      };
      const context = {
        closed: false,
        async newPage() {
          return page;
        },
        async close() {
          this.closed = true;
        },
      };
      contexts.push(context);
      return context;
    },
  };
  let browserClosed = false;
  const env = {
    E2E_ARTIFACTS: artifacts,
    E2E_BROWSER: 'chromium',
    IMPAIR_SCRIPT: '/owned/helper with spaces',
    IMPAIR_UDP_PORT: '41104',
    IMPAIR_WORKERS: '4',
    IMPAIR_DIRECTION: 'both',
    PATH: '/owned/tools with spaces',
    // This empty mocked journey reaches successful finalization without media
    // or clock simulation; profile tests fail at the injected apply boundary.
    IMPAIRED_PROFILES: ',',
    ...environment,
  };
  return {
    env,
    commands,
    contexts,
    artifacts,
    browserClosed: () => browserClosed,
    report: () =>
      JSON.parse(fs.readFileSync(path.join(artifacts, 'impaired-network-results.json'))),
    run: () =>
      impairment.run({
        env,
        async launchBrowser(name) {
          assert.equal(name, 'chromium');
          return browser;
        },
        async closeBrowser(value) {
          assert.equal(value, browser);
          browserClosed = true;
        },
        executeImpairment(program, args, options) {
          const command = { program, args, env: options.env };
          commands.push(command);
          return execute(command);
        },
      }),
  };
}

test('argumentless UDP block and cleanup preserve the captured environment and media port', async (t) => {
  const f = fixture(t, { IMPAIRED_BLOCK_UDP: '1' });
  const captured = { ...f.env };
  await f.run();
  assert.deepEqual(
    f.commands.map(({ args }) => args),
    [
      [captured.IMPAIR_SCRIPT, 'block-udp'],
      [captured.IMPAIR_SCRIPT, 'clear'],
      [captured.IMPAIR_SCRIPT, 'unblock-udp'],
    ],
  );
  for (const command of f.commands) {
    assert.equal(command.program, 'sh');
    assert.deepEqual(command.env, { ...captured, IMPAIR_DIRECTION: 'downlink' });
  }
  assert.deepEqual(f.env, captured, 'profile commands do not mutate the captured environment');
  assert.equal(f.report().passed, true);
  assert.ok(f.contexts.every((context) => context.closed));
  assert.equal(f.browserClosed(), true);
});

test('profile overrides merge with captured defaults and retain the original failure through cleanup', async (t) => {
  for (const name of ['lossy', 'lossyJoin']) {
    const original = new Error('owned profile failure');
    const f = fixture(t, { IMPAIRED_PROFILES: name }, ({ args }) => {
      if (args[1] === 'apply') throw original;
      throw new Error('later cleanup failure');
    });
    await assert.rejects(f.run(), (error) => error === original);
    assert.deepEqual(
      f.commands.map(({ args }) => args[1]),
      ['apply', 'clear'],
    );
    assert.deepEqual(f.commands[0].env, {
      ...f.env,
      IMPAIR_DIRECTION: name === 'lossyJoin' ? 'both' : 'downlink',
      IMPAIR_LOSS: '5',
      IMPAIR_DELAY_MS: '50',
      IMPAIR_JITTER_MS: '10',
    });
    assert.equal(f.commands[1].env.IMPAIR_LOSS, undefined);
    assert.equal(f.report().passed, false);
    assert.equal(f.report().failure.message, original.message);
    assert.ok(f.contexts.every((context) => context.closed));
    assert.equal(f.browserClosed(), true);
  }
});

test('final clear or unblock failure prevents success and attempts both independent cleanup actions', async (t) => {
  for (const [block, failed] of [
    [false, 'clear'],
    [true, 'clear'],
    [true, 'unblock-udp'],
  ]) {
    const f = fixture(t, { IMPAIRED_BLOCK_UDP: block ? '1' : '0' }, ({ args }) => {
      if (args[1] === failed) throw new Error('owned cleanup failure');
      return '';
    });
    await assert.rejects(f.run(), /impaired-network phases failed/);
    const report = f.report();
    assert.equal(report.completed, true);
    assert.equal(report.passed, false);
    assert.equal(report.failure.message, `impairment ${failed} failed: owned cleanup failure`);
    assert.deepEqual(
      f.commands.map(({ args }) => args[1]),
      block ? ['block-udp', 'clear', 'unblock-udp'] : ['clear'],
    );
    assert.equal(f.browserClosed(), true);
  }
});

test('mechanics-only mode never dispatches impairment commands', async (t) => {
  const f = fixture(t, { IMPAIR_SCRIPT: 'none' }, () => assert.fail('unexpected command'));
  await f.run();
  assert.equal(f.commands.length, 0);
  assert.equal(f.report().passed, true);
  assert.equal(fs.existsSync(path.join(f.artifacts, 'impairment.log')), false);
});
