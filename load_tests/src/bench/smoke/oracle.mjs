// Actual source-73 drivers with inert process/HTTP/WebSocket/clock adapters.
// The only real I/O is this driver's owned temporary artifact tree. No fake
// child is an OS process, and no fake HTTP/WebSocket object opens a socket.
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { readFileSync } from "node:fs";
import {
  chmod,
  mkdtemp,
  readFile,
  readdir,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  runShutdownSmoke,
  runCli as shutdownCli,
} from "./reference/build/shutdown-smoke.mjs";
import {
  runDiagnosticsSmoke,
  runCli as diagnosticsCli,
} from "./reference/build/diagnostics-smoke.mjs";
import { readDiagnosticReport } from "./reference/load_tests/diagnostic-report.mjs";

const binary = "/owned literal $(server)/simplestChat";
const generator = "/owned literal $(generator)/load_test";
const ports = { http: 45101, media: 45102 };
const privateText = "PRIVATE_SENTINEL";

function fixture(options, signal) {
  const trace = {
    spawn: 0,
    reserve: 0,
    validate: 0,
    ready: 0,
    sockets: [],
    messages: [],
    kills: [],
    reportReads: 0,
    reportAfterExit: true,
    artifacts: [],
    order: [],
  };
  let clock = 0,
    socket,
    exited = false,
    closeCalls = 0;
  const child = new EventEmitter();
  child.pid = 43210;
  child.stdout = new EventEmitter();
  child.stderr = new EventEmitter();
  function finish(code, termination = null) {
    if (exited) return;
    exited = true;
    trace.order.push("exit");
    child.emit("exit", code, termination);
  }
  child.kill = (termination) => {
    trace.kills.push(termination);
    trace.order.push(termination);
    if (options.killRefused && trace.kills.length === 1) return false;
    if (options.cleanupFails) return true;
    if (termination === "SIGKILL") {
      queueMicrotask(() => finish(null, "SIGKILL"));
      return true;
    }
    if (options.stalled) return true;
    queueMicrotask(() => {
      if (socket && !options.noRestartNotice)
        socket.receive({
          type: options.terminalRoom ? "roomClosed" : "serverRestarting",
          reason: options.wrongReason
            ? "Different reason"
            : "Server shutting down",
        });
      if (socket && !options.noClose)
        socket.serverClose(
          options.closeCode ?? 1001,
          options.cleanClose ?? true,
        );
      finish(
        options.exitCode ?? (options.exitSignal ? null : 0),
        options.exitSignal ?? null,
      );
    });
    return true;
  };
  class Socket extends EventTarget {
    static CLOSED = 3;
    readyState = 0;
    constructor(url, protocols) {
      super();
      socket = this;
      this.ordinal = trace.sockets.length;
      trace.sockets.push({ url, protocols });
      assert.equal(url, "ws://127.0.0.1:45101/ws");
      assert.deepEqual(protocols, ["simplestchat"]);
      queueMicrotask(() => {
        if (options.neverOpen) return;
        if (options.socketError) {
          this.dispatchEvent(new Event("error"));
          return;
        }
        this.readyState = 1;
        trace.order.push(
          this.ordinal ? "joined-socket-open" : "peer-socket-open",
        );
        this.dispatchEvent(new Event("open"));
      });
    }
    send(value) {
      const message = JSON.parse(value);
      assert.deepEqual(Object.keys(message), [
        "type",
        "roomId",
        "participantName",
      ]);
      assert.equal(message.type, "joinRoom");
      assert.equal(message.participantName, "Shutdown smoke");
      assert.match(message.roomId, /^shutdown-smoke-[0-9a-f-]{36}$/);
      trace.messages.push({ ...message, roomId: "shutdown-smoke-<uuid>" });
      trace.order.push("join-send");
      queueMicrotask(() => {
        if (!options.neverJoin)
          this.receive(
            options.admissionError
              ? { type: "error", message: privateText }
              : { type: "roomJoined", participantId: "owned-participant" },
          );
      });
    }
    receive(message) {
      if (message.type === "roomJoined") trace.order.push("joined");
      if (message.type === "serverRestarting")
        trace.order.push("restart-notice");
      this.dispatchEvent(
        new MessageEvent("message", { data: JSON.stringify(message) }),
      );
    }
    serverClose(code, wasClean) {
      this.readyState = Socket.CLOSED;
      const event = new Event("close");
      Object.assign(event, { code, wasClean });
      trace.order.push(this.ordinal ? "drain-close" : "peer-close");
      this.dispatchEvent(event);
    }
    close(code) {
      closeCalls++;
      if (options.closeThrows) throw Error(privateText);
      if (code === 1000) {
        assert.equal(
          this.ordinal,
          0,
          "peer probe is a separate unjoined socket",
        );
        if (!options.noPeerClose)
          queueMicrotask(() =>
            this.serverClose(
              options.peerCloseCode ?? 1000,
              options.peerCloseClean ?? true,
            ),
          );
      } else this.readyState = Socket.CLOSED;
    }
  }
  const dependencies = {
    validateBinary: async (value) => {
      trace.validate++;
      assert.equal(value, binary);
      if (options.missingBinary) throw Error(privateText);
    },
    reservePorts: async () => {
      trace.reserve++;
      return options.ports ?? ports;
    },
    spawn: (file, args, init) => {
      assert.equal(file, binary);
      assert.deepEqual(args, []);
      assert.deepEqual(init.stdio, ["ignore", "pipe", "pipe"]);
      assert.equal(
        init.cwd,
        fileURLToPath(new URL("./reference/", import.meta.url)),
      );
      trace.spawn++;
      trace.order.push("spawn");
      trace.environment = { ...init.env, PATH: "<captured-path>" };
      if (trace.environment.DIAGNOSTICS_PATH)
        trace.environment.DIAGNOSTICS_PATH = "<diagnostics>";
      assert.equal(init.env.DATABASE_URL, undefined);
      assert.equal(init.env.JWT_SECRET, undefined);
      queueMicrotask(() => {
        if (options.spawnError) child.emit("error", Error(privateText));
        if (options.earlyExit) finish(9);
        if (options.output)
          child.stderr.emit("data", Buffer.from(options.output));
      });
      return child;
    },
    fetch: async (url, init) => {
      assert.equal(url, "http://127.0.0.1:45101/ready");
      assert.equal(init.redirect, "error");
      assert(init.signal instanceof AbortSignal);
      trace.ready++;
      if (options.fetchError) throw Error(privateText);
      if (options.readyJsonError)
        return {
          status: 200,
          json: async () => {
            throw Error(privateText);
          },
        };
      return {
        status: options.status ?? 200,
        json: async () => options.readyBody ?? { status: "ready" },
      };
    },
    WebSocket: Socket,
    wait: async () => {
      await Promise.resolve();
      clock += 5;
      if (options.cancelPolling) signal.abort();
    },
    now: () => clock,
  };
  return {
    trace,
    child,
    dependencies,
    exited: () => exited,
    closeCalls: () => closeCalls,
  };
}

