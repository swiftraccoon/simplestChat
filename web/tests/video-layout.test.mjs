import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

const { calculateVideoLayout } = await loadTypeScript('src/video-layout.ts');
const landscape = 16 / 9;
const portrait = 9 / 16;

function packedHeight(sizes, width, gap = 8) {
  let occupied = 0;
  let rows = 1;
  for (const size of sizes) {
    assert.ok(size.width <= width, 'no tile exceeds the stage width');
    if (occupied && occupied + gap + size.width > width) {
      rows++;
      occupied = size.width;
    } else occupied += (occupied ? gap : 0) + size.width;
  }
  return rows * sizes[0].height + (rows - 1) * gap;
}

test('three cameras and portrait media fit without cropping or an extra sparse row', () => {
  const ratios = [landscape, landscape, landscape, portrait];
  const sizes = calculateVideoLayout(ratios, 900, 787);
  assert.ok(sizes[0].height > 240);
  assert.ok(sizes[0].height < 251, 'two useful rows win over three slightly larger rows');
  assert.ok(packedHeight(sizes, 900) <= 787);
  sizes.forEach((size, index) => {
    assert.equal(size.height, sizes[0].height);
    assert.ok(Math.abs(size.width / size.height - ratios[index]) < 1e-10);
  });
});

test('phone layout fits four mixed videos and treats equal camera/share aspects identically', () => {
  const sizes = calculateVideoLayout([landscape, landscape, portrait, portrait], 358, 340);
  assert.ok(packedHeight(sizes, 358) <= 340);
  assert.deepEqual(sizes[2], sizes[3]);
  assert.ok(sizes[0].height >= 96);
});

test('crowded rooms retain readable rows for scrolling instead of shrinking every tile', () => {
  const sizes = calculateVideoLayout(Array(13).fill(landscape), 358, 280);
  assert.equal(sizes[0].height, 96);
  assert.ok(sizes[0].width * 2 + 8 <= 358, 'crowded phone rows still fit two cameras');
  assert.ok(packedHeight(sizes, 358) > 280);
  const narrow = calculateVideoLayout([landscape, portrait], 120, 300);
  assert.ok(narrow.every((size) => size.width <= 120));
  assert.ok(packedHeight(narrow, 120) <= 300);
});

test('a pinned portrait keeps its narrow aspect and leaves height for the remaining row', () => {
  const ratios = [landscape, portrait, landscape];
  const sizes = calculateVideoLayout(ratios, 900, 700, 8, 1);
  assert.ok(sizes[1].height > sizes[0].height);
  assert.ok(Math.abs(sizes[1].width / sizes[1].height - portrait) < 1e-10);
  assert.ok(sizes[1].height + 8 + packedHeight([sizes[0], sizes[2]], 900) <= 700);
  const unpinned = calculateVideoLayout(ratios, 900, 700);
  assert.equal(unpinned[0].height, unpinned[1].height);
});

test('metadata arrival and rotation change aspect while unavailable metadata uses 16:9', () => {
  const pending = calculateVideoLayout([0, Number.NaN], 600, 400);
  assert.deepEqual(pending[0], pending[1]);
  assert.ok(Math.abs(pending[0].width / pending[0].height - landscape) < 1e-10);
  const rotated = calculateVideoLayout([portrait, landscape], 600, 400);
  assert.equal(rotated[0].height, rotated[1].height);
  assert.ok(rotated[0].width < rotated[1].width);
  assert.deepEqual(calculateVideoLayout([], 600, 400), []);
  assert.deepEqual(calculateVideoLayout([landscape], 0, 400), [{ width: 0, height: 0 }]);
});
