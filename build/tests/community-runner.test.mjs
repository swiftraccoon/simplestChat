import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import test from "node:test";

const root = fileURLToPath(new URL("../../", import.meta.url));
const runner = path.join(root, "web/e2e/community.cjs");
const require = createRequire(import.meta.url);
const runtime = require("../../web/e2e/community-runtime.cjs");

test("startup requires the initialized toolbar, authoritative capabilities and restored signaling", async () => {
  const nodes = new Map();
  const previous = globalThis.document;
  globalThis.document = {
    querySelector: (selector) => nodes.get(selector) ?? null,
  };
  const ready = runtime.communityStartupReady;
  assert.equal(ready(), false);
  nodes.set(".conversation-toolbar", {});
  nodes.set("#sign-in-btn", { hidden: true });
  nodes.set("#connection-status", { classList: { contains: () => false } });
  assert.equal(
    ready(),
    false,
    "static HTML does not establish loaded capabilities",
  );
  nodes.get("#sign-in-btn").hidden = false;
  assert.equal(
    ready(),
    false,
    "capabilities alone do not establish restored auth and signaling",
  );
  nodes.get("#connection-status").classList.contains = (name) =>
    name === "connected";
  assert.equal(ready(), true);
  nodes.delete(".conversation-toolbar");
  assert.equal(
    ready(),
    false,
    "initialized account/signaling state still requires the client UI",
  );
  globalThis.document = previous;
});

