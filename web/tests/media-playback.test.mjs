import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
import { createDOM, deferred, flush } from './ui-fixture.mjs';

const denied = () =>
  Object.assign(new Error('User activation required'), { name: 'NotAllowedError' });

async function fixture(t) {
  const dom = createDOM();
  const viewportEvents = new EventTarget();
  const viewportListeners = [];
  const window = {
    innerWidth: 320,
    innerHeight: 568,
    addEventListener(type, callback, options) {
      viewportListeners.push({ type, ...options });
      viewportEvents.addEventListener(type, callback, options);
    },
  };
  // Extend the shared text-only DOM only for the controls used by remote tiles.
  Object.defineProperty(dom.Node.prototype, 'dataset', {
    get() {
      return (this._dataset ??= {});
    },
  });
  Object.defineProperty(dom.Node.prototype, 'classList', {
    get() {
      return {
        contains: (value) => (this.className ?? '').split(/\s+/).includes(value),
        toggle: (value, enabled) => {
          const names = new Set((this.className ?? '').split(/\s+/).filter(Boolean));
          if (enabled) names.add(value);
          else names.delete(value);
          this.className = [...names].join(' ');
        },
      };
    },
  });
  dom.Node.prototype.add = function (option) {
    this.append(option);
  };
  Object.defineProperty(dom.Node.prototype, 'style', {
    get() {
      return (this._style ??= {});
    },
  });
  dom.Node.prototype.matches = function (selector) {
    assert.equal(selector, ':popover-open');
    return this.popoverOpen === true;
  };
  dom.Node.prototype.showPopover = function (options) {
    for (const node of dom.created) if (node.popoverOpen) node.hidePopover();
    this.emit('beforetoggle', { newState: 'open' });
    this.popoverOpen = true;
    this.popoverSource = options.source;
  };
  dom.Node.prototype.hidePopover = function () {
    this.emit('beforetoggle', { newState: 'closed' });
    this.popoverOpen = false;
  };
  dom.Node.prototype.focus = function () {
    dom.document.activeElement = this;
  };
  dom.Node.prototype.getBoundingClientRect = function () {
    if (this.className === 'personal-media-panel')
      return { left: 0, top: 0, right: 240, bottom: 350, width: 240, height: 350 };
    return { left: 260, top: 500, right: 304, bottom: 544, width: 44, height: 44 };
  };
  const query = dom.Node.prototype.querySelectorAll;
  dom.Node.prototype.querySelectorAll = function (selector) {
    const control = selector.match(/^\[data-control="([^"]+)"\]$/)?.[1];
    if (!control) return query.call(this, selector);
    return this.children.flatMap((child) => [
      ...(child.dataset.control === control ? [child] : []),
      ...child.querySelectorAll(selector),
    ]);
  };
  const state = {
    plays: [],
    roomCalls: [],
    notifications: [],
    enumerate: async () => [],
    play: async () => {
      throw denied();
    },
  };
  const { MediaControls } = await loadTypeScript('src/media-controls.ts', {
    modules: {
      './media': {},
      './media-controls.css': {},
      './settings-dialog': {},
      './settings-dialog.css': {},
      './audio-output': await loadTypeScript('src/audio-output.ts'),
    },
    globals: {
      document: dom.document,
      window,
      navigator: { mediaDevices: { enumerateDevices: () => state.enumerate() } },
      localStorage: { getItem: () => null, setItem() {} },
      Option: function (label, value) {
        const option = dom.document.createElement('option');
        option.textContent = label;
        option.value = value;
        return option;
      },
    },
  });
  const controls = new MediaControls({
    notify: (message) => state.notifications.push(message),
    getRoom: () => ({
      setRemoteMediaHidden: (...args) => state.roomCalls.push(['hidden', ...args]),
      setRemoteVideoQuality: (...args) => state.roomCalls.push(['quality', ...args]),
    }),
  });
  t.after(() => controls.destroy());
  const tile = dom.document.createElement('div');
  dom.document.body.append(tile);
  const pin = dom.document.createElement('button');
  pin.className = 'tile-pin';
  pin.textContent = 'Pin';
  pin.setAttribute('aria-pressed', 'false');
  pin.addEventListener('click', () => pin.setAttribute('aria-pressed', 'true'));
  tile.append(pin);
  function media(kind = 'audio') {
    const element = dom.document.createElement(kind);
    const track = { enabled: true, readyState: 'live' };
    element.srcObject = { getTracks: () => [track] };
    element.paused = true;
    element.muted = false;
    element.volume = 1;
    element.pause = () => {
      element.paused = true;
    };
    element.play = () => {
      state.plays.push(element);
      return state.play(element).then(() => {
        element.paused = false;
      });
    };
    tile.append(element);
    return element;
  }
  const audio = media();
  controls.attachTile(tile, 'alice', 'Alice');
  const notice = tile.querySelector('.personal-playback-blocked');
  const control = (name) => tile.querySelector(`[data-control="${name}"]`);
  return { ...dom, controls, state, tile, audio, media, notice, control, pin, viewportListeners };
}

