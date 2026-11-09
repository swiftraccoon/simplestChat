// Whole, unchanged 80 source and tests. All network/time boundaries are inert.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import vm from "node:vm";

const root = dirname(fileURLToPath(import.meta.url));
const pins = [
  [
    "public-smoke.mjs",
    "1c48cfe99296f65ad8855b4bf5c2fcd945c943259953e0d137edd2276b6f47bf",
  ],
  [
    "public-smoke.test.mjs",
    "16645211b9685d4c94829d70ab6c7ffc32db1f9427d7f1b73c11ebe6805d0187",
  ],
];
const sources = new Map(
  pins.map(([name, digest]) => {
    const bytes = readFileSync(join(root, "reference", name));
    assert.equal(createHash("sha256").update(bytes).digest("hex"), digest);
    return [name, bytes.toString("utf8")];
  }),
);
const ORIGIN = "https://chat.example.com";
const UUID = "00000000-0000-4000-8000-000000000001";
const HTML =
  '<script type="module" src="/assets/index-A1.js"></script>' +
  '<link rel="stylesheet" href="/assets/index-B2.css">';
const SECURITY = {
  "strict-transport-security": "max-age=31536000",
  "x-content-type-options": "nosniff",
  "x-frame-options": "DENY",
  "referrer-policy": "no-referrer",
  "cross-origin-opener-policy": "same-origin",
  "cross-origin-resource-policy": "same-origin",
  "permissions-policy":
    "camera=(self), microphone=(self), display-capture=(self), geolocation=()",
  "content-security-policy":
    "default-src 'self'; script-src 'self'; connect-src 'self'; " +
    "form-action 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'",
};

