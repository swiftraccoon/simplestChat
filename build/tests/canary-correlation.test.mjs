import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { runInNewContext } from "node:vm";
import helper from "../../web/e2e/canary-correlation.cjs";

const {
  collectCanaryMediaSample,
  openCanaryCorrelation,
  projectSample,
  movement,
  MAX_SAMPLES,
  MAX_BYTES,
} = helper;
const secret = "DO_NOT_RETAIN_CREDENTIAL_ADDRESS_DEVICE_OR_NATIVE_ID";
const clock = {
  startedAtMs: 100,
  finishedAtMs: 110,
  epochAtStartMs: 1000,
  epochAtFinishMs: 1010,
};
const stream = {
  peerOrdinal: 1,
  streamOrdinal: 1,
  direction: "send",
  kind: "video",
  ssrc: 123,
  rtxSsrc: 456,
  codec: "video/VP8",
  timestampMs: 1000,
  packets: 10,
  bytes: 3000,
  frames: 3,
  retransmittedPackets: 1,
  frameWidth: 640,
  frameHeight: 360,
  active: true,
};
const sample = (streams = [stream]) => ({
  ...clock,
  status: "complete",
  issues: [],
  streams,
});

function fixture(t) {
  const root = fs.mkdtempSync(
    path.join(os.tmpdir(), "canary-correlation-test."),
  );
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const artifacts = path.join(root, "public");
  fs.mkdirSync(artifacts, { mode: 0o700 });
  return {
    root,
    artifacts,
    file: path.join(root, "private.json"),
    enabled: true,
    profiles: ["baseline"],
    impairmentEnabled: false,
  };
}

function browserCollector(peers) {
  let now = 100;
  const timers = new Set();
  const execute = runInNewContext(
    `(${collectCanaryMediaSample.toString()})`,
    {
      window: { __communityPeers: peers },
      performance: { now: () => ++now },
      Date,
      setTimeout(callback, delay) {
        assert.ok(delay > 0 && delay <= 1000);
        const timer = setTimeout(() => {
          timers.delete(timer);
          callback();
        }, 5);
        timers.add(timer);
        return timer;
      },
      clearTimeout(timer) {
        clearTimeout(timer);
        timers.delete(timer);
      },
    },
    { timeout: 1000 },
  );
  return async () => {
    const value = JSON.parse(JSON.stringify(await execute()));
    assert.equal(timers.size, 0, "all bounded collection timers are cleared");
    assert.ok(value.streams.length <= 16);
    assert.doesNotMatch(JSON.stringify(value), new RegExp(secret));
    return value;
  };
}

test("browser collection retains audio/video SSRC and numeric counters, never credentials or native IDs", async () => {
  const entries = [
    ["codec", { type: "codec", mimeType: "video/VP8", sdpFmtpLine: secret }],
    [
      "v",
      {
        id: secret,
        type: "outbound-rtp",
        kind: "video",
        ssrc: 123,
        rtxSsrc: 456,
        codecId: "codec",
        timestamp: 1000,
        packetsSent: 10,
        bytesSent: 3000,
        framesEncoded: 3,
        trackIdentifier: secret,
        rid: secret,
        address: secret,
        usernameFragment: secret,
        retransmittedPacketsSent: 1,
      },
    ],
    [
      "a",
      {
        id: "audio",
        type: "outbound-rtp",
        kind: "audio",
        ssrc: 222,
        packetsSent: 20,
        bytesSent: 500,
        timestamp: 1000,
        mediaSourceId: secret,
      },
    ],
    [
      "r",
      {
        id: "received",
        type: "inbound-rtp",
        kind: "video",
        ssrc: 333,
        packetsReceived: 30,
        bytesReceived: 4000,
        framesDecoded: 8,
        timestamp: 1000,
      },
    ],
    [
      "candidate",
      { type: "local-candidate", id: secret, address: secret, port: 10000 },
    ],
  ];
  const peer = Object.freeze({
    connectionState: "connected",
    getStats: async () => new Map(entries),
    get localDescription() {
      assert.fail("collector must not inspect SDP");
    },
    getSenders() {
      assert.fail("collector must not inspect capture devices");
    },
  });
  const collect = browserCollector([peer]);
  const first = await collect();
  assert.equal(first.status, "complete");
  assert.deepEqual(
    first.streams.map(({ direction, kind, ssrc, rtxSsrc }) => ({
      direction,
      kind,
      ssrc,
      rtxSsrc,
    })),
    [
      { direction: "send", kind: "video", ssrc: 123, rtxSsrc: 456 },
      { direction: "send", kind: "audio", ssrc: 222, rtxSsrc: null },
      { direction: "receive", kind: "video", ssrc: 333, rtxSsrc: null },
    ],
  );
  const second = await collect();
  assert.deepEqual(
    second.streams.map((value) => value.streamOrdinal),
    [1, 2, 3],
  );
  assert.equal(
    first.streams[1].frames,
    null,
    "missing observations are unknown",
  );
});