function openMenu(f, tile = f.tile) {
  const disclosure = tile.querySelector('.personal-media-controls');
  const summary = disclosure.querySelector('summary');
  summary.emit('click', { preventDefault() {} });
  return { disclosure, summary, panel: disclosure.querySelector('.personal-media-panel') };
}

test('one compact named disclosure owns the existing pin and positions its panel beyond a tiny tile', async (t) => {
  const f = await fixture(t);
  const { disclosure, summary, panel } = openMenu(f);
  assert.equal(summary.getAttribute('aria-label'), 'Controls for Alice');
  assert.equal(summary.textContent, '⋯');
  assert.equal(summary.getAttribute('aria-haspopup'), 'dialog');
  assert.equal(summary.getAttribute('aria-controls'), panel.id);
  assert.equal(panel.getAttribute('popover'), 'auto');
  assert.equal(panel.getAttribute('role'), 'dialog');
  assert.equal(f.pin.parentNode, panel, 'move the real pin, retaining its existing listener');
  assert.equal(panel.popoverSource, summary);
  assert.equal(disclosure.open, true);
  assert.equal(summary.getAttribute('aria-expanded'), 'true');
  assert.equal(f.document.activeElement, panel);
  assert.equal(panel.style.left, '64px');
  assert.equal(
    panel.style.top,
    '146px',
    'open above a low tile when below would cross the viewport',
  );
  assert.equal(panel.style.maxWidth, '304px');
  f.pin.click();
  assert.equal(f.pin.getAttribute('aria-pressed'), 'true');
  assert.equal(panel.popoverOpen, false);
  assert.equal(disclosure.open, false);
});

test('native light dismissal clears disclosure state/listeners and permits reopening and explicit close', async (t) => {
  const f = await fixture(t);
  const { disclosure, summary, panel } = openMenu(f);
  assert.ok(f.viewportListeners.every(({ signal }) => !signal.aborted));
  panel.hidePopover(); // The browser uses the same event for Escape/outside taps.
  assert.equal(disclosure.open, false);
  assert.equal(summary.getAttribute('aria-expanded'), 'false');
  assert.ok(f.viewportListeners.every(({ signal }) => signal.aborted));
  openMenu(f);
  assert.equal(panel.popoverOpen, true);
  panel.querySelector('.personal-media-dismiss').click();
  assert.equal(panel.popoverOpen, false);
  assert.equal(f.document.activeElement, summary);
  assert.ok(f.viewportListeners.every(({ signal }) => signal.aborted));
});

