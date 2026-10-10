import assert from 'node:assert/strict';
import test from 'node:test';
import { loadContractModules, loadTypeScript } from './source-loader.mjs';
import { flush, uiFixture } from './ui-fixture.mjs';

/** The Camera & mic screen, with real preview, device and sheet modules over the text DOM. */
async function screenFixture(t, { videoEnabled = true } = {}) {
  const f = await uiFixture();
  const { Node, document } = f;
  document.activeElement = null;
  Object.defineProperty(Node.prototype, 'classList', {
    configurable: true,
    get() {
      const node = this;
      const names = () => (node.className ?? '').split(/\s+/).filter(Boolean);
      return {
        add: (...values) => {
          node.className = [...new Set([...names(), ...values])].join(' ');
        },
        remove: (...values) => {
          node.className = names()
            .filter((name) => !values.includes(name))
            .join(' ');
        },
        toggle: (value, force) => {
          const has = names().includes(value);
          const next = force ?? !has;
          if (next && !has) node.className = [...names(), value].join(' ');
          if (!next && has) node.className = names().filter((n) => n !== value).join(' ');
          return next;
        },
        contains: (value) => names().includes(value),
      };
    },
  });
  Node.prototype.focus = function focus() {
    document.activeElement = this;
  };
  Node.prototype.contains = function contains(node) {
    for (let current = node; current; current = current.parentNode) if (current === this) return true;
    return false;
  };
  Object.defineProperty(Node.prototype, 'dataset', {
    configurable: true,
    get() {
      this._dataset ??= {};
      return this._dataset;
    },
  });
  // Selects keep their options and expose `value` like a browser select.
  const createElement = document.createElement;
  document.createElement = (tag) => {
    const node = createElement(tag);
    if (tag === 'select') {
      node.options = [];
      node.add = (option) => {
        node.options.push(option);
        node.append(option);
      };
      node.replaceChildren = (...nodes) => {
        node.children = [];
        node.options = [];
        for (const option of nodes) node.add(option);
      };
    }
    return node;
  };
  const storage = new Map();
  const localStorage = {
    getItem: (key) => storage.get(key) ?? null,
    setItem: (key, value) => storage.set(key, String(value)),
    removeItem: (key) => storage.delete(key),
  };
  const listeners = new Map();
  const navigator = {
    mediaDevices: {
      enumerateDevices: async () => [
        { kind: 'videoinput', deviceId: 'camera-2', label: 'Elgato', groupId: '' },
        { kind: 'audioinput', deviceId: 'mic-2', label: 'Shure', groupId: '' },
      ],
      addEventListener: (type, callback) => listeners.set(type, callback),
      removeEventListener: (type) => listeners.delete(type),
      getUserMedia: async () => {
        throw new Error('no capture in this test');
      },
    },
  };
  class Option {
    constructor(text, value) {
      this.text = text;
      this.value = value;
      this.tagName = 'OPTION';
      this.parentNode = null;
      this.children = [];
      this.remove = () => {};
      this.querySelectorAll = () => [];
    }
  }
  const modules = await loadContractModules();
  const media = await loadTypeScript('src/media.ts', {
    modules: { '../node_modules/mediasoup-client/lib/Device.js': {}, './signaling': {} },
    globals: { localStorage, navigator, console },
  });
  const sheet = await loadTypeScript('src/settings-sheet.ts', {
    modules: { './ui': f.ui, './settings-sheet.css': {} },
    globals: { document, setTimeout: () => 1, clearTimeout: () => {} },
  });
  const api = await loadTypeScript('src/media-controls.ts', {
    modules: {
      './receive-policy': await loadTypeScript('src/receive-policy.ts'),
      './media': media,
      './media-controls.css': {},
      './settings-sheet': sheet,
      './audio-output': await loadTypeScript('src/audio-output.ts', {
        globals: { navigator, document },
      }),
      ...modules,
    },
    globals: {
      localStorage,
      navigator,
      document,
      console,
      window: {},
      Option,
      setTimeout: () => 1,
      clearTimeout: () => {},
      requestAnimationFrame: () => 1,
      cancelAnimationFrame: () => {},
    },
  });
  const room = {
    actions: [],
    audioEnabled: false,
    videoEnabled,
    setCapturePreferences(value) {
      this.actions.push(['save', value]);
    },
    async switchCamera(id) {
      this.actions.push(['camera', id]);
    },
    async switchMic(id) {
      this.actions.push(['microphone', id]);
    },
    setRemoteMediaHidden() {},
    setRemoteVideoQuality() {},
    setReceiveMode() {},
  };
  const notices = [];
  const controls = new api.MediaControls({ getRoom: () => room, notify: (m) => notices.push(m) });
  const context = {
    pushed: [],
    push(screen) {
      this.pushed.push(screen);
    },
    pop() {},
    close() {},
    current: () => true,
  };
  t.after(() => controls.destroy?.());
  return { ...f, api, media, controls, room, notices, context, localStorage };
}