test("native stats timeout does not stack more requests on the hung peer", async () => {
  let calls = 0;
  let resolve;
  const pending = new Promise((done) => {
    resolve = done;
  });
  const collect = browserCollector([
    {
      connectionState: "connected",
      getStats: () => {
        calls++;
        return pending;
      },
    },
  ]);
  const first = await collect();
  assert.ok(first.issues.includes("stats_timeout"));
  const second = await collect();
  assert.ok(second.issues.includes("stats_pending"));
  assert.equal(calls, 1);
  resolve(
    new Map([
      ["late", { id: "late", type: "outbound-rtp", kind: "video", ssrc: 1 }],
    ]),
  );
  await new Promise((done) => setImmediate(done));
  assert.equal(
    first.streams.length,
    0,
    "late completion cannot rewrite a completed observation",
  );
  assert.equal((await collect()).streams.length, 1);
  assert.equal(calls, 2);
});

test("peer, stat and stream limits are explicit coverage gaps", async () => {
  const entries = Array.from({ length: 20 }, (_, index) => [
    index,
    {
      id: String(index),
      type: "outbound-rtp",
      kind: "video",
      ssrc: index,
    },
  ]);
  const collect = browserCollector(
    Array.from({ length: 5 }, () => ({
      connectionState: "connected",
      getStats: async () => new Map(entries),
    })),
  );
  const result = await collect();
  assert.equal(result.status, "incomplete");
  assert.ok(result.issues.includes("peer_limit"));
  assert.ok(result.issues.includes("stream_limit"));
  assert.equal(result.streams.length, 16);
  const stats = browserCollector([
    {
      connectionState: "connected",
      getStats: async () =>
        new Map(
          Array.from({ length: 257 }, (_, index) => [
            index,
            { type: "irrelevant" },
          ]),
        ),
    },
  ]);
  assert.ok((await stats()).issues.includes("stats_limit"));
});

test("file projection rejects invalid identities and drops every unrecognized property", () => {
  const result = projectSample({
    ...sample(),
    secret,
    issues: [secret],
    streams: [
      { ...stream, nativeId: secret, localCandidate: { address: secret } },
      { ...stream, streamOrdinal: 2, ssrc: 2 ** 32 },
      { ...stream },
    ],
  });
  assert.doesNotMatch(JSON.stringify(result), new RegExp(secret));
  assert.equal(result.streams.length, 1);
  assert.equal(result.status, "incomplete");
  assert.ok(result.issues.includes("invalid_stream"));
  const invalid = projectSample({ ...sample(), finishedAtMs: 10 });
  assert.ok(invalid.issues.includes("invalid_sample"));
});

test("movement distinguishes resets, changed sources and missing counters from zero progress", () => {
  assert.equal(movement(null, stream), "first_observation");
  assert.equal(movement(stream, { ...stream, packets: 11 }), "increased");
  assert.equal(movement(stream, { ...stream }), "flat");
  assert.equal(
    movement(stream, { ...stream, packets: 0 }),
    "reset_or_replaced",
  );
  assert.equal(
    movement(stream, { ...stream, timestampMs: 900 }),
    "reset_or_replaced",
  );
  assert.equal(movement(stream, { ...stream, ssrc: 124 }), "reset_or_replaced");
  assert.equal(movement(stream, { ...stream, packets: null }), "unknown");
});

test("private evidence is exclusive 0600, outside public artifacts, and contains no unreviewed fields", (t) => {
  const options = fixture(t);
  const writer = openCanaryCorrelation(options);
  writer.add("publisher", { ...sample(), secret }, { ...clock, secret });
  writer.add("viewer", sample([{ ...stream, direction: "receive" }]), clock);
  writer.finish();
  assert.equal(fs.statSync(options.file).mode & 0o777, 0o600);
  assert.equal(fs.readdirSync(options.artifacts).length, 0);
  assert.equal(writer.summary().coverage, "complete");
  const body = fs.readFileSync(options.file, "utf8");
  assert.doesNotMatch(body, new RegExp(secret));
  assert.equal(JSON.parse(body).samples[0].streams[0].ssrc, 123);
  assert.throws(() => openCanaryCorrelation(options), { code: "EEXIST" });
  assert.equal(
    fs.readFileSync(options.file, "utf8"),
    body,
    "existing evidence must not be replaced",
  );
});

