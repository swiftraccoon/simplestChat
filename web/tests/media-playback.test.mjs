import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
import { createDOM, deferred, flush } from './ui-fixture.mjs';

const denied = () =>
  Object.assign(new Error('User activation required'), { name: 'NotAllowedError' });

async function fixture(t) {
  const dom = createDOM();
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
    },
    globals: {
      document: dom.document,
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
    notify() {},
    getRoom: () => ({
      setRemoteMediaHidden: (...args) => state.roomCalls.push(['hidden', ...args]),
      setRemoteVideoQuality: (...args) => state.roomCalls.push(['quality', ...args]),
    }),
  });
  t.after(() => controls.destroy());
  const tile = dom.document.createElement('div');
  dom.document.body.append(tile);
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
  return { ...dom, controls, state, tile, audio, media, notice, control };
}

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