test('detaching one open tile releases viewport listeners but preserves sibling controls and preferences', async (t) => {
  const f = await fixture(t);
  f.control('volume').value = '37';
  f.control('volume').emit('input');
  const screen = f.document.createElement('div');
  f.document.body.append(screen);
  f.controls.attachTile(screen, 'alice', 'Alice');
  const { panel } = openMenu(f);
  f.controls.detachTile(f.tile);
  assert.equal(panel.popoverOpen, false);
  assert.ok(f.viewportListeners.every(({ signal }) => signal.aborted));
  assert.equal(f.tile.querySelector('.personal-media-controls'), null);
  assert.ok(screen.querySelector('.personal-media-controls'));
  assert.equal(screen.querySelector('[data-control="volume"]').value, '37');
  f.controls.detachTile(f.tile); // Cleanup is idempotent.
});

/** Firefox counts no MediaStream frames in getVideoPlaybackQuality(); frame callbacks work everywhere. */
function playingVideo(f, tile = f.tile) {
  const video = f.document.createElement('video');
  video.srcObject = { getTracks: () => [{ kind: 'video', readyState: 'live' }] };
  video.paused = false;
  video.callbacks = [];
  video.requestVideoFrameCallback = (callback) => video.callbacks.push(callback);
  video.present = () => video.callbacks.splice(0).forEach((callback) => callback());
  video.getVideoPlaybackQuality = () => ({ totalVideoFrames: 0 });
  video.pause = () => {
    video.paused = true;
  };
  video.play = async () => {
    video.paused = false;
  };
  tile.append(video);
  return video;
}

