// Execute the unchanged 76 reference, including its own assertions and cleanup.
// Only owned tiny files, inert EventEmitters and a virtual clock are admitted.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { EventEmitter } from "node:events";
import { readFileSync } from "node:fs";
import { mkdtemp, readFile, readdir, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const root = dirname(fileURLToPath(import.meta.url));
const reference = join(root, "reference");
const sources = new Map();
for (const [name, digest] of [
  [
    "build/restart-browser-smoke.mjs",
    "1c68528ede669695dcd63bb1a6f8a398d8e331ab18ac42b8e8f73e20ca34a485",
  ],
  [
    "build/shutdown-smoke.mjs",
    "573c35b173491b99046ef7a98c639cb5cdfcc9a4bf52f4db1a3ebbfdee398f39",
  ],
  [
    "web/e2e/lifecycle-cleanup.cjs",
    "c8707b96ddd3a1bc9582478e91401f980ba2dc692af077dc74a99576cfb23ac4",
  ],
]) {
  const bytes = readFileSync(join(reference, name));
  assert.equal(createHash("sha256").update(bytes).digest("hex"), digest);
  sources.set(name, bytes.toString("utf8"));
}
const binary = "/owned/inert/simplestChat";
const ports = { http: 45101, media: 45102 };
const privateText = "PRIVATE_REFERENCE_FIXTURE";

async function modules() {
  let clock = 0;
  const timers = new Map();
  const signals = new EventEmitter();
  Object.assign(signals, {
    argv: ["node"],
    env: { PATH: "/owned/inert/path" },
  });
  const advance = (milliseconds) => {
    clock += milliseconds;
    for (const [timer, item] of [...timers]) {
      if (item.deadline <= clock && timers.delete(timer)) item.callback();
    }
  };
  const context = vm.createContext({
    AbortController,
    AbortSignal,
    DOMException,
    URL,
    Buffer,
    console,
    process: signals,
    performance: { now: () => clock },
    setTimeout(callback, milliseconds) {
      const timer = {};
      timers.set(timer, { callback, deadline: clock + milliseconds });
      return timer;
    },
    clearTimeout: (timer) => timers.delete(timer),
  });
  const clone = (value) =>
    vm.runInContext(
      `JSON.parse(${JSON.stringify(JSON.stringify(value))})`,
      context,
    );
  const cleanup = vm.runInContext(
    `(function() { const module = {exports:{}}; const exports = module.exports;\n${sources.get("web/e2e/lifecycle-cleanup.cjs")}\nreturn module.exports; })()`,
    context,
  );
  const cache = new Map();
  const load = async (name) => {
    if (cache.has(name)) return cache.get(name);
    const module = new vm.SourceTextModule(sources.get(name), {
      context,
      identifier: join(reference, name),
      initializeImportMeta(meta) {
        meta.url = new URL(name, `file://${reference}/`).href;
      },
    });
    cache.set(name, module);
    await module.link(async (specifier) => {
      if (specifier === "./shutdown-smoke.mjs")
        return load("build/shutdown-smoke.mjs");
      assert(
        specifier.startsWith("node:"),
        "Only original built-in imports are admitted",
      );
      let values;
      if (specifier === "node:timers/promises") {
        values = {
          setTimeout: async (milliseconds) => {
            advance(milliseconds);
            await Promise.resolve();
          },
        };
      } else if (specifier === "node:module") {
        values = {
          createRequire: () => (name) => {
            assert.equal(name, "../web/e2e/lifecycle-cleanup.cjs");
            return cleanup;
          },
        };
      } else if (specifier === "node:child_process") {
        values = { spawn: () => assert.fail("No actual child is permitted") };
      } else if (specifier === "node:net" || specifier === "node:dgram") {
        values = {
          default: new Proxy(
            {},
            { get: () => () => assert.fail("No actual socket is permitted") },
          ),
        };
      } else values = await import(specifier);
      const names = Object.keys(values);
      return new vm.SyntheticModule(
        names,
        function () {
          for (const key of names) this.setExport(key, values[key]);
        },
        { context },
      );
    });
    return module;
  };
  const entry = await load("build/restart-browser-smoke.mjs");
  await entry.evaluate();
  return {
    entry: entry.namespace,
    advance,
    clone,
    signals,
    timers,
    now: () => clock,
  };
}

function fixture(options, runtime, cancellation) {
  let invariantError;
  const check = new Proxy(assert, {
    get(target, name) {
      const value = target[name];
      if (typeof value !== "function") return value;
      return (...args) => {
        try {
          return value(...args);
        } catch (error) {
          invariantError ??= error;
          throw error;
        }
      };
    },
  });
  const trace = {
    validate: 0,
    reserve: 0,
    spawn: 0,
    ready: 0,
    launches: 0,
    connections: 0,
    contexts: 0,
    routes: 0,
    scripts: 0,
    browserCloses: 0,
    browserKills: 0,
    captureDenials: 0,
    order: [],
    environment: null,
  };
  const pages = [],
    children = [],
    scripts = [];
  let routeHandler,
    navigationCounts = null;
  const cancel = () => cancellation.abort();
  const browserProcess = { exitCode: null, signalCode: null };
  const browserServer = {
    process: () => browserProcess,
    wsEndpoint: () => "owned-browser-endpoint",
    async close() {
      trace.browserCloses++;
      trace.order.push("browser-close");
      if (options.lateCleanupExternal) {
        await routeHandler({
          request: () => ({ url: () => "https://PRIVATE.invalid" }),
          continue: async () =>
            check.fail("An external cleanup request must be blocked"),
          abort: async () => {
            trace.order.push("late-external-blocked");
          },
        });
      }
      if (options.lateCleanupPageError)
        pages[0]?.emit("pageerror", Error(privateText));
      if (options.cleanupFailure || options.closeFailure)
        throw Error(privateText);
      if (options.closeTimeout) {
        runtime.advance(10_000);
        return new Promise(() => {});
      }
      browserProcess.exitCode = options.browserExit ?? 0;
    },
    async kill() {
      trace.browserKills++;
      trace.order.push("browser-kill");
      if (options.browserKillFailure) throw Error(privateText);
      browserProcess.signalCode = "SIGKILL";
    },
  };
  const newSocket = (page) => {
    const socket = new EventEmitter();
    socket.url = () =>
      `ws://127.0.0.1:${ports.http}/${options.foreignSocket ? "PRIVATE" : "ws"}`;
    socket.receive = (message) =>
      socket.emit("framereceived", { payload: JSON.stringify(message) });
    socket.send = (message) =>
      socket.emit("framesent", { payload: JSON.stringify(message) });
    page.socket = socket;
    page.emit("websocket", socket);
    return socket;
  };
  class Page extends EventEmitter {
    constructor(index) {
      super();
      this.index = index;
      this.frame = {};
      this.values = {};
      this.messages = [];
      this.navigations = 0;
      this.realm = vm.createContext({
        navigator: { mediaDevices: {} },
        DOMException,
      });
      for (const script of scripts)
        vm.runInContext(`(${script.toString()})()`, this.realm);
      check.equal(
        vm.runInContext(
          "['getUserMedia','getDisplayMedia'].every(name => typeof navigator.mediaDevices[name] === 'function') && ['getUserMedia','webkitGetUserMedia','mozGetUserMedia'].every(name => typeof navigator[name] === 'function')",
          this.realm,
        ),
        true,
      );
    }
    mainFrame() {
      return this.frame;
    }
    async goto(url, init) {
      check.equal(url, "http://127.0.0.1:45101");
      check.equal(
        JSON.stringify(init),
        JSON.stringify({ waitUntil: "domcontentloaded" }),
      );
      const calls = [];
      await routeHandler({
        request: () => ({
          url: () => (options.external ? "https://PRIVATE.invalid" : url),
        }),
        continue: async () => calls.push("continue"),
        abort: async () => calls.push("abort"),
      });
      check.deepEqual(calls, [options.external ? "abort" : "continue"]);
      this.navigations++;
      this.emit("framenavigated", this.frame);
      if (options.pageError) this.emit("pageerror", Error(privateText));
    }
    locator(selector) {
      const page = this;
      check.ok(
        [
          "#name-input",
          "#room-input",
          "#join-btn",
          "#room-screen",
          "#chat-input",
          "#chat-messages",
          "#leave-btn",
          "#join-screen",
        ].includes(selector),
      );
      return {
        async fill(value) {
          page.values[selector] = value;
        },
        async click() {
          if (selector === "#join-btn") {
            check.equal(
              page.values["#name-input"],
              `Restart guest ${page.index + 1}`,
            );
            check.match(
              page.values["#room-input"],
              /^restart-browser-[0-9a-f-]{36}$/,
            );
            if (pages.length === 2)
              check.equal(
                page.values["#room-input"],
                pages[0].values["#room-input"],
              );
            const socket = newSocket(page);
            socket.send({ type: "joinRoom" });
            if (!options.noInitialJoin) socket.receive({ type: "roomJoined" });
            if (options.malformed)
              socket.emit("framereceived", { payload: privateText });
            trace.order.push(`join-${page.index + 1}`);
          } else {
            check.equal(selector, "#leave-btn");
            trace.order.push(`leave-${page.index + 1}`);
          }
        },
        async waitFor(value) {
          check.ok(["#room-screen", "#join-screen"].includes(selector));
          check.equal(
            JSON.stringify(value),
            JSON.stringify({ state: "visible" }),
          );
        },
        async press(value) {
          check.equal(selector, "#chat-input");
          check.equal(value, "Enter");
          const text = page.values[selector];
          check.match(
            text,
            /^Local restart smoke (before|after) [0-9a-f-]{36}$/,
          );
          const before = text.includes(" before ");
          trace.order.push(before ? "chat-before" : "chat-after");
          if (!(before ? options.dropBefore : options.dropAfter)) {
            for (const peer of pages) peer.messages.push(text);
          }
        },
        async inputValue() {
          return page.values[selector];
        },
        getByText(text, value) {
          check.equal(selector, "#chat-messages");
          check.equal(JSON.stringify(value), JSON.stringify({ exact: true }));
          return {
            async waitFor() {
              if (!page.messages.includes(text)) throw Error(privateText);
            },
          };
        },
      };
    }
    async evaluate(source) {
      if (options.invalidCapture)
        return runtime.clone({ user: 0, display: 0, PRIVATE: true });
      return runtime.clone(
        vm.runInContext(`(${source.toString()})()`, this.realm),
      );
    }
  }
  const context = {
    setDefaultTimeout(value) {
      check.equal(value, 10_000);
    },
    async route(pattern, handler) {
      check.equal(pattern, "**/*");
      trace.routes++;
      routeHandler = handler;
    },
    async addInitScript(script) {
      trace.scripts++;
      scripts.push(script);
    },
    async newPage() {
      check.equal(scripts.length, 1);
      const page = new Page(pages.length);
      pages.push(page);
      trace.order.push(`page-${pages.length}`);
      return page;
    },
  };
  const playwright = Object.fromEntries(
    ["chromium", "firefox", "webkit"].map((engine) => [
      engine,
      {
        async launchServer(init) {
          trace.launches++;
          trace.order.push("browser-launch");
          check.equal(trace.spawn, 1);
          check.ok(trace.order.includes("ready-1"));
          check.equal(
            JSON.stringify(init),
            JSON.stringify({ headless: true, timeout: 15_000 }),
          );
          if (options.cancelLaunch) {
            cancel();
            await Promise.resolve();
          }
          if (options.launchFailure) throw Error(privateText);
          return browserServer;
        },
        async connect(endpoint, init) {
          trace.connections++;
          trace.order.push("browser-connect");
          check.equal(endpoint, "owned-browser-endpoint");
          check.equal(JSON.stringify(init), JSON.stringify({ timeout: 5000 }));
          if (options.connectFailure) throw Error(privateText);
          return {
            version: () => "inert-browser-1",
            async newContext(init) {
              trace.contexts++;
              check.equal(
                JSON.stringify(init),
                JSON.stringify({
                  permissions: [],
                  viewport: { width: 1280, height: 900 },
                }),
              );
              return context;
            },
          };
        },
      },
    ]),
  );
  const dependencies = {
    async validateBinary(value) {
      trace.validate++;
      check.equal(value, binary);
      if (options.missingBinary) throw Error(privateText);
    },
    async reservePorts() {
      trace.reserve++;
      return options.ports ?? ports;
    },
    spawn(file, args, init) {
      trace.spawn++;
      const ordinal = trace.spawn;
      trace.order.push(`spawn-${ordinal}`);
      check.equal(file, binary);
      check.equal(JSON.stringify(args), "[]");
      check.equal(init.cwd, `${reference}/`);
      check.equal(init.stdio[0], "ignore");
      check.ok(
        Number.isInteger(init.stdio[1]) && init.stdio[1] === init.stdio[2],
      );
      const expected = {
        PATH: "/owned/inert/path",
        BIND_ADDR: "127.0.0.1",
        PORT: "45101",
        ANNOUNCE_IP: "127.0.0.1",
        MEDIA_WORKERS: "1",
        WEBRTC_SERVER_PORT_BASE: "45102",
        ALLOWED_ORIGINS: "http://127.0.0.1:45101",
        ALLOW_AD_HOC_ROOMS: "true",
        REGISTRATION_ENABLED: "false",
        RUN_MIGRATIONS: "false",
        RUST_LOG: "simplestchat=info,mediasoup=warn",
      };
      check.deepEqual(JSON.parse(JSON.stringify(init.env)), expected);
      trace.environment = { ...expected, PATH: "<captured-path>" };
      const child = new EventEmitter();
      child.pid = 43000 + ordinal;
      child.fixtureSignals = [];
      let exited = false;
      const exit = (code, signal = null) => {
        if (exited) return;
        exited = true;
        trace.order.push(`exit-${ordinal}`);
        child.emit("exit", code, signal);
      };
      child.kill = (signal) => {
        child.fixtureSignals.push(signal);
        trace.order.push(
          `${signal === "SIGTERM" ? "stop" : "kill"}-${ordinal}`,
        );
        check.ok(signal === "SIGTERM" || signal === "SIGKILL");
        if (ordinal === 1 && signal === "SIGTERM") {
          navigationCounts ??= pages.map((page) => page.navigations);
          if (options.cancelRestart) cancel();
          if (!options.noRestartNotice)
            for (const page of pages)
              page.socket?.receive({ type: "serverRestarting" });
        }
        if (options.killRefused && signal === "SIGTERM") return false;
        if (options.serverTermHang && signal === "SIGTERM") return true;
        queueMicrotask(() =>
          exit(
            signal === "SIGKILL" ? null : options.serverExit42 ? 42 : 0,
            signal === "SIGKILL" ? "SIGKILL" : null,
          ),
        );
        return true;
      };
      children.push(child);
      if (ordinal === 2) {
        check.ok(trace.order.includes("exit-1"));
        for (const page of pages) {
          if (!options.noRejoin)
            newSocket(page).receive({ type: "roomJoined" });
          if (options.navigateOnRestart) {
            page.navigations++;
            page.emit("framenavigated", page.frame);
          }
          if (options.terminalRoom)
            page.socket?.receive({ type: "roomClosed" });
          if (options.lostDraft) page.values["#chat-input"] = "";
          if (options.capture) {
            trace.captureDenials++;
            vm.runInContext(
              "navigator.mediaDevices.getUserMedia().catch(() => {})",
              page.realm,
            );
          }
        }
      }
      queueMicrotask(() => {
        if (
          options.serverStartFailure === true ||
          options.serverStartFailure === ordinal
        ) {
          child.pid = undefined;
          child.emit("error", Error(privateText));
        } else if (options.serverEarlyExit) exit(42);
      });
      return child;
    },
    async fetch(url, init) {
      trace.ready++;
      check.equal(url, "http://127.0.0.1:45101/ready");
      check.equal(init.redirect, "error");
      check.ok(init.signal instanceof AbortSignal);
      const ordinal = children.length;
      if (!trace.order.includes(`ready-${ordinal}`))
        trace.order.push(`ready-${ordinal}`);
      if (options.fetchFailure) throw Error(privateText);
      return {
        status: options.readyStatus ?? 200,
        json: async () => options.readyBody ?? { status: "ready" },
      };
    },
    loadPlaywright: () => playwright,
  };
  return {
    trace,
    dependencies,
    navigation: () => navigationCounts,
    invariant: () => invariantError,
    children,
    browserProcess,
  };
}

async function runCase(options = {}, cli = false) {
  const directory = await mkdtemp(join(tmpdir(), "restart-reference-inert."));
  const output = join(directory, "attempt");
  const runtime = await modules();
  const cancellation = new AbortController();
  const f = fixture(options, runtime, cancellation);
  let report,
    failure,
    status,
    stdout = "",
    stderr = "";
  const engine = options.browser ?? "chromium";
  const invoke = async (parameters) => {
    try {
      report = await runtime.entry.runRestartBrowserSmoke(
        parameters,
        f.dependencies,
      );
      return report;
    } catch (error) {
      failure = error;
      report = error.report;
      throw error;
    }
  };
  try {
    if (options.cancelBefore) cancellation.abort();
    if (cli) {
      // Connect the fixture's own cancellation source to the original CLI's
      // registered signal handler; it still chooses its original status1.
      cancellation.signal.addEventListener(
        "abort",
        () => runtime.signals.emit("SIGTERM"),
        { once: true },
      );
      status = await runtime.entry.runCli({
        args: ["--binary", binary, "--browser", engine, "--output", output],
        stdout: {
          write: (value) => {
            stdout += value;
          },
        },
        stderr: {
          write: (value) => {
            stderr += value;
          },
        },
        run: invoke,
      });
      assert.equal(status, report?.passed ? 0 : 1);
      assert.equal(stdout.startsWith("PASS "), status === 0);
      assert.equal(stderr.length > 0, status !== 0);
    } else {
      try {
        await invoke({
          binary,
          browser: engine,
          output,
          signal: cancellation.signal,
        });
      } catch {
        /* The projection retains only fixed observations, never errors. */
      }
    }
    if (f.invariant()) throw f.invariant();
    assert.equal(
      runtime.timers.size,
      0,
      "Every admitted virtual timer must retire",
    );
    assert.equal(runtime.signals.listenerCount("SIGINT"), 0);
    assert.equal(runtime.signals.listenerCount("SIGTERM"), 0);
    if (report) {
      const saved = JSON.parse(
        await readFile(join(output, "outcome.json"), "utf8"),
      );
      assert.deepEqual(saved, JSON.parse(JSON.stringify(report)));
      assert.equal((await stat(output)).mode & 0o7777, 0o700);
      const names = await readdir(output);
      assert.deepEqual(
        names.sort(),
        [
          "outcome.json",
          ...report.servers.map((_, i) => `server-${i + 1}.log`),
        ].sort(),
      );
      for (const name of names)
        assert.equal((await stat(join(output, name))).mode & 0o7777, 0o600);
      report = saved;
    }
    const observation = {
      browser: engine,
      phase: report?.phase ?? "setup",
      pages: report?.pages ?? [],
      captureCalls: (report?.captureCalls ?? []).map((value) => ({
        user: value.user,
        display: value.display,
      })),
      pageErrors: report?.pageErrors ?? 0,
      blockedExternalRequests: report?.blockedExternalRequests ?? 0,
      browserVersion: report?.browserVersion ?? null,
      rejoinMilliseconds: report?.rejoinMilliseconds === undefined ? null : 0,
      draftPreserved: report?.draftPreserved ?? null,
      messageDelivery: report?.messageDelivery ?? null,
      navigationCountsBeforeRestart: [
        "restart",
        "after_restart",
        "complete",
      ].includes(report?.phase)
        ? f.navigation()
        : null,
    };
    const result = {
      passed: !failure && report?.passed === true,
      observation,
      servers: (report?.servers ?? []).map((server) => ({
        exit: server.exit,
        signals: server.signals,
        spawnFailed: server.spawnFailed,
        processError: server.processError,
      })),
      browserCleanupPassed: report?.browserCleanup?.passed === true,
      trace: f.trace,
    };
    if (cli) result.status = status;
    assert(!JSON.stringify(result).includes("PRIVATE"));
    if (result.passed) {
      assert.equal(f.trace.spawn, 2);
      assert.equal(f.trace.launches, 1);
      assert.equal(f.trace.connections, 1);
      assert.equal(f.trace.contexts, 1);
      assert.equal(f.trace.routes, 1);
      assert.equal(f.trace.scripts, 1);
      assert.equal(f.trace.browserCloses, 1);
      assert.equal(f.trace.browserKills, 0);
      assert.deepEqual(
        result.servers.map((server) => server.signals),
        [["SIGTERM"], ["SIGTERM"]],
      );
      assert.deepEqual(
        result.servers.map((server) => server.exit),
        [
          { code: 0, signal: null },
          { code: 0, signal: null },
        ],
      );
      assert(
        f.trace.order.indexOf("exit-1") < f.trace.order.indexOf("spawn-2"),
      );
      assert(
        f.trace.order.indexOf("exit-2") <
          f.trace.order.indexOf("browser-close"),
      );
    }
    return result;
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

async function cliCases() {
  const rows = [];
  for (const [phase, options] of [
    ["success", {}],
    ["failure", { dropBefore: true }],
    ["cancelled", { cancelLaunch: true }],
  ]) {
    const result = await runCase(options, true);
    rows.push({ phase, status: result.status });
  }
  for (const [phase, args, expected] of [
    ["help", ["--help"], 0],
    ["arguments", ["--binary", "relative"], 2],
  ]) {
    const runtime = await modules();
    const sink = { write() {} };
    const status = await runtime.entry.runCli({
      args,
      stdout: sink,
      stderr: sink,
      run: () => assert.fail("No workload admitted"),
    });
    assert.equal(status, expected);
    rows.push({ phase, status });
  }
  return rows;
}

const inputBytes = readFileSync(0);
assert(inputBytes.length <= 256 * 1024);
const input = JSON.parse(inputBytes.toString("utf8"));
assert(Array.isArray(input.cases) && input.cases.length <= 128);
const results = [];
for (const options of input.cases) results.push(await runCase(options));
process.stdout.write(JSON.stringify({ results, cli: await cliCases() }) + "\n");
