/** Installed before application code; these UI fixtures must never capture or produce sound. */
function silentUiGuard() {
  let attempts = 0;
  const blocked = [];
  const deny = (name) => {
    attempts++;
    if (blocked.length < 20) blocked.push(name);
    throw new DOMException('Disabled by silent UI fixture', 'NotAllowedError');
  };
  const reject = (name) => () => {
    try {
      deny(name);
    } catch (error) {
      return Promise.reject(error);
    }
  };
  const lock = (object, name, value) =>
    Object.defineProperty(object, name, { value, configurable: false, writable: false });
  const mediaDevices = Object.assign(new EventTarget(), {
    getUserMedia: reject('getUserMedia'),
    getDisplayMedia: reject('getDisplayMedia'),
    enumerateDevices: reject('enumerateDevices'),
  });
  lock(navigator, 'mediaDevices', Object.freeze(mediaDevices));
  for (const method of ['getUserMedia', 'webkitGetUserMedia', 'mozGetUserMedia'])
    lock(navigator, method, () => deny(method));
  for (const name of [
    'Audio',
    'AudioContext',
    'webkitAudioContext',
    'OfflineAudioContext',
    'webkitOfflineAudioContext',
    'RTCPeerConnection',
    'webkitRTCPeerConnection',
    'SpeechRecognition',
    'webkitSpeechRecognition',
  ])
    if (name in window)
      lock(window, name, function () {
        deny(name);
      });
  lock(HTMLMediaElement.prototype, 'play', reject('HTMLMediaElement.play'));
  lock(HTMLMediaElement.prototype, 'load', () => deny('HTMLMediaElement.load'));
  Object.defineProperty(HTMLMediaElement.prototype, 'srcObject', {
    get: () => null,
    set: () => deny('HTMLMediaElement.srcObject'),
    configurable: false,
  });
  Object.defineProperty(HTMLMediaElement.prototype, 'autoplay', {
    get: () => false,
    set: (enabled) => {
      if (enabled) deny('HTMLMediaElement.autoplay');
    },
    configurable: false,
  });
  if (window.speechSynthesis)
    lock(window.speechSynthesis, 'speak', () => deny('speechSynthesis.speak'));
  Object.defineProperty(window, '__silentUiGuard', {
    get: () => ({ installed: true, attempts, blocked: [...blocked] }),
    configurable: false,
  });
}

/** Return only fixed guard names/counts, never application or media data. */
async function silentUiState(page) {
  const state = await page.evaluate(() => window.__silentUiGuard);
  if (!state?.installed || !Number.isSafeInteger(state.attempts))
    throw new Error('Silent UI guard was not installed');
  if (state.attempts !== 0)
    throw new Error(`Silent UI attempted a blocked API: ${state.blocked.join(', ')}`);
  return state;
}

module.exports = { silentUiGuard, silentUiState };
