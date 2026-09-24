import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
import { deferred, flush } from './ui-fixture.mjs';

async function fixture() {
  const timers = new Map();
  let sequence = 0;
  const devices = new EventTarget();
  const calls = [];
  const captures = [];
  devices.enumerateDevices = async () => [];
  devices.getUserMedia = () => {
    captures.push(true);
    throw new Error('Capture forbidden');
  };
  class Audio {
    sinkId = '';
    paused = true;
    currentTime = 0;
    constructor(src) {
      this.src = src;
    }
    async setSinkId(id) {
      calls.push(id);
      this.sinkId = id;
    }
    play() {
      this.paused = false;
      calls.push('play');
      return Promise.resolve();
    }
    pause() {
      this.paused = true;
    }
    removeAttribute(name) {
      if (name === 'src') this.src = '';
    }
    load() {
      calls.push('load');
    }
  }
  const revoked = [];
  const urls = [];
  const api = await loadTypeScript('src/audio-output.ts', {
    globals: {
      navigator: { mediaDevices: devices },
      HTMLMediaElement: Audio,
      Audio,
      URL: {
        createObjectURL: (blob) => {
          urls.push(blob);
          return 'blob:owned';
        },
        revokeObjectURL: (url) => revoked.push(url),
      },
      setTimeout: (callback) => {
        timers.set(++sequence, callback);
        return sequence;
      },
      clearTimeout: (id) => timers.delete(id),
    },
  });
  return { ...api, devices, timers, calls, captures, Audio, revoked, urls };
}

test('speaker routing applies to audio and screen video without capture and serializes newer choices', async () => {
  const f = await fixture();
  const audio = new f.Audio();
  const screen = new f.Audio();
  const elements = [audio, screen];
  const output = new f.AudioOutput(() => elements);
  const first = deferred();
  const calls = [];
  audio.setSinkId = (id) => {
    calls.push(id);
    return (id === 'first' ? first.promise : Promise.resolve()).then(() => {
      audio.sinkId = id;
    });
  };
  const earlier = output.change('first');
  const later = output.change('latest');
  assert.deepEqual(calls, ['first']);
  first.resolve();
  await Promise.all([earlier, later]);
  assert.deepEqual(calls, ['first', 'latest']);
  assert.equal(audio.sinkId, 'latest');
  assert.equal(screen.sinkId, 'latest');
  const added = new f.Audio();
  elements.push(added);
  await output.attach(added);
  assert.equal(added.sinkId, 'latest');
  assert.equal(f.captures.length, 0);
  assert.equal(f.timers.size, 0);
});

test('a hung speaker switch has a bounded UI deadline but never stacks native calls; retired elements are not retried', async () => {
  const f = await fixture();
  const audio = new f.Audio();
  let elements = [audio];
  let nativeCalls = 0;
  const native = deferred();
  audio.setSinkId = () => {
    nativeCalls++;
    return native.promise;
  };
  const output = new f.AudioOutput(() => elements);
  const request = output.change('missing');
  const rejected = assert.rejects(request, (error) => error.name === 'TimeoutError');
  for (const callback of f.timers.values()) callback();
  await rejected;
  const latest = output.change('other');
  assert.equal(nativeCalls, 1);
  elements = [];
  native.resolve();
  await latest;
  assert.equal(nativeCalls, 1);
});

test('device changes coalesce, keep one native call through timeouts, and discard results after teardown', async () => {
  const f = await fixture();
  const pending = deferred();
  let enumerations = 0;
  const updates = [];
  let failures = 0;
  f.devices.enumerateDevices = () => {
    enumerations++;
    return pending.promise;
  };
  const watcher = f.observeMediaDevices(
    (devices) => updates.push(devices),
    () => failures++,
  );
  await flush();
  for (let index = 0; index < 50; index++) f.devices.dispatchEvent(new Event('devicechange'));
  assert.equal(enumerations, 1);
  for (const callback of f.timers.values()) callback();
  assert.equal(failures, 1);
  watcher.refresh();
  assert.equal(enumerations, 1);
  watcher.dispose();
  pending.resolve([{ kind: 'audiooutput', deviceId: 'private' }]);
  await flush();
  f.devices.dispatchEvent(new Event('devicechange'));
  await flush();
  assert.deepEqual(updates, []);
  assert.equal(enumerations, 1);
  assert.equal(f.timers.size, 0);
  assert.equal(f.captures.length, 0);
});

test('device hot-plug refreshes a live observer and preserves the browser-provided list', async () => {
  const f = await fixture();
  const updates = [];
  const watcher = f.observeMediaDevices(
    (devices) => updates.push(devices),
    () => assert.fail('unexpected failure'),
  );
  await flush();
  const headset = { kind: 'audiooutput', deviceId: 'private', label: 'Headset' };
  f.devices.enumerateDevices = async () => [headset];
  f.devices.dispatchEvent(new Event('devicechange'));
  await flush();
  assert.deepEqual(updates, [[], [headset]]);
  watcher.dispose();
});

test('speaker tone is local and starts only on play; stop and disposal clear owned audio, timer and URL', async () => {
  const f = await fixture();
  const tone = new f.SpeakerTest();
  assert.equal(tone.element.paused, true);
  assert.deepEqual(f.calls, []);
  assert.equal(f.urls[0].type, 'audio/wav');
  assert.equal(f.urls[0].size, 6444);
  const bytes = new DataView(await f.urls[0].arrayBuffer());
  assert.equal(bytes.getUint32(24, true), 8000);
  assert.equal(bytes.getUint32(40, true), 6400);
  const playing = tone.play();
  assert.equal(f.calls[0], 'play', 'native play starts within the calling gesture');
  await playing;
  for (const callback of f.timers.values()) callback();
  assert.equal(tone.element.paused, true);
  tone.dispose();
  tone.dispose();
  assert.deepEqual(f.revoked, ['blob:owned']);
  assert.equal(tone.element.src, '');
  assert.equal(f.timers.size, 0);
  assert.equal(f.captures.length, 0);
});
