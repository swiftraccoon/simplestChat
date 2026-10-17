import assert from 'node:assert/strict';
import test from 'node:test';
import vm from 'node:vm';
import { createRequire } from 'node:module';

const { silentUiGuard } = createRequire(import.meta.url)('../e2e/silent-ui-guard.cjs');

function fixture() {
  let nativeCalls = 0;
  const original = () => nativeCalls++;
  class MediaElement {}
  Object.assign(MediaElement.prototype, { play: original, load: original });
  const sandbox = {
    navigator: {},
    HTMLMediaElement: MediaElement,
    Audio: original,
    AudioContext: original,
    OfflineAudioContext: original,
    RTCPeerConnection: original,
    SpeechRecognition: original,
    speechSynthesis: { speak: original },
    EventTarget,
    DOMException,
  };
  sandbox.window = sandbox;
  const context = vm.createContext(sandbox);
  vm.runInContext(`(${silentUiGuard.toString()})();`, context);
  return { sandbox, nativeCalls: () => nativeCalls };
}

test('silent UI guards reject before any original constructor, capture or playback call', async () => {
  const { sandbox, nativeCalls } = fixture();
  for (const name of ['Audio', 'AudioContext', 'OfflineAudioContext', 'RTCPeerConnection'])
    assert.throws(() => new sandbox[name](), { name: 'NotAllowedError' });
  for (const name of ['getUserMedia', 'getDisplayMedia', 'enumerateDevices'])
    await assert.rejects(sandbox.navigator.mediaDevices[name](), { name: 'NotAllowedError' });
  const media = new sandbox.HTMLMediaElement();
  await assert.rejects(media.play(), { name: 'NotAllowedError' });
  assert.throws(() => media.load(), { name: 'NotAllowedError' });
  assert.throws(() => (media.srcObject = {}), { name: 'NotAllowedError' });
  assert.throws(() => (media.autoplay = true), { name: 'NotAllowedError' });
  assert.throws(() => sandbox.speechSynthesis.speak({}), { name: 'NotAllowedError' });
  assert.equal(sandbox.__silentUiGuard.attempts, 12);
  assert.equal(nativeCalls(), 0);
});

test('silent UI guard methods and its attempt ledger cannot be replaced', () => {
  const { sandbox } = fixture();
  assert.throws(() => Object.defineProperty(sandbox.navigator, 'mediaDevices', { value: {} }));
  assert.throws(() => Object.defineProperty(sandbox, 'AudioContext', { value: () => {} }));
  assert.throws(() =>
    Object.defineProperty(sandbox.HTMLMediaElement.prototype, 'play', { value: () => {} }),
  );
  assert.throws(() => Object.defineProperty(sandbox, '__silentUiGuard', { value: {} }));
  const state = sandbox.__silentUiGuard;
  state.attempts = 99;
  state.blocked.push('unowned value');
  assert.equal(sandbox.__silentUiGuard.attempts, 0);
  assert.equal(sandbox.__silentUiGuard.blocked.length, 0);
});