function normalize(value, directory) {
  if (!value) return null;
  const result = structuredClone(value);
  if (Object.hasOwn(result, "shutdownMs") && result.shutdownMs !== null)
    result.shutdownMs = 0;
  if (directory) result.artifactDirectory = "<owned-artifacts>";
  return result;
}
function projection(error, value, family, fixture, directory) {
  const result = {
    passed: !error,
    shutdown: family === "shutdown" ? normalize(value) : null,
    diagnostic:
      family === "diagnostics"
        ? normalize(error?.result ?? value, directory)
        : null,
    trace: fixture.trace,
  };
  if (result.shutdown) {
    assert(result.shutdown.shutdownMs === 0);
    assert.deepEqual(result.shutdown.ports, ports);
  }
  return result;
}
const record = (patch) => ({
  schemaVersion: 1,
  kind: "operation",
  operationId: 1,
  connectionId: 1,
  operation: "join_room",
  stage: null,
  outcome: "ok",
  startedUs: 1,
  elapsedUs: 100,
  ...patch,
});
function diagnosticContents(options) {
  const records = options.records ?? [
    record(),
    record({ kind: "stage", stage: "room_lock_wait", outcome: "completed" }),
  ];
  const summary = {
    schemaVersion: 1,
    kind: "summary",
    accepted: records.length,
    written: records.length,
    dropped: 0,
    expired: 0,
    unfinished: 0,
    writeFailed: false,
    ...options.summary,
  };
  return (
    options.raw ??
    [...records, ...(options.noSummary ? [] : [summary])]
      .map((row) => JSON.stringify(row) + "\n")
      .join("")
  );
}