async function runtime(options = {}, cli = null) {
  let clock = 0;
  let timerId = 0;
  let invariant;
  const timers = new Map();
  const trace = {
    requests: [],
    connects: [],
    sent: [],
    closes: [],
    order: [],
    cancelledBodies: 0,
  };
  const sockets = [];
  const stdout = [];
  const stderr = [];
  const exits = [];
  const pureTests = [];
  const check = (condition, message) => {
    if (!condition) {
      invariant ??= Error(message);
      throw invariant;
    }
  };
  const setTimer = (callback, milliseconds) => {
    check(
      Number.isFinite(milliseconds) && milliseconds >= 0,
      "finite timer only",
    );
    const id = ++timerId;
    timers.set(id, { callback, at: clock + milliseconds });
    return id;
  };
  const virtualSignal = {
    any(signals) {
      const controller = new AbortController();
      for (const signal of signals) {
        if (signal.aborted) controller.abort();
        else
          signal.addEventListener("abort", () => controller.abort(), {
            once: true,
          });
      }
      return controller.signal;
    },
    timeout(milliseconds) {
      const controller = new AbortController();
      setTimer(() => controller.abort(), milliseconds);
      return controller.signal;
    },
  };
  const emit = (socket, type, event = {}) => {
    queueMicrotask(() => {
      for (const listener of socket.listeners.get(type) ?? []) {
        listener.callback(event);
        if (listener.once) socket.listeners.get(type).delete(listener);
      }
    });
  };
  const message = (socket, value) =>
    emit(socket, "message", {
      data: typeof value === "string" ? value : JSON.stringify(value),
    });
  class FakeWebSocket {
    static OPEN = 1;
    constructor(url) {
      check(
        url === `${ORIGIN.replace("https:", "wss:")}/ws`,
        "fixed WSS endpoint",
      );
      this.index = sockets.length;
      check(this.index < 2, "at most two owned guests");
      this.readyState = FakeWebSocket.OPEN;
      this.listeners = new Map();
      sockets.push(this);
      trace.connects.push(url);
      trace.order.push(`connect:${this.index}`);
      if (options.socketFailure && this.index === 0) emit(this, "error");
      else if (options.socketClosed && this.index === 0) {
        this.readyState = 3;
        emit(this, "close", { code: 1006, wasClean: false });
      } else emit(this, "open");
    }
    addEventListener(type, callback, options = {}) {
      const list = this.listeners.get(type) ?? new Set();
      list.add({ callback, once: options.once === true });
      this.listeners.set(type, list);
    }
    send(raw) {
      check(
        this.readyState === 1 && typeof raw === "string",
        "only open text send",
      );
      const value = JSON.parse(raw);
      const keys = Object.keys(value).sort();
      if (value.type === "joinRoom") {
        check(
          JSON.stringify(keys) === '["participantName","roomId","type"]',
          "join keys",
        );
        check(value.roomId === "lobby", "fixed room");
        check(
          value.participantName ===
            `Smoke ${UUID.slice(0, 8)} ${this.index + 1}`,
          "guest name",
        );
        trace.sent.push({
          guest: this.index,
          type: value.type,
          roomId: value.roomId,
        });
        trace.order.push(`send:${this.index}:joinRoom`);
        if (options.joinMissing) return;
        if (options.binaryFrame)
          return emit(this, "message", { data: new Uint8Array([1]) });
        if (options.malformedFrame)
          return message(this, "PRIVATE_INVALID_JSON");
        if (options.missingType) return message(this, {});
        if (options.largeFrame)
          return message(this, "x".repeat(256 * 1024 + 1));
        if (options.roomRejected)
          return message(this, { type: options.roomRejected });
        if (options.rawFrame !== undefined) message(this, options.rawFrame);
        if (options.extraFrames) {
          check(
            Number.isSafeInteger(options.extraFrames) &&
              options.extraFrames <= 128,
            "bounded ignored frame fixture",
          );
          const frame = { type: "unrelated", content: "x".repeat(65536) };
          for (let n = 0; n < options.extraFrames; n++) message(this, frame);
        }
        const participantId = options.emptyParticipant
          ? ""
          : `guest-${options.duplicateParticipant ? 0 : this.index}`;
        if (options.duplicateJoin)
          return message(
            this,
            `{"type":"PRIVATE","type":"roomJoined",` +
              `"participantId":"PRIVATE","participantId":"${participantId}","PRIVATE":1e300}`,
          );
        message(this, { type: "roomJoined", participantId });
      } else if (value.type === "chatMessage") {
        check(this.index === 0, "only first guest sends message");
        check(
          JSON.stringify(keys) === '["clientMessageId","content","type"]',
          "chat keys",
        );
        check(value.clientMessageId === `smoke-${UUID}`, "exact correlation");
        check(
          value.content ===
            `[SimplestChat public smoke ${UUID}] HTTPS and text delivery check.`,
          "exact one public message",
        );
        trace.sent.push({ guest: this.index, type: value.type });
        trace.order.push(`send:${this.index}:chatMessage`);
        if (options.sendFailure) throw Error("PRIVATE_SEND_FAILURE");
        const entry = {
          clientMessageId: value.clientMessageId,
          content: value.content,
          participantId: "guest-0",
          messageId: "message-1",
        };
        const wrong = options.chatMutation ?? {};
        const acknowledge = () => {
          if (options.ackMissing) return;
          message(this, {
            type: "messageAck",
            clientMessageId: value.clientMessageId,
            message: {
              ...entry,
              ...options.ackPatch,
              ...(options.ackMismatch
                ? { ...wrong, participantId: "foreign" }
                : {}),
            },
            ...options.ackOuterPatch,
          });
          if (options.duplicateAck)
            message(this, {
              type: "messageAck",
              clientMessageId: value.clientMessageId,
              message: { ...entry, messageId: "message-2" },
            });
        };
        if (!options.duplicateDelivery) acknowledge();
        if (!options.deliveryMissing)
          message(sockets[1], {
            type: "chatReceived",
            ...entry,
            ...(options.deliveryMismatch
              ? { ...wrong, participantId: "foreign" }
              : {}),
            ...(options.chatIdentityMismatch ? { messageId: "message-2" } : {}),
            ...options.deliveryPatch,
          });
        if (options.duplicateDelivery) {
          message(sockets[1], {
            type: "chatReceived",
            ...entry,
            messageId: "message-2",
          });
          acknowledge();
        }
      } else if (value.type === "leaveRoom") {
        check(JSON.stringify(keys) === '["type"]', "leave keys");
        trace.sent.push({ guest: this.index, type: value.type });
        trace.order.push(`send:${this.index}:leaveRoom`);
        if (options.leaveFailure) throw Error("PRIVATE_LEAVE_FAILURE");
        if (this.index === 0 && sockets[1] && !options.leaveMissing) {
          message(sockets[1], {
            type: "participantLeft",
            participantId: options.leaveWrong ? "foreign" : "guest-0",
          });
        }
      } else check(false, "unexpected application mutation");
    }
    close(code, reason) {
      check(
        code === 1000 && reason === "Public smoke complete",
        "fixed close intent",
      );
      trace.closes.push({ guest: this.index, code, reason });
      trace.order.push(`close:${this.index}`);
      if (options.closeThrow) throw Error("PRIVATE_CLOSE_FAILURE");
      if (options.closeMissing) return;
      this.readyState = 3;
      if (options.lateClose) clock += 5000;
      const close = () =>
        emit(this, "close", {
          code: options.closeUnclean ? 1006 : 1000,
          wasClean: !options.closeUnclean,
        });
      if (options.closeAtDeadline) setTimer(close, 5000);
      else close();
    }
  }
  const fetch = async (url, init) => {
    check(typeof url === "string", "string fetch URL");
    const parsed = new URL(url);
    check(
      parsed.origin === ORIGIN && !parsed.search && !parsed.hash,
      "fixed request URL",
    );
    check(
      init.redirect === "error" && init.credentials === "omit",
      "no redirects/credentials",
    );
    const path = parsed.pathname;
    const expectedAccept = ["/health", "/ready"].includes(path)
      ? "application/json"
      : path === "/"
        ? "text/html"
        : "*/*";
    check(
      init.headers.Accept === expectedAccept &&
        Object.keys(init.headers).length === 1,
      "fixed Accept only",
    );
    check(init.signal instanceof AbortSignal, "owned abort signal");
    trace.requests.push({ path, accept: init.headers.Accept });
    trace.order.push(`request:${path}`);
    const targeted = path === (options.httpPath ?? "/health");
    if (targeted && options.fetchFailure) throw Error("PRIVATE_FETCH_FAILURE");
    if (targeted && options.fetchHang) return new Promise(() => {});
    if (targeted && options.fetchDeadline)
      return new Promise((_, reject) => {
        init.signal.addEventListener(
          "abort",
          () => reject(Error("PRIVATE_ABORT")),
          { once: true },
        );
      });
    const headers = new Headers(path === "/" ? SECURITY : {});
    let status =
      path.startsWith("/diagnostics") || path === "/metrics" ? 404 : 200;
    let type;
    let text;
    if (path === "/health" || path === "/ready") {
      type = "application/json";
      text = JSON.stringify({ status: path === "/health" ? "ok" : "ready" });
    } else if (path === "/") {
      type = "text/html";
      text = options.html ?? HTML;
    } else if (/^\/assets\/[A-Za-z0-9_.-]+\.js$/.test(path)) {
      type = "text/javascript";
      text = "export {};";
    } else if (/^\/assets\/[A-Za-z0-9_.-]+\.css$/.test(path)) {
      type = "text/css";
      text = "body{}";
    } else if (status === 404) {
      type = "text/plain";
      text = "not found";
    } else check(false, "unexpected fetch path");
    if (targeted) {
      status = options.httpStatus ?? status;
      type = options.httpType ?? type;
      text = options.body ?? text;
      if (options.bodyBytes !== undefined) {
        check(
          Number.isSafeInteger(options.bodyBytes) &&
            options.bodyBytes <= 2 * 1024 * 1024 + 1,
          "bounded body fixture",
        );
        text =
          options.padBody && options.bodyBytes >= Buffer.byteLength(text)
            ? text + " ".repeat(options.bodyBytes - Buffer.byteLength(text))
            : "x".repeat(options.bodyBytes);
      }
    }
    headers.set("content-type", type);
    if (options.missingHeader && path === "/")
      headers.delete(options.missingHeader);
    if (options.header && path === "/") headers.set(...options.header);
    if (options.identityEncoding && targeted)
      headers.set("content-encoding", "gzip");
    if (options.lateResponse && targeted) clock += 35_000;
    let bytes = Buffer.from(text);
    if (targeted && options.invalidUtf8) {
      bytes = Buffer.concat([
        Buffer.from(
          options.invalidUtf8 === "status"
            ? '{"status":"'
            : '{"status":"ok","PRIVATE":"',
        ),
        Buffer.from([255]),
        Buffer.from('"}'),
      ]);
    }
    let offset = 0;
    const reader = {
      async read() {
        if (offset === bytes.length) return { done: true };
        const end = Math.min(offset + 1024, bytes.length);
        const value = bytes.subarray(offset, end);
        offset = end;
        return { done: false, value };
      },
      async cancel() {
        trace.cancelledBodies++;
      },
    };
    return {
      status,
      headers,
      body:
        targeted && options.missingBody ? null : { getReader: () => reader },
    };
  };
  class VirtualDate extends Date {
    constructor(...args) {
      super(...(args.length ? args : ["2026-10-22T00:00:00Z"]));
    }
    static now() {
      return 1_792_627_200_000 + clock;
    }
  }
  const process = {
    argv: cli
      ? ["node", join(root, "reference/public-smoke.mjs"), ...cli]
      : ["node"],
    env: options.tlsDisabled ? { NODE_TLS_REJECT_UNAUTHORIZED: "0" } : {},
    stdout: {
      write(value, callback) {
        stdout.push(String(value));
        callback?.();
      },
    },
    stderr: {
      write(value) {
        stderr.push(String(value));
      },
    },
    exit(code) {
      exits.push(code);
    },
    exitCode: undefined,
  };
  const context = vm.createContext({
    Buffer,
    Headers,
    URL,
    AbortController,
    AbortSignal: virtualSignal,
    WebSocket: options.unsupportedNode ? undefined : FakeWebSocket,
    fetch: options.unsupportedNode ? undefined : fetch,
    Date: VirtualDate,
    performance: { now: () => clock },
    process,
    setTimeout: setTimer,
    clearTimeout: (id) => timers.delete(id),
    console: { log: () => check(false, "unexpected console output") },
  });
  const module = new vm.SourceTextModule(sources.get("public-smoke.mjs"), {
    context,
    identifier: join(root, "reference/public-smoke.mjs"),
    initializeImportMeta(meta) {
      meta.url = pathToFileURL(join(root, "reference/public-smoke.mjs")).href;
    },
  });
  const synthetic = (values) =>
    new vm.SyntheticModule(
      Object.keys(values),
      function () {
        for (const [key, value] of Object.entries(values))
          this.setExport(key, value);
      },
      { context },
    );
  await module.link((name) => {
    if (name === "node:crypto") return synthetic({ randomUUID: () => UUID });
    if (name === "node:path") return synthetic({ resolve });
    if (name === "node:url") return synthetic({ pathToFileURL });
    throw Error("Unapproved original module import");
  });
  const drive = async (operation) => {
    let settled = false;
    let result;
    let error;
    Promise.resolve(operation).then(
      (value) => {
        result = value;
        settled = true;
      },
      (reason) => {
        error = reason;
        settled = true;
      },
    );
    for (let steps = 0; !settled && steps < 1000; steps++) {
      // Drain all current Promise work before advancing an inert deadline.
      await new Promise(setImmediate);
      if (exits.length) {
        settled = true;
        break;
      }
      if (settled) break;
      const next = [...timers].sort(
        (a, b) => a[1].at - b[1].at || a[0] - b[0],
      )[0];
      check(next !== undefined, "pending reference has a bounded timer");
      timers.delete(next[0]);
      clock = Math.max(clock, next[1].at);
      next[1].callback();
    }
    assert(settled, "bounded virtual operation must settle");
    if (invariant) throw invariant;
    if (error) throw error;
    return result;
  };
  await drive(module.evaluate());
  return {
    module,
    trace,
    timers,
    context,
    process,
    stdout,
    stderr,
    exits,
    drive,
    pureTests,
    testModule: async () => {
      const tests = new vm.SourceTextModule(
        sources.get("public-smoke.test.mjs"),
        { context },
      );
      await tests.link((name) => {
        if (name === "../public-smoke.mjs") return module;
        if (name === "node:assert/strict")
          return synthetic({ default: assert });
        if (name === "node:test")
          return synthetic({
            default: (name, test) => {
              pureTests.push([name, test]);
            },
          });
        throw Error("Unapproved reference test import");
      });
      await tests.evaluate();
      for (const [, test] of pureTests) await test();
      assert.equal(pureTests.length, 7);
    },
  };
}