test("admission rejects public paths, symlinked public parents and invalid canary profiles", (t) => {
  const options = fixture(t);
  assert.equal(openCanaryCorrelation({ ...options, file: undefined }), null);
  for (const changes of [
    { file: "relative.json" },
    { enabled: false },
    { impairmentEnabled: true },
    { profiles: ["baseline", "lossy"] },
    { githubActions: true },
    { file: path.join(options.artifacts, "private.json") },
  ])
    assert.throws(() => openCanaryCorrelation({ ...options, ...changes }));
  const alias = path.join(options.root, "alias");
  fs.symlinkSync(options.artifacts, alias);
  assert.throws(
    () =>
      openCanaryCorrelation({
        ...options,
        file: path.join(alias, "private.json"),
      }),
    /outside/,
  );
  fs.symlinkSync(path.join(options.root, "target"), options.file);
  assert.throws(() => openCanaryCorrelation(options), { code: "EEXIST" });
});

test("capture is bounded and keeps unknown coverage without affecting caller workload", (t) => {
  const options = fixture(t);
  const writer = openCanaryCorrelation(options);
  const streams = Array.from({ length: 16 }, (_, index) => ({
    ...stream,
    streamOrdinal: index + 1,
    ssrc: index,
  }));
  for (let index = 0; index < MAX_SAMPLES + 5; index++)
    writer.add(index % 2 ? "viewer" : "publisher", sample(streams), clock);
  writer.finish();
  const data = JSON.parse(fs.readFileSync(options.file, "utf8"));
  assert.equal(data.samples.length, MAX_SAMPLES);
  assert.equal(data.droppedSamples, 5);
  assert.equal(data.coverage, "incomplete");
  assert.ok(data.issues.includes("sample_limit"));
  assert.ok(fs.statSync(options.file).size <= MAX_BYTES);
  assert.equal(writer.summary().coverage, "incomplete");
  assert.doesNotThrow(() => writer.finish());
  assert.doesNotThrow(() => writer.add("publisher", sample(), clock));
});

test("collection errors persist unknown status and fixed issue codes without raw exception text", (t) => {
  const options = fixture(t);
  const writer = openCanaryCorrelation(options);
  writer.add(
    "publisher",
    {
      status: "incomplete",
      issues: ["collection_failed", secret],
      streams: [],
    },
    clock,
  );
  writer.add("viewer", sample(), clock);
  writer.finish();
  const data = JSON.parse(fs.readFileSync(options.file, "utf8"));
  assert.equal(data.coverage, "incomplete");
  assert.equal(data.samples[0].streams.length, 0);
  assert.ok(data.samples[0].issues.includes("collection_failed"));
  assert.doesNotMatch(JSON.stringify(data), new RegExp(secret));
});

test("write failures do not replace workload errors and are explicit in the public coverage summary", (t) => {
  const options = fixture(t);
  const writer = openCanaryCorrelation(options);
  const write = t.mock.method(fs, "writeSync", () => {
    throw new Error(secret);
  });
  assert.doesNotThrow(() => writer.add("publisher", sample(), clock));
  assert.doesNotThrow(() => writer.add("viewer", sample(), clock));
  assert.doesNotThrow(() => writer.finish());
  assert.equal(writer.summary().coverage, "incomplete");
  assert.ok(writer.summary().issues.includes("write_failed"));
  assert.doesNotMatch(JSON.stringify(writer.summary()), new RegExp(secret));
  write.mock.restore();
  assert.equal(
    JSON.parse(fs.readFileSync(options.file, "utf8")).coverage,
    "incomplete",
  );
});

test("partial writes produce a complete bounded JSON document", (t) => {
  const options = fixture(t);
  const original = fs.writeSync;
  t.mock.method(
    fs,
    "writeSync",
    (descriptor, buffer, offset, length, position) =>
      original(descriptor, buffer, offset, Math.min(length, 64), position),
  );
  const writer = openCanaryCorrelation(options);
  writer.add("publisher", sample(), clock);
  writer.add("viewer", sample(), clock);
  writer.finish();
  assert.equal(
    JSON.parse(fs.readFileSync(options.file, "utf8")).coverage,
    "complete",
  );
});