async function runCase(input) {
  assert(["shutdown", "diagnostics"].includes(input.family));
  const options = input.options ?? {};
  const cancellation = new AbortController();
  const f = fixture(options, cancellation);
  let error, value, directory;
  if (options.cancelBefore) cancellation.abort();
  const parameters = {
    binary,
    signal: cancellation.signal,
    startupTimeoutMs: 50,
    shutdownTimeoutMs: 50,
    cleanupTimeoutMs: 20,
  };
  if (input.family === "shutdown") {
    try {
      value = await runShutdownSmoke(parameters, f.dependencies);
    } catch (caught) {
      error = caught;
    }
    if (options.output && error) assert(error.serverOutput.length <= 32768);
  } else {
    directory = await mkdtemp(join(tmpdir(), "paired-smoke-inert."));
    await chmod(directory, 0o700);
    const diagnosticPath = join(directory, "server-diagnostics.jsonl");
    const save = async (path, data) => {
      const name = path.slice(directory.length + 1);
      if (options.failWrite === name) throw Error(privateText);
      await writeFile(path, JSON.stringify(data, null, 2) + "\n", {
        flag: "wx",
        mode: 0o600,
      });
      f.trace.artifacts.push(name);
      if (name === "diagnostics-smoke.json" && options.cancelPublication)
        cancellation.abort();
    };
    const execute = async (file, args, init) => {
      f.trace.reportReads++;
      f.trace.reportAfterExit &&= f.exited();
      assert(
        f.exited(),
        "full recorder is read only after the exact fake server exit",
      );
      assert.equal(file, generator);
      assert.deepEqual(args, ["report", "diagnostics", diagnosticPath]);
      assert.equal(init.timeout, 30000);
      assert.equal(init.maxBuffer, 2 * 1024 * 1024);
      assert.equal(init.killSignal, "SIGKILL");
      assert.equal(init.signal, cancellation.signal);
      assert(
        Object.keys(init.env).every((key) =>
          ["PATH", "TMPDIR", "LANG", "SYSTEMROOT"].includes(key),
        ),
      );
      f.trace.order.push("report-read");
      if (options.reportProcessCode)
        throw Object.assign(Error(privateText), {
          code: options.reportProcessCode,
          killed: !!options.reportKilled,
          signal: options.reportSignal,
        });
      const report = await readDiagnosticReport(diagnosticPath);
      if (options.malformedReport) report.PRIVATE = privateText;
      if (options.cancelReport) cancellation.abort();
      const stdout = options.invalidReportJson
        ? privateText
        : JSON.stringify(report);
      const status = options.reportStatus ?? (report.coverage.complete ? 0 : 1);
      if (status)
        throw Object.assign(Error(privateText), { code: status, stdout });
      return { stdout };
    };
    try {
      value = await runDiagnosticsSmoke(
        { binary, generator, signal: cancellation.signal },
        {
          createDirectory: async () => directory,
          spawn: (...args) => f.dependencies.spawn(...args),
          run: async (settings, { spawn }) => {
            if (!options.missing)
              await writeFile(diagnosticPath, diagnosticContents(options), {
                flag: "wx",
                mode: 0o600,
              });
            return runShutdownSmoke(
              { ...parameters, ...settings },
              { ...f.dependencies, spawn },
            );
          },
          executeReport: execute,
          save,
          saveLog: async (path, data, init) => {
            if (options.failLog) throw Error(privateText);
            assert(data.length <= 32768);
            await writeFile(path, data, init);
            f.trace.artifacts.push("server.log");
          },
        },
      );
    } catch (caught) {
      error = caught;
    }
  }
  const result = projection(error, value, input.family, f, directory);
  if (directory) {
    const names = await readdir(directory);
    for (const name of names) {
      assert.equal((await stat(join(directory, name))).mode & 0o777, 0o600);
    }
    if (names.includes("diagnostics-smoke.json")) {
      const saved = JSON.parse(
        await readFile(join(directory, "diagnostics-smoke.json"), "utf8"),
      );
      result.savedPassed = saved.passed;
      assert.equal(saved.passed, result.diagnostic.passed);
    }
    result.artifactNames = names.sort();
    await rm(directory, { recursive: true });
  }
  if (result.passed) {
    assert.equal(f.trace.spawn, 1);
    assert.equal(f.trace.sockets.length, 2);
    assert.equal(f.trace.messages.length, 1);
    assert.deepEqual(f.trace.kills, ["SIGTERM"]);
    assert(
      f.trace.order.indexOf("peer-close") < f.trace.order.indexOf("join-send"),
    );
    assert(f.trace.order.indexOf("joined") < f.trace.order.indexOf("SIGTERM"));
    if (input.family === "diagnostics") {
      assert(
        f.trace.order.indexOf("exit") < f.trace.order.indexOf("report-read"),
      );
      assert.equal(result.savedPassed, true);
    }
  }
  // Exit/cleanup failure diagnostics stay inside this fixture, never in rows.
  assert(!JSON.stringify(result).includes(privateText));
  return result;
}

async function cliCases() {
  const results = [];
  for (const [kind, cli, args] of [
    ["shutdown", shutdownCli, ["--binary", binary]],
    [
      "diagnostics",
      diagnosticsCli,
      ["--binary", binary, "--generator", generator],
    ],
  ]) {
    for (const phase of [
      "help",
      "arguments",
      "success",
      "failure",
      "late-cancellation",
    ]) {
      let output = "";
      const stream = {
        write: (value) => {
          output += value;
        },
      };
      const before = ["SIGINT", "SIGTERM"].map((name) =>
        process.listenerCount(name),
      );
      const status = await cli({
        args:
          phase === "help"
            ? ["--help"]
            : phase === "arguments"
              ? ["--binary", "relative"]
              : args,
        stdout: stream,
        stderr: stream,
        run: async () => {
          if (phase === "failure") throw Error("fixed fixture failure");
          if (phase === "late-cancellation") process.emit("SIGTERM");
          return {
            passed: true,
            ready: true,
            records: 2,
            artifactDirectory: "<owned-artifacts>",
          };
        },
      });
      assert.deepEqual(
        ["SIGINT", "SIGTERM"].map((name) => process.listenerCount(name)),
        before,
      );
      results.push({ kind, phase, status });
    }
  }
  return results;
}

const input = JSON.parse(readFileSync(0, "utf8"));
assert(Array.isArray(input.cases) && input.cases.length <= 128);
const results = [];
for (const row of input.cases) results.push(await runCase(row));
process.stdout.write(JSON.stringify({ results, cli: await cliCases() }) + "\n");