test("initial and invitation navigation use bounded app readiness without network-idle waits or reload retries", async () => {
  const source = await readFile(runner, "utf8");
  const calls = [],
    page = {
      async goto(url, options) {
        calls.push(["navigation", url, options]);
      },
      async waitForFunction(predicate, argument, options) {
        calls.push([
          "ready",
          typeof predicate === "function",
          argument,
          options,
        ]);
      },
    };
  const clients = [
    {
      page,
      testNetwork: {
        async ready() {
          calls.push(["network"]);
        },
      },
    },
  ];
  const navigate = (page, url) =>
    runtime.navigateCommunityPage(page, url, clients);
  await navigate(page, "https://127.0.0.1:3120/");
  await navigate(page, "https://127.0.0.1:3120/#invite=owned-code");
  assert.deepEqual(JSON.parse(JSON.stringify(calls)), [
    [
      "navigation",
      "https://127.0.0.1:3120/",
      { waitUntil: "domcontentloaded" },
    ],
    ["network"],
    ["ready", true, null, { timeout: 10000 }],
    [
      "navigation",
      "https://127.0.0.1:3120/#invite=owned-code",
      { waitUntil: "domcontentloaded" },
    ],
    ["network"],
    ["ready", true, null, { timeout: 10000 }],
  ]);
  assert.doesNotMatch(source, /waitUntil: 'networkidle'|page\.reload\(/);
});

test("community steps distinguish successful checks, unsupported simulations, and failures", async () => {
  // Exercise the actual reporter without browser or database side effects.
  const report = { steps: [] },
    saved = [],
    messages = [],
    phases = [];
  let networkChecks = 0;
  const fixture = {
    report,
    httpObserved: 7,
    setActiveStep() {},
    get observedHttpRequests() {
      return this.httpObserved;
    },
    saveReport() {
      saved.push(JSON.parse(JSON.stringify(report)));
    },
    async paceHttpRequests() {
      phases.push("pace");
      fixture.httpObserved += 2;
    },
    async testNetworkReady() {
      phases.push("ready");
      networkChecks++;
    },
    log(message) {
      messages.push(message);
    },
  };
  const step = runtime.createCommunityStep(fixture);
  await step("native check", async () => {
    phases.push("work");
    fixture.httpObserved += 3;
  });
  await step("unsupported simulation", async () => {
    phases.push("work");
    fixture.httpObserved++;
    return { skip: "No synthetic event delivery" };
  });
  await assert.rejects(
    step("broken check", async () => {
      phases.push("work");
      throw new Error("owned failure");
    }),
    /owned failure/,
  );
  assert.deepEqual(JSON.parse(JSON.stringify(report.steps)), [
    { name: "native check", passed: true, observedHttpRequests: 3 },
    {
      name: "unsupported simulation",
      passed: false,
      observedHttpRequests: 1,
      skipped: true,
      reason: "No synthetic event delivery",
    },
    { name: "broken check", passed: false },
  ]);
  assert.equal(
    saved.length,
    5,
    "started checks and their outcomes must be persisted",
  );
  assert.deepEqual(messages, [
    "PASS native check",
    "SKIP unsupported simulation: No synthetic event delivery",
  ]);
  assert.equal(
    networkChecks,
    5,
    "configuration is checked before each step and after completed work",
  );
  assert.deepEqual(
    phases,
    [
      "pace",
      "ready",
      "work",
      "ready",
      "pace",
      "ready",
      "work",
      "ready",
      "pace",
      "ready",
      "work",
    ],
    "pacing precedes configuration and work; per-step counts exclude requests observed while pacing",
  );
});

test("a failed owned test-network configuration cannot mark a community step passed or skipped", async () => {
  const source = await readFile(runner, "utf8");
  for (const failingCheck of [1, 2]) {
    const report = { steps: [] },
      messages = [],
      phases = [];
    let checks = 0,
      workRan = false;
    const fixture = {
      httpObserved: 0,
      setActiveStep() {},
      get observedHttpRequests() {
        return this.httpObserved;
      },
      report,
      saveReport() {},
      log(value) {
        messages.push(value);
      },
      async paceHttpRequests() {
        phases.push("pace");
      },
      async testNetworkReady() {
        phases.push("ready");
        if (++checks === failingCheck)
          throw new Error("owned configuration failed");
      },
    };
    const step = runtime.createCommunityStep(fixture);
    await assert.rejects(
      step("configured native check", async () => {
        phases.push("work");
        fixture.httpObserved += 2;
        workRan = true;
        return { skip: "unsupported simulation" };
      }),
      /owned configuration failed/,
    );
    assert.equal(workRan, failingCheck === 2);
    assert.deepEqual(JSON.parse(JSON.stringify(report.steps)), [
      {
        name: "configured native check",
        passed: false,
        ...(failingCheck === 2 && { observedHttpRequests: 2 }),
      },
    ]);
    assert.deepEqual(messages, []);
    assert.deepEqual(
      phases,
      failingCheck === 1
        ? ["pace", "ready"]
        : ["pace", "ready", "work", "ready"],
    );
  }
});

test("a failed HTTP request budget prevents community work and a passing report", async () => {
  const report = { steps: [] },
    messages = [];
  let networkChecks = 0,
    workRan = false,
    counterReads = 0;
  const step = runtime.createCommunityStep({
    setActiveStep() {},
    report,
    saveReport() {},
    log(value) {
      messages.push(value);
    },
    get observedHttpRequests() {
      counterReads++;
      return 0;
    },
    async paceHttpRequests() {
      throw new Error("owned request budget unavailable");
    },
    async testNetworkReady() {
      networkChecks++;
    },
  });
  await assert.rejects(
    step("budgeted native check", async () => {
      workRan = true;
    }),
    /owned request budget unavailable/,
  );
  assert.equal(workRan, false);
  assert.equal(
    networkChecks,
    0,
    "work and its configuration checks remain behind request admission",
  );
  assert.equal(
    counterReads,
    0,
    "no per-step observation starts before request admission",
  );
  assert.deepEqual(JSON.parse(JSON.stringify(report.steps)), [
    { name: "budgeted native check", passed: false },
  ]);
  assert.deepEqual(messages, []);
});

test("only explicit successful retirement removes an owned client from network readiness checks", async () => {
  const calls = [],
    firstPage = {},
    secondPage = {};
  const clients = [
    {
      page: firstPage,
      testNetwork: {
        async ready() {
          calls.push("first-ready");
        },
        async dispose() {
          calls.push("first-dispose");
        },
      },
    },
    {
      page: secondPage,
      testNetwork: {
        async ready() {
          calls.push("second-ready");
        },
        async dispose() {
          calls.push("second-dispose");
        },
      },
    },
  ];
  const { testNetworkReady, retireTestNetwork } =
    runtime.createTestNetworkOwnership(clients);
  await retireTestNetwork(firstPage);
  await testNetworkReady();
  assert.deepEqual(calls, ["first-ready", "first-dispose", "second-ready"]);
  assert.equal(
    clients.length,
    2,
    "retired clients remain available as evidence and for owned browser cleanup",
  );
  assert.equal(clients[0].testNetworkRetired, true);
  assert.equal(clients[1].testNetworkRetired, undefined);
  clients[1].testNetwork.ready = async () => {
    throw new Error("unexpected session disposal");
  };
  await assert.rejects(testNetworkReady(), /unexpected session disposal/);
  await assert.rejects(
    retireTestNetwork(secondPage),
    /unexpected session disposal/,
  );
  assert.equal(clients[1].testNetworkRetired, undefined);
  await assert.rejects(retireTestNetwork({}), /only an owned community client/);
});

async function fixture(t, toolingSource) {
  const directory = await mkdtemp(
    path.join(os.tmpdir(), "simplestchat-browser-runner."),
  );
  t.after(() => rm(directory, { recursive: true, force: true }));
  const tooling = path.join(directory, "fake tooling");
  await mkdir(tooling);
  await writeFile(
    path.join(tooling, "package.json"),
    JSON.stringify({ version: "0.0.0-fixture", main: "index.cjs" }),
  );
  await writeFile(
    path.join(tooling, "index.cjs"),
    toolingSource ??
      `module.exports = {
    chromium: { async launch() { throw new Error('owned fixture launch failure'); } },
    firefox: { async launch() { throw new Error('owned fixture launch failure'); } },
  };\n`,
  );
  const artifacts = path.join(directory, "artifacts");
  const env = {
    PATH: process.env.PATH,
    COMMUNITY_E2E: "1",
    BASE_URL: "http://127.0.0.1:3119",
    PLAYWRIGHT_MODULE: tooling,
    E2E_ARTIFACTS: artifacts,
  };
  return {
    artifacts,
    run: (overrides) =>
      spawnSync(
        process.execPath,
        [
          "-e",
          "require(process.argv[1]).run().catch(error => { console.error(error.message); process.exitCode = 1; })",
          runner,
        ],
        {
          cwd: root,
          env: { ...env, ...overrides },
          encoding: "utf8",
          timeout: 10_000,
        },
      ),
  };
}

function pageFailureTooling(diagnosticsFail = false) {
  return `
    const { runInNewContext } = require('node:vm');
    const listeners = new Map();
    const peer = {
      addEventListener(type, listener) { listeners.set(type, listener); },
      connectionState: 'new', iceConnectionState: 'new', signalingState: 'stable', iceGatheringState: 'complete',
      localDescription: null,
      remoteDescription: { type: 'offer', sdp: [
        'v=0', 'a=ice-pwd:OWNED_ICE_SECRET', 'm=video 9 UDP/TLS/RTP/SAVPF 96',
        'a=mid:0', 'a=sendonly', 'a=rtpmap:96 VP8/90000',
        'a=candidate:PRIVATE_FOUNDATION 1 udp 1234 192.0.2.73 41010 typ host',
      ].join('\\r\\n') },
      getTransceivers() { return [{ mid: '0', direction: 'recvonly', currentDirection: null, stopped: false,
        receiver: { track: { kind: 'video', enabled: true, muted: true, readyState: 'live', id: 'OWNED_TRACK_SECRET' } },
      }]; },
      async getStats() { return new Map([['T01', { id: 'T01', type: 'transport', iceState: 'new', dtlsState: 'new', bytesReceived: 0, bytesSent: 0 }]]); },
    };
    const window = {
      async fetch() { throw new Error('fixture must not fetch'); },
      RTCPeerConnection: class { constructor() { return peer; } },
    };
    const Notification = class {};
    const navigator = { mediaDevices: { getUserMedia() { throw new Error('fixture must not capture'); } } };
    const page = {
      setDefaultTimeout() {}, on() {}, async route() {},
      async addInitScript(fn, argument) {
        runInNewContext('(' + fn.toString() + ')(argument)', { window, navigator, performance, URL, EventTarget, Notification, argument });
      },
      async goto() {
        new window.RTCPeerConnection();
        listeners.get('icecandidate')({ candidate: { candidate: 'candidate:PRIVATE_FOUNDATION 1 udp 1234 192.0.2.73 41010 typ host ufrag OWNED_ICE_SECRET' } });
        throw new Error('owned fixture navigation failure');
      },
      async evaluate(fn) {
        if (${diagnosticsFail}) throw new Error('owned fixture diagnostics failure');
        return runInNewContext('(' + fn.toString() + ')()', { window, performance, setTimeout, clearTimeout });
      },
      locator() { return { async evaluateAll() {}, async isVisible() { return false; } }; },
      async screenshot() {},
    };
    module.exports = { chromium: { async launch() { return {
      version() { return 'fixture-browser'; },
      async newContext(options) {
        // The cold-start measurement is a separate owned page before the
        // diagnostic client. Stub it without executing any browser/native code.
        if (options.viewport.width === 390) return {
          async newCDPSession() { return { async send() {} }; },
          async newPage() { return {
            on() {}, async addInitScript() {}, async goto() {}, async waitForFunction() {},
            async evaluate() { return { captureRequests: 0 }; },
          }; },
          async close() {},
        };
        return { async newPage() { return page; }, async close() {} };
      },
      async close() {},
    }; } } };
  `;
}

test("community runner collects sanitized peer state on failure without replacing the failed outcome", async (t) => {
  const { artifacts, run } = await fixture(t, pageFailureTooling());
  const result = run({});
  assert.equal(result.status, 1, result.stderr);
  const report = JSON.parse(
    await readFile(path.join(artifacts, "community-results.json"), "utf8"),
  );
  assert.equal(report.passed, false);
  assert.equal(report.complete, false);
  assert.equal(report.error, "owned fixture navigation failure");
  assert.equal(report.diagnostics.length, 1);
  const peer = report.diagnostics[0].media[0];
  assert.equal(peer.iceGatheringState, "complete");
  assert.equal(peer.descriptions.remote.media[0].direction, "sendonly");
  assert.equal(peer.descriptions.remote.media[0].candidates.count, 1);
  assert.equal(peer.transceivers[0].currentDirection, null);
  assert.equal(peer.stats[0].bytesReceived, 0);
  assert.equal(peer.iceEvents.length, 2);
  assert.equal(peer.iceEvents[1].phase, "candidate");
  assert.equal(peer.iceEvents[1].candidate.addressKind, "ipv4");
  assert.equal(peer.iceEventsDropped, 0);
  assert.ok(peer.iceEventsObservedAtMs >= peer.iceEvents[1].elapsedMs);
  assert.doesNotMatch(
    JSON.stringify(report.diagnostics),
    /OWNED_ICE_SECRET|OWNED_TRACK_SECRET|PRIVATE_FOUNDATION|192\.0\.2\.73/,
  );
});

test("community runner preserves the original failure when collecting peer diagnostics fails", async (t) => {
  const { artifacts, run } = await fixture(t, pageFailureTooling(true));
  const result = run({});
  assert.equal(result.status, 1, result.stderr);
  const report = JSON.parse(
    await readFile(path.join(artifacts, "community-results.json"), "utf8"),
  );
  assert.equal(report.passed, false);
  assert.equal(report.error, "owned fixture navigation failure");
  assert.deepEqual(report.diagnostics[0].media, {
    error: "owned fixture diagnostics failure",
  });
});

for (const failedBeforeCleanup of [false, true]) {
  for (const failingBrowser of [0, 1]) {
    test(`community browser ${failingBrowser} cleanup failure ${failedBeforeCleanup ? "preserves the original failure" : "fails an otherwise successful run"}`, async () => {
      const report = { complete: true, passed: true };
      const originalFailure = new Error("owned original failure");
      const cleanupFailure = new Error("owned close failure");
      const saved = [];
      const closed = [];
      const owned = [0, 1, 2].map((index) => ({
        async close() {
          // A later close must finish before returning even when an earlier one
          // rejects. These represent independently owned WebKit capture clients.
          await new Promise((resolve) => setTimeout(resolve, index * 5));
          closed.push(index);
          if (index === failingBrowser) throw cleanupFailure;
        },
      }));
      const run = async () => {
        let failure;
        try {
          if (failedBeforeCleanup) throw originalFailure;
        } finally {
          failure = await runtime.closeCommunityClients({
            report,
            clients: [],
            browser: owned[0],
            additionalBrowsers: owned.slice(1),
            deadline: (work) => work,
            saveReport() {
              saved.push({ ...report });
            },
          });
        }
        if (failure) throw failure;
      };
      await assert.rejects(
        run(),
        (error) =>
          error === (failedBeforeCleanup ? originalFailure : cleanupFailure),
      );
      assert.deepEqual(closed.sort(), [0, 1, 2]);
      assert.equal(report.complete, false);
      assert.equal(report.passed, false);
      assert.equal(report.cleanupError, cleanupFailure.message);
      assert.ok(Date.parse(report.finishedAt));
      assert.equal(saved.length, 1);
      assert.deepEqual(saved[0], report);
    });
  }
}

test("community peer snapshots remain failure-only", async () => {
  const source = await readFile(runner, "utf8");
  const failureStart = source.indexOf("report.failedStep = activeStep;");
  const cleanupStart = source.indexOf("\n  } finally {", failureStart);
  const call = "item.page.evaluate(collectPeerDiagnostics)";
  assert.ok(failureStart >= 0 && cleanupStart > failureStart);
  assert.equal(source.split(call).length - 1, 1);
  assert.ok(source.slice(failureStart, cleanupStart).includes(call));
});

test("community runner refuses missing disposable-server opt-in before launching", async (t) => {
  const { artifacts, run } = await fixture(t);
  const result = run({ COMMUNITY_E2E: "" });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Set COMMUNITY_E2E=1/);
  await assert.rejects(
    readFile(path.join(artifacts, "community-results.json")),
    { code: "ENOENT" },
  );
});

