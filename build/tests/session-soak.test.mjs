import assert from "node:assert/strict";
import { test } from "node:test";
import vm from "node:vm";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const {
  configuration,
  sessionExpiry,
  continuityFailure,
  expiryCoverage,
  installSessionObservation,
} = require("../../web/e2e/session-soak-checks.cjs");
const { within } = require("../../web/e2e/session-soak.cjs");

const environment = {
  SESSION_SOAK_E2E: "1",
  DISPOSABLE_TEST_DATABASE: "1",
  TEST_SERVER_PID: "123",
  TEST_METRICS_TOKEN: "a".repeat(64),
};

test("session soak fixes full wall-clock workload and labels smoke separately", () => {
  const full = configuration(environment);
  assert.equal(full.durationMs, 1200000);
  assert.equal(full.messageIntervalMs, 3000);
  assert.equal(full.clients, 2);
  assert.equal(full.recoveryMs, 60000);
  assert.equal(full.profile, "full");
  assert.equal(
    configuration({ ...environment, SESSION_SOAK_PROFILE: "smoke" }).durationMs,
    30000,
  );
});

test("session soak refuses public, ambiguous, credential-bearing, or unowned origins", () => {
  for (const BASE_URL of [
    "https://research.clinic",
    "http://127.0.0.2",
    "http://127.0.0.1.example.test",
    "http://user:pass@127.0.0.1",
    "http://127.0.0.1/other",
    "http://127.0.0.1/?token=x",
    "http://127.0.0.1/#fragment",
    "ws://127.0.0.1",
  ])
    assert.throws(() => configuration({ ...environment, BASE_URL }));
  for (const field of [
    "SESSION_SOAK_E2E",
    "DISPOSABLE_TEST_DATABASE",
    "TEST_SERVER_PID",
    "TEST_METRICS_TOKEN",
  ])
    assert.throws(() => configuration({ ...environment, [field]: "" }));
  assert.throws(() =>
    configuration({ ...environment, NODE_TLS_REJECT_UNAUTHORIZED: "0" }),
  );
  assert.throws(() =>
    configuration({ ...environment, SESSION_SOAK_PROFILE: "fast-full" }),
  );
  for (const TEST_SERVER_PID of ["1", "-5", "0", "2.5", "9007199254740992"])
    assert.throws(() => configuration({ ...environment, TEST_SERVER_PID }));
});

test("session soak accepts only exact HTTP(S) loopback origins", () => {
  for (const BASE_URL of [
    "http://127.0.0.1:3119",
    "http://localhost:3119",
    "https://[::1]:3119",
  ])
    assert.equal(configuration({ ...environment, BASE_URL }).base, BASE_URL);
});

const ownToken = (payload) =>
  `header.${Buffer.from(JSON.stringify(payload)).toString("base64url")}.signature`;
test("only original expiry timestamp survives successful own auth parsing", () => {
  const now = 1800000000000;
  const expires = now + 900000;
  assert.equal(
    sessionExpiry(
      {
        token: ownToken({ exp: expires / 1000, sub: "private-account" }),
        user: { email: "private@example.test" },
      },
      now,
    ),
    expires,
  );
  for (const exp of [
    undefined,
    "1800000900",
    NaN,
    0,
    -1,
    (now - 1000) / 1000,
    (now + 1800000) / 1000,
    Number.MAX_SAFE_INTEGER,
  ])
    assert.throws(() => sessionExpiry({ token: ownToken({ exp }) }, now));
  for (const token of ["", "x.y", "x.!.z", "x".repeat(16385)])
    assert.throws(() => sessionExpiry({ token }, now));
});

