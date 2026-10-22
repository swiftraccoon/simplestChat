import assert from "node:assert/strict";
import { ChildProcess } from "node:child_process";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { compileFunction } from "node:vm";

const require = createRequire(import.meta.url);
const { launchOwnedWebKit } = require("../../web/e2e/runner-webkit-owner.cjs");
const { closeOwnedBrowser } = require("../../web/e2e/lifecycle-cleanup.cjs");
const metadata = { version: "1.64.0", platform: "darwin" };
const fixedFailure = /Owned in-process WebKit browser configuration failed\./;

function browserOwnerFixture(playwright) {
  const source = new URL(
    "../../web/e2e/runner-browser-owner.cjs",
    import.meta.url,
  );
  const sourceRequire = createRequire(source);
  const module = { exports: {} };
  const requested = [];
  // Execute the actual identity check without resolving an installed browser
  // package. Only the two unchanged lifecycle helpers may import normally.
  compileFunction(
    readFileSync(source, "utf8"),
    ["require", "module", "exports"],
    {
      filename: fileURLToPath(source),
    },
  )(
    (name) => {
      requested.push(name);
      if (name === "playwright") return playwright;
      assert.ok(
        ["./lifecycle-cleanup.cjs", "./runner-webkit-owner.cjs"].includes(name),
        "the inert owner has no package or dependency fallback",
      );
      return sourceRequire(name);
    },
    module,
    module.exports,
  );
  return { ...module.exports, requested };
}

function fixture() {
  const calls = [];
  const child = new ChildProcess();
  // This fixture never spawns or signals a process. The adapter must retain this
  // exact ChildProcess object; only inert owned close/kill methods can be called.
  child.pid = process.pid;
  const browserProcess = {
    process: child,
    async close() {
      calls.push(["native-close", this === browserProcess]);
      child.exitCode = 0;
    },
    async kill() {
      calls.push(["native-kill", this === browserProcess]);
      child.signalCode = "SIGKILL";
    },
  };
  class _WKBrowser {
    constructor() {
      this.options = { name: "webkit", browserType: "webkit", browserProcess };
    }
  }
  const implementation = new _WKBrowser();
  const connection = {
    toImpl(value) {
      assert.equal(value, browser);
      return implementation;
    },
  };
  const browser = {
    _connection: connection,
    _type: "Browser",
    async close() {
      calls.push(["public-close"]);
      child.exitCode = 0;
    },
  };
  const playwright = {
    _connection: connection,
    webkit: {
      async launch(options) {
        calls.push(["launch", options]);
        return browser;
      },
    },
  };
  return {
    calls,
    child,
    browserProcess,
    implementation,
    connection,
    browser,
    playwright,
  };
}

test("exact-pin in-process WebKit retains the actual owned child and native close/kill receivers", async () => {
  const f = fixture();
  const result = await launchOwnedWebKit(
    f.playwright,
    { headless: true },
    metadata,
  );
  assert.equal(result.browser, f.browser);
  assert.equal(result.server.process(), f.child);
  assert.deepEqual(f.calls, [["launch", { headless: true }]]);
  const report = {},
    retained = [];
  await closeOwnedBrowser(result.server, report, () =>
    retained.push(structuredClone(report)),
  );
  assert.equal(report.browserCleanup.passed, true);
  assert.equal(report.browserCleanup.exitCode, 0);
  assert.deepEqual(f.calls.at(-1), ["native-close", true]);
  assert.equal(retained.at(-1).browserCleanup.exitStatusObserved, true);
  await result.server.kill();
  assert.deepEqual(f.calls.at(-1), ["native-kill", true]);
});

test("wrong WebKit version, platform or connection refuses before browser launch", async () => {
  for (const changes of [{ version: "1.65.0" }, { platform: "linux" }]) {
    const f = fixture();
    await assert.rejects(
      launchOwnedWebKit(f.playwright, {}, { ...metadata, ...changes }),
      fixedFailure,
    );
    assert.deepEqual(f.calls, []);
  }
  const f = fixture();
  delete f.playwright._connection.toImpl;
  await assert.rejects(
    launchOwnedWebKit(f.playwright, {}, metadata),
    fixedFailure,
  );
  assert.deepEqual(f.calls, []);
});

test("unexpected connection, implementation or process shape closes only its launched browser", async () => {
  const mutations = [
    (f) => {
      f.browser._connection = {};
    },
    (f) => {
      f.browser._type = "Other";
    },
    (f) => {
      f.implementation.options.name = "chromium";
    },
    (f) => {
      f.implementation.options.browserType = "chromium";
    },
    (f) => {
      f.browserProcess.process = { pid: process.pid };
    },
    (f) => {
      f.child.pid = undefined;
    },
    (f) => {
      f.browserProcess.close = undefined;
    },
    (f) => {
      f.browserProcess.kill = undefined;
    },
    (f) => {
      f.connection.toImpl = () => {
        throw new Error("PRIVATE_IMPLEMENTATION_PAYLOAD");
      };
    },
  ];
  for (const mutate of mutations) {
    const f = fixture();
    mutate(f);
    await assert.rejects(
      launchOwnedWebKit(f.playwright, {}, metadata),
      fixedFailure,
    );
    assert.deepEqual(f.calls, [["launch", {}], ["public-close"]]);
  }
});

test("refused browser cleanup failures stay fixed and cannot become passing ownership evidence", async () => {
  const f = fixture();
  f.browser._connection = {};
  f.browser.close = async () => {
    f.calls.push(["public-close"]);
    throw new Error("PRIVATE_CLOSE_PAYLOAD");
  };
  await assert.rejects(
    launchOwnedWebKit(f.playwright, {}, metadata),
    (error) => {
      assert.equal(
        error.message,
        "Owned in-process WebKit browser cleanup failed.",
      );
      assert.doesNotMatch(error.message, /PRIVATE/);
      return true;
    },
  );
  assert.deepEqual(f.calls, [["launch", {}], ["public-close"]]);
});

test("opt-in runner refuses a replacement Playwright module before any launch or page work", async () => {
  const f = fixture();
  const { createBrowserOwner, requested } = browserOwnerFixture(
    Object.freeze({}),
  );
  const owner = createBrowserOwner({
    browserName: "webkit",
    playwright: f.playwright,
    persist() {},
    webKitTestNetwork: true,
  });
  await assert.rejects(owner.launchBrowser("webkit", {}), fixedFailure);
  assert.deepEqual(f.calls, []);
  assert.deepEqual(requested, [
    "./lifecycle-cleanup.cjs",
    "./runner-webkit-owner.cjs",
    "playwright",
  ]);
});