test("community runner rejects unknown engines without silently testing Chromium", async (t) => {
  const { artifacts, run } = await fixture(t);
  const result = run({ E2E_BROWSER: "safari" });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Unsupported E2E browser/);
  await assert.rejects(
    readFile(path.join(artifacts, "community-results.json")),
    { code: "ENOENT" },
  );
});

for (const browser of ["chromium", "firefox"]) {
  test(`community runner preserves a failed ${browser} launch report and exits nonzero`, async (t) => {
    const { artifacts, run } = await fixture(t);
    const result = run({ E2E_BROWSER: browser });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /owned fixture launch failure/);
    const report = JSON.parse(
      await readFile(path.join(artifacts, "community-results.json"), "utf8"),
    );
    assert.equal(report.browser, browser);
    assert.equal(report.browserVersion, null);
    assert.equal(report.playwrightVersion, "0.0.0-fixture");
    assert.equal(report.complete, false);
    assert.equal(report.passed, false);
    assert.equal(report.failedStep, "launch");
    assert.equal(report.error, "owned fixture launch failure");
    assert.deepEqual(report.steps, []);
    assert.ok(Date.parse(report.finishedAt) >= Date.parse(report.startedAt));
  });
}

test("HTTP workload admission separates account data from credentials and scrubs dynamic identities", async () => {
  const httpCounts = new Map();
  const classify = (request) =>
    runtime.httpRoute(request, "https://owned.test", httpCounts);
  const request = (path) => ({
    url: () => `https://owned.test${path}`,
    method: () => "GET",
  });
  for (const route of [
    "profile",
    "preferences",
    "attachments",
    "contacts",
    "saved-rooms",
    "notification-preferences",
    "inbox",
    "push",
    "updates",
  ]) {
    assert.equal(
      classify(request(`/api/auth/${route}`)).lane,
      "account",
      route,
    );
    assert.equal(
      classify(
        request(`/api/auth/${route}/OWNED_PRIVATE_ID?token=OWNED_SECRET`),
      ).lane,
      "account",
      route,
    );
  }
  for (const route of [
    "login",
    "register",
    "refresh",
    "logout",
    "sessions",
    "password",
    "recovery/key",
    "passkeys/enroll",
    "ws-ticket",
  ])
    assert.equal(classify(request(`/api/auth/${route}`)).lane, "auth", route);
  assert.equal(
    classify(request("/api/auth/profiles/OWNED_PRIVATE_ID")).lane,
    "profiles",
  );
  assert.equal(
    classify(request("/api/rooms/OWNED_ROOM/image")).lane,
    "profiles",
  );
  assert.equal(
    classify(request("/api/rooms/OWNED_ROOM/settings")).lane,
    "rooms",
  );
  assert.equal(
    classify({
      url: () => "https://other.test/api/auth/updates",
      method: () => "GET",
    }),
    null,
  );
  assert.doesNotMatch(JSON.stringify([...httpCounts.keys()]), /OWNED_|token=/);
});
