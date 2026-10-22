/** Private inert Playwright surface; the actual phase/channel and FD0 run unchanged. */
const assert = require("node:assert/strict");
const { EventEmitter } = require("node:events");
const { Socket } = require("node:net");
const vm = require("node:vm");

const [entry, mode] = process.argv.slice(2);
assert.ok(["success", "extra"].includes(mode));
const phase = require(entry);
const { createChannel } = require(
  require("node:path").join(
    require("node:path").dirname(entry),
    "restart-browser-channel.cjs",
  ),
);
const port = process.env.BROWSER_RESTART_PORT;
const origin = `http://127.0.0.1:${port}`;
const pages = [];
let script;
let route;
const socket = new Socket({ fd: 0, readable: true, writable: true });
const channel = createChannel({
  runId: process.env.BROWSER_RESTART_RUN_ID,
  stream: socket,
});
function connect(page) {
  const wire = new EventEmitter();
  wire.url = () => `ws://127.0.0.1:${port}/ws`;
  wire.receive = (type) =>
    wire.emit("framereceived", { payload: JSON.stringify({ type }) });
  page.wire = wire;
  page.emit("websocket", wire);
  wire.receive("roomJoined");
}
class Page extends EventEmitter {
  constructor() {
    super();
    this.frame = {};
    this.values = {};
    this.messages = [];
    this.realm = vm.createContext({
      navigator: { mediaDevices: {} },
      DOMException,
    });
    vm.runInContext(`(${script.toString()})()`, this.realm);
  }
  mainFrame() {
    return this.frame;
  }
  async goto(url, options) {
    assert.equal(url, origin);
    assert.deepEqual(options, { waitUntil: "domcontentloaded" });
    await route({
      request: () => ({ url: () => origin }),
      continue: async () => {},
      abort: async () =>
        assert.fail("Owned fixture origin must remain allowed"),
    });
    this.emit("framenavigated", this.frame);
  }
  locator(selector) {
    return {
      fill: async (value) => {
        this.values[selector] = value;
      },
      click: async () => {
        if (selector === "#join-btn") connect(this);
        else assert.equal(selector, "#leave-btn");
      },
      waitFor: async (options) => {
        assert.ok(["#room-screen", "#join-screen"].includes(selector));
        assert.deepEqual(options, { state: "visible" });
      },
      press: async (key) => {
        assert.equal(selector, "#chat-input");
        assert.equal(key, "Enter");
        for (const page of pages) page.messages.push(this.values[selector]);
      },
      inputValue: async () => this.values[selector],
      getByText: (value, options) => ({
        waitFor: async () => {
          assert.equal(selector, "#chat-messages");
          assert.deepEqual(options, { exact: true });
          assert.ok(this.messages.includes(value));
        },
      }),
    };
  }
  async evaluate(source) {
    return structuredClone(
      vm.runInContext(`(${source.toString()})()`, this.realm),
    );
  }
}
const child = { exitCode: null, signalCode: null };
const server = {
  wsEndpoint: () => "owned-inert-browser",
  process: () => child,
  close: async () => {
    child.exitCode = 0;
  },
  kill: async () => assert.fail("The inert browser must close gracefully"),
};
const browser = {
  version: () => "inert-browser-1",
  async newContext(options) {
    assert.deepEqual(options, {
      permissions: [],
      viewport: { width: 1280, height: 900 },
    });
    return {
      setDefaultTimeout: (value) => assert.equal(value, 10000),
      route: async (pattern, handler) => {
        assert.equal(pattern, "**/*");
        route = handler;
      },
      addInitScript: async (source) => {
        script = source;
      },
      newPage: async () => {
        const page = new Page();
        pages.push(page);
        return page;
      },
    };
  },
};
const playwright = {
  [process.env.BROWSER_RESTART_ENGINE]: {
    async launchServer(options) {
      assert.deepEqual(options, { headless: true, timeout: 15000 });
      return server;
    },
    async connect(endpoint, options) {
      assert.equal(endpoint, "owned-inert-browser");
      assert.deepEqual(options, { timeout: 5000 });
      return browser;
    },
  },
};
const control = {
  async request(name) {
    // Observations follow the actual Rust transition ACK, never a fake parent.
    await channel.request(name);
    if (name === "stop-first")
      for (const page of pages) page.wire.receive("serverRestarting");
    if (name === "start-second") for (const page of pages) connect(page);
    if (name === "stop-second" && mode === "extra")
      await new Promise((resolve, reject) => {
        socket.write("x\n", (error) => (error ? reject(error) : resolve()));
      });
  },
  cancel: (code) => channel.cancel(code),
  finish: () => channel.finish(),
};
phase.main({ playwright, channel: control }).then(
  (status) => {
    process.exitCode = status;
  },
  () => {
    process.stderr.write("Inert restart fixture failed.\n");
    process.exitCode = 1;
  },
);
