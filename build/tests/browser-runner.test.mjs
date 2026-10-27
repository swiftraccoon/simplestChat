import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { runInNewContext } from "node:vm";

const require = createRequire(import.meta.url);
const {
  createBrowserOwner,
} = require("../../web/e2e/runner-browser-owner.cjs");
const SafeReporter = require("../../web/e2e/runner-reporter.cjs");

test("ui stress marks a returned negative workload report as a failed runner step", async () => {
  const source = await readFile(
    new URL("../../web/e2e/specs/stress.spec.cjs", import.meta.url),
    "utf8",
  );
  for (const passed of [false, true]) {
    let scenario, assertionFailure, stepFailure;
    let runs = 0;
    const steps = [];
    const suiteRuntime = {
      env: { UI_STRESS_E2E: "1" },
      async step(name, work) {
        try {
          await work();
          steps.push({ name, passed: true });
        } catch (error) {
          stepFailure = error;
          steps.push({ name, passed: false });
          throw error;
        }
      },
    };
    runInNewContext(source, {
      require(id) {
        if (id === "../runner-fixtures.cjs")
          return {
            test(name, work) {
              assert.equal(name, "ui-stress");
              assert.equal(scenario, undefined);
              scenario = work;
            },
            expect(actual) {
              return {
                toBe(expected) {
                  try {
                    assert.equal(actual, expected);
                  } catch (error) {
                    assertionFailure = error;
                    throw error;
                  }
                },
              };
            },
          };
        assert.equal(id, "../ui-stress.cjs");
        return {
          async run(env, runtime) {
            assert.equal(env, suiteRuntime.env);
            assert.equal(runtime, suiteRuntime);
            runs++;
            return { passed };
          },
        };
      },
    });
    assert.equal(typeof scenario, "function");
    const operation = scenario({ suiteRuntime });
    if (passed) await operation;
    else {
      await assert.rejects(operation, (error) => error === assertionFailure);
      assert.equal(stepFailure, assertionFailure);
    }
    assert.equal(runs, 1);
    assert.deepEqual(steps, [
      { name: "bounded authenticated room workload and cleanup", passed },
    ]);
  }
});

function fixture({ connectFailure = false, saveFailure = false } = {}) {
  const servers = [],
    snapshots = [];
  const playwright = {
    chromium: {
      async launchServer(options) {
        const child = { exitCode: null, signalCode: null };
        const server = {
          options,
          closes: 0,
          kills: 0,
          process: () => child,
          wsEndpoint: () => "fixture-owned-endpoint",
          async close() {
            this.closes++;
            child.exitCode = 0;
          },
          async kill() {
            this.kills++;
            child.signalCode = "SIGKILL";
          },
        };
        servers.push(server);
        return server;
      },
      async connect(endpoint) {
        assert.equal(endpoint, "fixture-owned-endpoint");
        if (connectFailure) throw new Error("owned connection failed");
        return {};
      },
    },
  };
  const owner = createBrowserOwner({
    browserName: "chromium",
    playwright,
    persist(records) {
      if (saveFailure) throw new Error("PRIVATE_PERSISTENCE_PAYLOAD");
      snapshots.push(structuredClone(records));
    },
  });
  return { owner, servers, snapshots };
}

test("runner owns exact BrowserServers and records clean observed exits for every remaining browser", async () => {
  const f = fixture();
  const first = await f.owner.launchBrowser("chromium", { headless: true });
  await f.owner.launchBrowser("chromium", { headless: true });
  assert.equal(f.owner.browserServer(first), f.servers[0]);
  assert.deepEqual(f.servers[0].options, { headless: true });
  await f.owner.closeAll();
  assert.deepEqual(
    f.servers.map((server) => [server.closes, server.kills]),
    [
      [1, 0],
      [1, 0],
    ],
  );
  assert.throws(() => f.owner.browserServer(first), /not owned/);
  assert.ok(
    f.snapshots
      .at(-1)
      .every(
        (record) =>
          record.browserCleanup.passed && record.browserCleanup.exitCode === 0,
      ),
  );
});