test("a recovered socket/full rejoin is never a seamless continuity pass", () => {
  assert.equal(
    continuityFailure({ navigationCount: 1, initialNavigationCount: 1 }),
    null,
  );
  for (const [field, code] of Object.entries({
    socketCloses: "authenticated_socket_closed",
    freshJoins: "fresh_room_join",
    resumeRejected: "resume_rejected",
    protocolErrors: "signaling_request_rejected",
    disabledTransitions: "chat_input_disabled",
    authenticationRenewalFailed: "authentication_renewal_failed",
    captureRequests: "unexpected_capture_request",
  }))
    assert.equal(
      continuityFailure({
        navigationCount: 1,
        initialNavigationCount: 1,
        [field]: 1,
        resumeAccepted: 1,
      }),
      code,
    );
  assert.equal(
    continuityFailure({ navigationCount: 2, initialNavigationCount: 1 }),
    "page_navigated",
  );
  assert.equal(
    continuityFailure({
      navigationCount: 1,
      initialNavigationCount: 1,
      failure: "first_failure",
      socketCloses: 1,
    }),
    "first_failure",
  );
});

test("initial room hash navigation is the armed baseline, not a continuity failure", () => {
  assert.equal(
    continuityFailure({ navigationCount: 2, initialNavigationCount: 2 }),
    null,
  );
  assert.equal(
    continuityFailure({ navigationCount: 3, initialNavigationCount: 2 }),
    "page_navigated",
  );
  for (const initialNavigationCount of [undefined, null, 0, -1, 1.5, "2"])
    assert.equal(
      continuityFailure({ navigationCount: 2, initialNavigationCount }),
      "page_navigated",
    );
});

test("full coverage requires both natural refreshes and time beyond both original expiries", () => {
  const client = {
    registeredAtMs: 1000,
    originalExpiresAtMs: 901000,
    refreshFailures: 0,
    refreshes: [{ atMs: 721000, expiresAtMs: 1621000 }],
  };
  assert.equal(expiryCoverage([client, client], 1000000), true);
  assert.equal(expiryCoverage([client, client], 950000), false);
  assert.equal(expiryCoverage([client], 1000000), false);
  assert.equal(
    expiryCoverage([client, { ...client, refreshFailures: 1 }], 1000000),
    false,
  );
  for (const refresh of [
    { atMs: 721000, expiresAtMs: 901000 },
    { atMs: 901001, expiresAtMs: 1621000 },
    { atMs: 2000, expiresAtMs: 1621000 },
  ])
    assert.equal(
      expiryCoverage([client, { ...client, refreshes: [refresh] }], 1000000),
      false,
    );
  assert.equal(
    expiryCoverage([client, { ...client, refreshes: [] }], 1000000),
    false,
  );
});

function fixture() {
  const sent = [];
  const input = { disabled: false };
  class Socket extends EventTarget {
    static OPEN = 1;
    constructor(url) {
      super();
      this.url = url;
      this.readyState = 1;
    }
    send(value) {
      sent.push(value);
    }
  }
  const realm = {
    window: { WebSocket: Socket },
    location: { origin: "http://127.0.0.1:3119" },
    navigator: {
      mediaDevices: {
        getUserMedia() {
          throw new Error("Must never capture");
        },
      },
    },
    document: { documentElement: {}, querySelector: () => input },
    MutationObserver: class {
      observe() {}
    },
    URL,
    WeakRef,
    performance,
    DOMException,
    Date,
  };
  vm.runInNewContext(`(${installSessionObservation.toString()})()`, realm);
  const socket = new realm.window.WebSocket(
    "http://127.0.0.1:3119".replace(/^http/, "ws") + "/ws",
  );
  const message = (payload) => {
    const event = new Event("message");
    Object.defineProperty(event, "data", { value: JSON.stringify(payload) });
    socket.dispatchEvent(event);
  };
  message({ type: "roomJoined", participantId: "account-owner" });
  const api = realm.window.__sessionSoak;
  api.arm();
  return { api, socket, message, input, sent, realm };
}