test('a remote camera that stops delivering frames says so on its tile until frames return', async (t) => {
  const f = await fixture(t);
  const video = playingVideo(f);
  const notice = f.tile.querySelector('.video-stalled-notice');
  assert.equal(notice.getAttribute('role'), 'status');
  f.controls.checkVideoProgress(1_000);
  video.present();
  f.controls.checkVideoProgress(6_000);
  assert.equal(video.callbacks.length, 1, 'one callback waits for the next frame');
  f.controls.checkVideoProgress(8_000);
  f.controls.checkVideoProgress(11_000);
  assert.equal(video.callbacks.length, 1, 'and is not stacked while it waits');
  assert.equal(notice.hidden, true, 'five seconds without a frame is not yet a stall');
  f.controls.checkVideoProgress(12_000);
  assert.equal(notice.hidden, false);
  assert.match(notice.textContent, /^Alice's video stopped at /);
  video.present();
  f.controls.checkVideoProgress(14_000);
  assert.equal(notice.hidden, true, 'the notice clears with the next frame');
});

test('a browser without frame callbacks is never told a camera stopped', async (t) => {
  const f = await fixture(t);
  const video = playingVideo(f);
  delete video.requestVideoFrameCallback;
  for (const now of [0, 10_000, 60_000]) f.controls.checkVideoProgress(now);
  assert.equal(f.tile.querySelector('.video-stalled-notice').hidden, true);
});

test('screen shares, people hidden for me and paused elements never read as stalled', async (t) => {
  const f = await fixture(t);
  const video = playingVideo(f);
  const notice = f.tile.querySelector('.video-stalled-notice');
  f.tile.className = 'video-tile screen-share';
  f.controls.checkVideoProgress(0);
  f.controls.checkVideoProgress(60_000);
  assert.equal(notice.hidden, true, 'a still screen sends no frames');
  f.tile.className = 'video-tile';
  video.paused = true;
  f.controls.checkVideoProgress(120_000);
  assert.equal(notice.hidden, true, 'blocked playback has its own notice');
  video.paused = false;
  f.control('hide').click();
  f.controls.checkVideoProgress(130_000);
  f.controls.checkVideoProgress(200_000);
  assert.equal(notice.hidden, true, 'a hidden broadcast is not expected to play');
});

test('blocked remote playback exposes an accessible action and its click retries without changing preferences or publishers', async (t) => {
  const f = await fixture(t);
  const toolbar = f.document.createElement('div');
  f.document.body.append(toolbar);
  f.controls.mountToolbar(toolbar);
  const master = toolbar.querySelector('input');
  master.value = '50';
  master.emit('input');
  f.control('volume').value = '60';
  f.control('volume').emit('input');
  await flush();
  assert.equal(f.notice.hidden, false);
  assert.equal(f.notice.getAttribute('role'), 'status');
  const retry = f.control('retry-playback');
  assert.equal(retry.textContent, 'Enable playback');
  assert.equal(retry.getAttribute('aria-label'), 'Enable playback for Alice');
  assert.equal(f.audio.volume, 0.3);
  const source = f.audio.srcObject;
  const plays = f.state.plays.length;
  f.state.play = async () => {};
  retry.click();
  assert.equal(
    f.state.plays.length,
    plays + 1,
    'play must run inside the click, before an asynchronous boundary',
  );
  await flush();
  assert.equal(f.notice.hidden, true);
  assert.equal(f.audio.paused, false);
  assert.equal(f.audio.muted, false);
  assert.equal(f.audio.volume, 0.3);
  assert.equal(f.audio.srcObject, source);
  assert.equal(source.getTracks()[0].enabled, true);
  assert.deepEqual(f.state.roomCalls, [], 'playback recovery is viewer-only');
});

for (const preference of ['muted', 'hidden', 'silent']) {
  test(`blocked ${preference} playback does not prompt or override the preference`, async (t) => {
    const f = await fixture(t);
    if (preference === 'silent') {
      f.control('volume').value = '0';
      f.control('volume').emit('input');
    } else f.control(preference === 'muted' ? 'mute' : 'hide').click();
    await flush();
    assert.equal(f.notice.hidden, true);
    f.control('retry-playback').click(); // Even a stale/programmatic click must not clear preferences.
    await flush();
    assert.equal(f.notice.hidden, true);
    assert.equal(f.audio.muted, preference !== 'silent');
    if (preference === 'hidden') assert.equal(f.audio.paused, true);
    if (preference === 'silent') assert.equal(f.audio.volume, 0);
    assert.equal(f.audio.srcObject.getTracks()[0].enabled, true);
  });
}

for (const operation of ['detach', 'reset', 'remove']) {
  test(`${operation} prevents a late rejected play from reviving recovery UI`, async (t) => {
    const f = await fixture(t);
    await flush();
    const pending = deferred();
    f.state.play = () => pending.promise;
    f.control('retry-playback').click();
    if (operation === 'detach') f.controls.detachParticipant('alice');
    else if (operation === 'reset') f.controls.reset();
    else f.tile.remove();
    const before = f.notice.hidden;
    pending.reject(denied());
    await flush();
    assert.equal(f.notice.isConnected, false);
    assert.equal(f.notice.hidden, before, 'late completion must not mutate detached UI');
    assert.equal(f.document.querySelector('.personal-playback-blocked'), null);
  });
}

for (const replacement of ['stream', 'element']) {
  test(`a late rejection for an old ${replacement} cannot mark its replacement as blocked`, async (t) => {
    const f = await fixture(t);
    await flush();
    const pending = deferred();
    f.state.play = () => pending.promise;
    f.control('retry-playback').click();
    f.state.play = async () => {};
    if (replacement === 'stream') f.audio.srcObject = {};
    else {
      f.audio.remove();
      f.media();
    }
    f.controls.attachTile(f.tile, 'alice', 'Alice');
    await flush();
    assert.equal(f.notice.hidden, true);
    pending.reject(denied());
    await flush();
    assert.equal(f.notice.hidden, true);
  });
}

test('recovery waits for every blocked element on its tile and does not retry another tile', async (t) => {
  const f = await fixture(t);
  const video = f.media('video');
  f.controls.attachTile(f.tile, 'alice', 'Alice');
  const other = f.document.createElement('div');
  f.document.body.append(other);
  const otherAudio = f.media();
  other.append(otherAudio);
  f.controls.attachTile(other, 'alice', 'Alice');
  await flush();
  const otherPlays = f.state.plays.filter((element) => element === otherAudio).length;
  f.state.play = async (element) => {
    if (element === video) throw denied();
  };
  f.control('retry-playback').click();
  await flush();
  assert.equal(f.audio.paused, false);
  assert.equal(
    f.notice.hidden,
    false,
    'one successful element must not hide another blocked element',
  );
  f.state.play = async () => {};
  f.control('retry-playback').click();
  await flush();
  assert.equal(f.notice.hidden, true);
  assert.equal(f.state.plays.filter((element) => element === otherAudio).length, otherPlays);
  assert.equal(
    otherAudio.paused,
    true,
    'another tile belonging to the same participant stays untouched',
  );
  assert.deepEqual(f.state.roomCalls, []);
});

test('stream identity is checked even before the replacement is attached to controls', async (t) => {
  const f = await fixture(t);
  const pending = deferred();
  f.state.play = () => pending.promise;
  f.controls.attachTile(f.tile, 'alice', 'Alice');
  f.audio.srcObject = {};
  pending.reject(denied());
  await flush();
  assert.equal(f.notice.hidden, true);
});

test('non-policy playback rejection does not masquerade as an autoplay permission prompt', async (t) => {
  const f = await fixture(t);
  f.state.play = async () => {
    throw Object.assign(new Error('Replaced stream'), { name: 'AbortError' });
  };
  f.controls.attachTile(f.tile, 'alice', 'Alice');
  await flush();
  assert.equal(f.notice.hidden, true);
});

test('resume preserves per-person mute, hide and volume while retrying paused visible playback', async (t) => {
  const f = await fixture(t);
  await flush();
  f.state.play = async () => {};
  f.control('volume').value = '32';
  f.control('volume').emit('input');
  f.control('mute').click();
  await flush();
  f.audio.paused = true;
  f.controls.resumePlayback();
  await flush();
  assert.equal(f.audio.paused, false);
  assert.equal(f.audio.muted, true);
  assert.equal(f.audio.volume, 0.32);
  f.control('hide').click();
  const before = f.state.plays.length;
  f.controls.resumePlayback();
  await flush();
  assert.equal(f.state.plays.length, before);
  assert.equal(f.audio.paused, true);
  assert.equal(f.audio.muted, true);
});

test('speaker disappearance is reported once without choosing a fallback or losing personal preferences', async (t) => {
  const f = await fixture(t);
  await f.controls.output.change('headset');
  const pending = deferred();
  let calls = 0;
  f.state.enumerate = () => {
    calls++;
    return pending.promise;
  };
  for (let index = 0; index < 20; index++) f.controls.refreshDevices();
  await flush();
  assert.equal(calls, 1);
  pending.resolve([]);
  await flush();
  assert.equal(f.controls.output.selected, 'headset');
  assert.equal(f.state.notifications.length, 1);
  assert.match(f.state.notifications[0], /Reconnect it or choose a speaker/);
  f.controls.refreshDevices();
  await flush();
  assert.equal(f.state.notifications.length, 1);
  f.state.enumerate = async () => [{ kind: 'audiooutput', deviceId: 'headset' }];
  f.controls.refreshDevices();
  await flush();
  assert.equal(f.controls.outputWarning, false);
});

test('old room device enumeration cannot show a speaker warning after leave', async (t) => {
  const f = await fixture(t);
  await f.controls.output.change('headset');
  const pending = deferred();
  f.state.enumerate = () => pending.promise;
  f.controls.refreshDevices();
  await flush();
  f.controls.reset();
  pending.resolve([]);
  await flush();
  assert.deepEqual(f.state.notifications, []);
});

test('background lifecycle stops private preview and tones, without automatic foreground capture', async (t) => {
  const f = await fixture(t);
  const stopped = [];
  f.controls.preview = { stop: () => stopped.push('preview') };
  f.controls.speakerTest = { stop: () => stopped.push('tone'), dispose() {} };
  f.controls.setPageActive(false);
  assert.deepEqual(stopped, ['preview', 'tone']);
  f.controls.setPageActive(true);
  assert.deepEqual(stopped, ['preview', 'tone']);
});