function normalized(summary) {
  return {
    ...JSON.parse(JSON.stringify(summary)),
    runId: "RUN",
    startedAt: "TIME",
    durationMs: 0,
  };
}
function checkedSummary(summary, trace, options) {
  const report = normalized(summary);
  assert(
    !JSON.stringify(report).includes("PRIVATE"),
    "private inputs never appear in summary",
  );
  assert.deepEqual(Object.keys(report.checks), [
    "health",
    "ready",
    "homepage",
    "securityHeaders",
    "staticAssets",
    "privateEndpoints",
    "guestJoins",
    "textAcknowledgedAndDelivered",
    "firstLeaveObserved",
    "webSocketCleanup",
  ]);
  assert(trace.connects.length <= 2);
  assert(
    trace.sent.filter((value) => value.type === "chatMessage").length <= 1,
  );
  assert(trace.closes.length <= 2);
  if (report.passed && options.html === undefined) {
    assert.deepEqual(trace.order, [
      "request:/health",
      "request:/ready",
      "request:/",
      "request:/assets/index-A1.js",
      "request:/assets/index-B2.css",
      "request:/metrics",
      "request:/diagnostics",
      "request:/diagnostics/media",
      "connect:0",
      "send:0:joinRoom",
      "connect:1",
      "send:1:joinRoom",
      "send:0:chatMessage",
      "send:0:leaveRoom",
      "send:1:leaveRoom",
      "close:0",
      "close:1",
    ]);
    assert.equal(trace.cancelledBodies, 8);
  }
  return report;
}
async function one(options) {
  const owned = await runtime(options);
  try {
    const summary = await owned.drive(
      owned.module.namespace.runSmoke({ origin: ORIGIN, room: "lobby" }),
    );
    const report = checkedSummary(summary, owned.trace, options);
    return {
      status: report.passed ? 0 : 1,
      summary: report,
      trace: owned.trace,
    };
  } finally {
    owned.timers.clear();
  }
}
async function cliCase(args, options = {}) {
  const owned = await runtime(options, args);
  try {
    const text = owned.stdout.join("");
    const summary = text.startsWith("{")
      ? checkedSummary(JSON.parse(text), owned.trace, options)
      : null;
    return {
      status: owned.exits.at(-1) ?? owned.process.exitCode ?? 0,
      summary,
      help: text.startsWith("Usage:"),
      error: owned.stderr.join("").trim(),
      requests: owned.trace.requests.length,
      sockets: owned.trace.connects.length,
    };
  } finally {
    owned.timers.clear();
  }
}

const input = [];
let bytes = 0;
for await (const part of process.stdin) {
  bytes += part.length;
  assert(bytes <= 64 * 1024, "bounded oracle input");
  input.push(part);
}
const request = JSON.parse(Buffer.concat(input).toString("utf8") || "{}");
assert(Array.isArray(request.cases ?? []));
assert((request.cases ?? []).length <= 128);
const originalTests = await runtime();
await originalTests.testModule();
originalTests.timers.clear();
const results = [];
for (const options of request.cases ?? []) results.push(await one(options));
const cli = [];
if (request.cli === true) {
  for (const args of [
    ["--help"],
    [],
    ["--origin", "http://PRIVATE.invalid"],
    ["--origin", ORIGIN],
    ["--origin", ORIGIN, "--room", "PRIVATE/path"],
  ]) {
    cli.push(await cliCase(args));
  }
  cli.push(await cliCase(["--origin", ORIGIN], { httpStatus: 503 }));
  cli.push(await cliCase(["--origin", ORIGIN], { fetchHang: true }));
}
process.stdout.write(
  `${JSON.stringify({ unchangedReferenceTests: 7, results, cli })}\n`,
);
