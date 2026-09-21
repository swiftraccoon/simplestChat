import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

const { spatialLayerForRenderedWidth, LAYER_WIDTHS } = await loadTypeScript('src/layer-cap.ts');

test('a fresh tile takes the smallest layer that covers its device pixels', () => {
  assert.deepEqual(LAYER_WIDTHS, [320, 640, 1280]);
  assert.equal(spatialLayerForRenderedWidth(200, 1, null), 0);
  assert.equal(spatialLayerForRenderedWidth(200, 2, null), 1, 'retina doubles the need');
  assert.equal(spatialLayerForRenderedWidth(640, 1, null), 1);
  assert.equal(spatialLayerForRenderedWidth(641, 1, null), 2);
  assert.equal(spatialLayerForRenderedWidth(4000, 1, null), 2, 'never above the top layer');
  assert.equal(spatialLayerForRenderedWidth(0, 0, null), 0, 'degenerate sizes stay lowest');
});

test('hysteresis keeps a tile near a boundary on its current layer', () => {
  // At layer 1 (640 px) a tile must exceed 736 px before stepping up...
  assert.equal(spatialLayerForRenderedWidth(700, 1, 1), 1);
  assert.equal(spatialLayerForRenderedWidth(737, 1, 1), 2);
  // ...and shrink below 272 px (85% of the 320 px layer) before stepping down.
  assert.equal(spatialLayerForRenderedWidth(300, 1, 1), 1);
  assert.equal(spatialLayerForRenderedWidth(271, 1, 1), 0);
  // A jump of two layers is taken directly.
  assert.equal(spatialLayerForRenderedWidth(1400, 1, 0), 2);
  assert.equal(spatialLayerForRenderedWidth(100, 1, 2), 0);
  assert.equal(spatialLayerForRenderedWidth(500, 1, 7), 1, 'an unknown current layer resets');
});