test("explicit suite close transfers cleanup evidence and teardown cannot close it twice", async () => {
  const f = fixture();
  const browser = await f.owner.launchBrowser("chromium", {});
  const report = {},
    retained = [];
  await f.owner.closeBrowser(browser, report, () =>
    retained.push(structuredClone(report)),
  );
  await f.owner.closeAll();
  assert.equal(f.servers[0].closes, 1);
  assert.equal(report.browserCleanup.passed, true);
  assert.equal(retained.at(-1).browserCleanup.exitStatusObserved, true);
  await f.owner.closeBrowser({}, report);
  assert.equal(f.servers[0].closes, 1, "unowned browser is never closed");
});

test("a failed browser connection closes its already-owned process before rejecting", async () => {
  const f = fixture({ connectFailure: true });
  await assert.rejects(
    f.owner.launchBrowser("chromium", {}),
    /owned connection failed/,
  );
  assert.equal(f.servers[0].closes, 1);
  assert.equal(f.snapshots.at(-1)[0].browserCleanup.passed, true);
  await f.owner.closeAll();
  assert.equal(f.servers[0].closes, 1);
});

test("a report persistence failure still closes the process and fails with bounded cleanup evidence", async () => {
  const f = fixture({ saveFailure: true });
  await assert.rejects(
    f.owner.launchBrowser("chromium", {}),
    /owned cleanup failed/,
  );
  assert.equal(f.servers[0].closes, 1);
  assert.equal(f.servers[0].kills, 0);
});

test("runner project mismatch is rejected before launching a browser", async () => {
  const f = fixture();
  await assert.rejects(
    f.owner.launchBrowser("firefox", {}),
    /match its runner project/,
  );
  assert.deepEqual(f.servers, []);
});

test("safe runner reports retain fixed step outcomes and omit action parameters and native error payloads", async (t) => {
  const artifacts = await mkdtemp(
    path.join(os.tmpdir(), "simplestchat-runner-report-"),
  );
  t.after(() => rm(artifacts, { recursive: true, force: true }));
  const reporter = new SafeReporter({ artifacts });
  const item = {
    title: "owned account journey",
    parent: { project: () => ({ name: "accounts-chromium" }) },
  };
  const result = {
    retry: 1,
    duration: 12,
    status: "failed",
    errors: [{ message: "PRIVATE_PASSWORD", stack: "PRIVATE_COOKIE" }],
  };
  reporter.onStepEnd(item, result, {
    category: "pw:api",
    title: "fill PRIVATE_PASSWORD",
    duration: 2,
  });
  reporter.onStepEnd(item, result, {
    category: "test.step",
    title: "fixed scenario",
    duration: 3,
    error: { message: "PRIVATE_COOKIE" },
  });
  reporter.onTestEnd(item, result);
  reporter.onEnd({ status: "failed" });
  const report = await readFile(
    path.join(artifacts, "runner-results.json"),
    "utf8",
  );
  const timeline = await readFile(
    path.join(artifacts, "step-trace.json"),
    "utf8",
  );
  const html = await readFile(
    path.join(artifacts, "runner-results.html"),
    "utf8",
  );
  assert.doesNotMatch(report + timeline + html, /PRIVATE_|pw:api|fill /);
  assert.equal(JSON.parse(report).tests[0].errors, 1);
  assert.deepEqual(JSON.parse(timeline), [
    {
      project: "accounts-chromium",
      test: item.title,
      retry: 1,
      step: "fixed scenario",
      durationMs: 3,
      passed: false,
    },
  ]);
  for (const name of [
    "runner-results.json",
    "step-trace.json",
    "runner-results.html",
  ])
    assert.equal((await stat(path.join(artifacts, name))).mode & 0o777, 0o600);
});