test("observer correlates native UI send, exact own ack and peer payload without changing frames", () => {
  const { api, socket, message, sent } = fixture();
  api.expect({
    content: "Session soak 0001",
    senderId: "account-owner",
    own: true,
  });
  const frame = JSON.stringify({
    type: "chatMessage",
    content: "Session soak 0001",
    clientMessageId: "client-1",
  });
  socket.send(frame);
  assert.deepEqual(sent, [frame]);
  message({
    type: "messageAck",
    clientMessageId: "client-1",
    message: {
      content: "Session soak 0001",
      participantId: "account-owner",
      clientMessageId: "client-1",
      messageId: "message-1",
    },
  });
  assert.equal(api.delivery("Session soak 0001").ack, true);
  assert.equal(api.status().acknowledgements, 1);
  api.expect({
    content: "Session soak 0002",
    senderId: "account-peer",
    own: false,
  });
  message({
    type: "chatReceived",
    content: "Session soak 0002",
    participantId: "account-peer",
    messageId: "message-2",
  });
  assert.equal(api.delivery("Session soak 0002").received, true);
  assert.equal(api.status().peerDeliveries, 1);
  assert.equal(api.status().failure, null);
  assert.equal(JSON.stringify(api.status()).includes("Session soak"), false);
});

test("observer preserves duplicate, sender and ack-correlation failures", () => {
  const { api, socket, message } = fixture();
  api.expect({
    content: "Session soak 0001",
    senderId: "account-owner",
    own: true,
  });
  socket.send(
    JSON.stringify({
      type: "chatMessage",
      content: "Session soak 0001",
      clientMessageId: "client-1",
    }),
  );
  message({
    type: "messageAck",
    clientMessageId: "incorrect-client",
    message: {
      content: "Session soak 0001",
      participantId: "account-owner",
      messageId: "message-1",
      clientMessageId: "client-1",
    },
  });
  assert.equal(api.status().failure, "incorrect_message_ack");
  message({
    type: "chatReceived",
    content: "unrelated text",
    participantId: "other",
    messageId: "message-2",
  });
  assert.equal(api.status().failure, "incorrect_message_ack");
  const peer = fixture();
  peer.api.expect({
    content: "Session soak 0001",
    senderId: "account-peer",
    own: false,
  });
  const frame = {
    type: "chatReceived",
    content: "Session soak 0001",
    participantId: "account-peer",
    messageId: "message-1",
  };
  peer.message(frame);
  peer.message(frame);
  assert.equal(peer.api.status().failure, "duplicate_live_delivery");
});

test("observer distinguishes renewals, rejected resume, fresh join and input recovery", () => {
  const { api, socket, message, input } = fixture();
  message({ type: "authenticationRenewed", token: "DO-NOT-RETAIN" });
  message({ type: "authenticationRenewalFailed", message: "DO-NOT-RETAIN" });
  message({ type: "reconnectResult", success: false });
  message({ type: "roomJoined", participantId: "account-owner" });
  input.disabled = true;
  assert.equal(api.status().inputDisabled, true);
  input.disabled = false;
  const close = new Event("close");
  Object.assign(close, { code: 1008, wasClean: true });
  socket.dispatchEvent(close);
  const value = api.status();
  assert.equal(value.authenticationRenewed, 1);
  assert.equal(value.authenticationRenewalFailed, 1);
  assert.equal(value.resumeRejected, 1);
  assert.equal(value.freshJoins, 1);
  assert.equal(value.socketCloses, 1);
  assert.equal(value.disabledTransitions, 1);
  assert.equal(value.inputDisabled, false);
  assert.equal(JSON.stringify(value).includes("DO-NOT-RETAIN"), false);
  api.cleanup();
  socket.dispatchEvent(close);
  assert.equal(api.status().socketCloses, 1);
});

test("text-only observer rejects media requests without invoking capture", async () => {
  const { api, realm } = fixture();
  await assert.rejects(
    realm.navigator.mediaDevices.getUserMedia({ audio: true }),
    { name: "NotAllowedError" },
  );
  assert.equal(api.status().captureRequests, 1);
});

test("unbounded Playwright evaluations are enclosed by an independent deadline", async () => {
  assert.equal(await within(() => Promise.resolve(42), 50), 42);
  await assert.rejects(
    within(() => new Promise(() => {}), 10),
    /operation_timeout/,
  );
});
