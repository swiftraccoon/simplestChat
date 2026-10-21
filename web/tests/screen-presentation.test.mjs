import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';

const codecs = (mime) => [
  { mimeType: 'audio/opus' },
  { mimeType: 'video/rtx' },
  { mimeType: mime },
  { mimeType: 'video/VP8' },
];

test('presentation preference defaults to detail and keeps explicit choice without storage', async () => {
  const stored = new Map();
  let blocked = false;
  const api = await loadTypeScript('src/screen-presentation.ts', {
    globals: {
      localStorage: {
        getItem(key) {
          if (blocked) throw new Error('Storage denied');
          return stored.get(key) ?? null;
        },
        setItem(key, value) {
          if (blocked) throw new Error('Storage denied');
          stored.set(key, value);
        },
      },
    },
  });
  assert.equal(api.loadScreenPresentationMode(), 'detail');
  api.saveScreenPresentationMode('motion');
  assert.equal(api.loadScreenPresentationMode(), 'motion');
  blocked = true;
  api.saveScreenPresentationMode('detail');
  assert.equal(api.loadScreenPresentationMode(), 'detail');
  api.saveScreenPresentationMode('motion');
  assert.equal(api.loadScreenPresentationMode(), 'motion');
});

test('VP8 and H264 presentation ladders reserve camera and both audio sources below 3 Mbps', async () => {
  const api = await loadTypeScript('src/screen-presentation.ts');
  for (const mime of ['video/VP8', 'video/H264']) {
    for (const mode of ['detail', 'motion']) {
      const { encodings } = api.screenEncodingOptions(mode, codecs(mime), true);
      assert.equal(encodings.length, 3);
      assert.deepEqual(
        encodings.map((encoding) => encoding.scaleResolutionDownBy),
        [4, 2, 1],
      );
      assert.equal(
        encodings.reduce((sum, encoding) => sum + encoding.maxBitrate, 0),
        api.SCREEN_VIDEO_BITRATE,
      );
      assert.ok(
        api.SCREEN_VIDEO_BITRATE + 400_000 + 2 * api.PRESENTATION_AUDIO_BITRATE < 3_000_000,
      );
      assert.ok(encodings.every((encoding) => !('rid' in encoding) && !('active' in encoding)));
      assert.ok(
        encodings.every((encoding) => encoding.maxFramerate === (mode === 'motion' ? 30 : 15)),
      );
      assert.equal(api.screenContentHint(mode), mode);
      assert.deepEqual(api.screenCaptureConstraints(mode), {
        video: {
          width: { max: 1920 },
          height: { max: 1080 },
          frameRate: { ideal: mode === 'motion' ? 30 : 15, max: mode === 'motion' ? 30 : 15 },
        },
        audio: true,
      });
    }
  }
});

test('a storage write quota failure preserves the explicit mode even when reads still work', async () => {
  const api = await loadTypeScript('src/screen-presentation.ts', {
    globals: {
      localStorage: {
        getItem: () => null,
        setItem() {
          throw new Error('Quota exceeded');
        },
      },
    },
  });
  api.saveScreenPresentationMode('motion');
  assert.equal(api.loadScreenPresentationMode(), 'motion');
});

test('VP9 spatial scalability requires an affirmative smooth encoder capability query', async () => {
  const queries = [];
  let result = { supported: true, smooth: true };
  const api = await loadTypeScript('src/screen-presentation.ts', {
    globals: {
      navigator: {
        mediaCapabilities: {
          async encodingInfo(query) {
            queries.push(query);
            if (result instanceof Error) throw result;
            return result;
          },
        },
      },
    },
  });
  assert.equal(await api.supportsVp9ScreenSvc(codecs('video/VP8'), 'detail'), false);
  assert.equal(queries.length, 0, 'an available later VP9 codec never changes the default codec');
  assert.equal(await api.supportsVp9ScreenSvc(codecs('video/VP9'), 'detail'), true);
  assert.equal(queries[0].type, 'webrtc');
  assert.equal(queries[0].video.scalabilityMode, 'L3T3_KEY');
  assert.equal(
    api.screenEncodingOptions('detail', codecs('video/VP9'), true).encodings[0].scalabilityMode,
    'L3T3_KEY',
  );
  for (const capability of [
    { supported: false, smooth: true },
    { supported: true, smooth: false },
    new Error('No WebRTC query support'),
  ]) {
    result = capability;
    assert.equal(await api.supportsVp9ScreenSvc(codecs('video/VP9'), 'motion'), false);
    const { encodings } = api.screenEncodingOptions('motion', codecs('video/VP9'), false);
    assert.equal(encodings.length, 1);
    assert.equal(encodings[0].maxBitrate, api.SCREEN_VIDEO_BITRATE);
    assert.equal(encodings[0].scalabilityMode, undefined);
  }
  assert.equal(
    api.screenEncodingOptions('detail', codecs('video/AV1'), true).encodings[0].scalabilityMode,
    undefined,
  );
});

test('a hung VP9 capability query times out without capture or forced codec selection', async () => {
  let expire, cleared;
  const api = await loadTypeScript('src/screen-presentation.ts', {
    globals: {
      navigator: { mediaCapabilities: { encodingInfo: () => new Promise(() => {}) } },
      setTimeout(callback, milliseconds) {
        assert.equal(milliseconds, 1000);
        expire = callback;
        return 17;
      },
      clearTimeout(timer) {
        cleared = timer;
      },
    },
  });
  const supported = api.supportsVp9ScreenSvc(codecs('video/VP9'), 'detail');
  expire();
  assert.equal(await supported, false);
  assert.equal(cleared, 17);
});
