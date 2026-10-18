import assert from 'node:assert/strict';
import test from 'node:test';
import pattern from '../e2e/presentation-pattern.cjs';

function frame(width, height, changed = pattern.text) {
  const pixels = new Uint8ClampedArray(width * height * 4).fill(255);
  for (let character = 0; character < changed.length; character++)
    for (let row = 0; row < 7; row++)
      for (let column = 0; column < 5; column++) {
        const x = Math.floor(((144 + character * 144 + (column + 0.5) * 24) * width) / 1920);
        const y = Math.floor(((144 + (row + 0.5) * 24) * height) / 1080);
        const offset = (y * width + x) * 4;
        const value = pattern.glyphs[changed[character]][row][column] === '1' ? 24 : 232;
        pixels.fill(value, offset, offset + 3);
      }
  return { pixels, width, height, glyphs: pattern.glyphs, text: pattern.text };
}

test('presentation reader recognizes actual glyph pixels at all three screen layer sizes', () => {
  for (const [width, height] of [
    [1920, 1080],
    [960, 540],
    [480, 270],
  ]) {
    const read = pattern.decodePresentationFrame(frame(width, height));
    assert.equal(read.text, pattern.text);
    assert.equal(read.contrast, 208);
    assert.equal(read.height, height);
  }
});

test('presentation reader rejects blank video and distinguishes changed decoded text', () => {
  const source = frame(480, 270);
  assert.equal(
    pattern.decodePresentationFrame({
      ...source,
      pixels: new Uint8ClampedArray(source.pixels.length).fill(255),
    }).text,
    '',
  );
  assert.equal(pattern.decodePresentationFrame(frame(480, 270, 'SLIDES 4321')).text, 'SLIDES 4321');
});