const selectNamed = (root, name) =>
  root.querySelectorAll('select').find((node) => node.getAttribute('name') === name);

test('the Camera & mic screen lists devices and applies a camera change at once', async (t) => {
  const f = await screenFixture(t);
  const section = f.controls.sections().find((entry) => entry.id === 'camera');
  assert.equal(section.label, 'Camera & mic');
  assert.equal(section.status(), 'Default camera · Default mic');
  const screen = section.screen();
  const body = await screen.body(f.context);
  await flush();
  const camera = selectNamed(body, 'cameraDeviceId');
  assert.ok(camera, 'the camera select keeps its name');
  assert.deepEqual(
    camera.options.map((option) => option.value),
    ['', 'camera-2'],
    'enumerated cameras fill the select',
  );
  camera.value = 'camera-2';
  camera.emit('change');
  await flush();
  assert.deepEqual(f.room.actions.map((action) => action[0]), ['save', 'camera']);
  assert.equal(f.media.loadCapturePreferences().cameraDeviceId, 'camera-2');
  assert.equal(section.status(), 'Elgato · Default mic', 'the list status names the device');
  const row = camera.parentNode;
  assert.equal(row.querySelector('.sheet-row-saved')?.textContent, 'Saved');
  screen.leave?.();
});

test('a failed device switch restores the select to the confirmed choice and reports it', async (t) => {
  const f = await screenFixture(t);
  f.room.switchCamera = async () => {
    throw new Error('Device unavailable');
  };
  const screen = f.controls.sections().find((entry) => entry.id === 'camera').screen();
  const body = await screen.body(f.context);
  await flush();
  const camera = selectNamed(body, 'cameraDeviceId');
  camera.value = 'camera-2';
  camera.emit('change');
  await flush();
  assert.equal(camera.value, '', 'the select returns to the saved baseline');
  assert.equal(f.media.loadCapturePreferences().cameraDeviceId, '');
  const status = body.querySelector('.media-preview-status');
  assert.match(status.textContent, /Device unavailable/);
  screen.leave?.();
});

test('the Quality screen applies capture options one at a time', async (t) => {
  const f = await screenFixture(t);
  const screen = f.controls.sections().find((entry) => entry.id === 'camera').screen();
  const body = await screen.body(f.context);
  await flush();
  const quality = body
    .querySelectorAll('button')
    .find((node) => node.querySelector('.sheet-row-label')?.textContent === 'Quality');
  assert.equal(quality.querySelector('.sheet-row-status').textContent, '720p · 30 fps');
  quality.click();
  const pushed = f.context.pushed.at(-1);
  assert.equal(pushed.title, 'Quality');
  const qualityBody = await pushed.body(f.context);
  const frameRate = selectNamed(qualityBody, 'frameRate');
  frameRate.value = '15';
  frameRate.emit('change');
  await flush();
  assert.equal(f.media.loadCapturePreferences().frameRate, 15);
  assert.deepEqual(f.room.actions.map((action) => action[0]), ['save', 'camera']);
  const noise = qualityBody
    .querySelectorAll('input')
    .find((node) => node.getAttribute('name') === 'noiseSuppression');
  noise.checked = false;
  noise.emit('change');
  await flush();
  assert.equal(f.media.loadCapturePreferences().noiseSuppression, false);
  screen.leave?.();
});
