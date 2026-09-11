import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const stylesheet = await readFile(new URL('../src/style.css', import.meta.url), 'utf8');
const colors = new Map(
  [...stylesheet.matchAll(/^\s*(--[\w-]+):\s*(#[\da-f]{3,6});/gm)].map((match) => [
    match[1],
    match[2],
  ]),
);

function luminance(token) {
  const color = colors.get(token);
  assert.ok(color, `missing opaque color token ${token}`);
  assert.match(color, /^#(?:[\da-f]{3}|[\da-f]{6})$/);
  const hex = color.length === 4 ? [...color.slice(1)].map((n) => n + n).join('') : color.slice(1);
  const channels = hex.match(/../g).map((n) => {
    const value = Number.parseInt(n, 16) / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  });
  return channels[0] * 0.2126 + channels[1] * 0.7152 + channels[2] * 0.0722;
}

function checkPair(foreground, background) {
  const first = luminance(foreground);
  const second = luminance(background);
  const ratio = (Math.max(first, second) + 0.05) / (Math.min(first, second) + 0.05);
  assert.ok(ratio >= 4.5, `${foreground} on ${background}: ${ratio}:1 is below 4.5:1`);
}

// Source-token regressions complement, not replace, the rendered axe checks.
// Normal-text threshold/formula: https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html
test('text, secondary text and links have normal-text contrast on every opaque theme surface', () => {
  for (const background of ['--bg', '--surface', '--surface-2', '--surface-3', '--border']) {
    for (const foreground of ['--text', '--text-dim', '--accent', '--accent-hover', '--danger']) {
      checkPair(foreground, background);
    }
  }
});

test('filled primary controls retain readable text in normal and hover states', () => {
  checkPair('--on-accent', '--accent-fill');
  checkPair('--on-accent', '--accent-fill-hover');
});

test('connection and unread status colors retain readable text in every state', () => {
  for (const background of ['--danger', '--success', '--warning']) {
    checkPair('--status-ink', background);
  }
});

test('primary fills stay separate from link-text colors in both shared stylesheets', async () => {
  const settings = await readFile(new URL('../src/settings-dialog.css', import.meta.url), 'utf8');
  for (const css of [stylesheet, settings]) {
    assert.doesNotMatch(css, /background:\s*var\(--accent(?:,|\))/);
    assert.doesNotMatch(css, /background:\s*var\(--accent-hover(?:,|\))/);
    assert.match(css, /background:\s*var\(--accent-fill\)/);
    assert.match(css, /color:\s*var\(--on-accent\)/);
  }
});
