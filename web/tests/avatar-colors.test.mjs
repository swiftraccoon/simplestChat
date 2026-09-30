import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import ts from '@typescript/typescript6';
import { evaluateTypeScript, loadTypeScript } from './source-loader.mjs';

const { avatarColors, chatColor, CHAT_PALETTE } = await loadTypeScript('src/avatar-colors.ts');

// Independently convert HSL using the channel-offset formula, then check the
// emitted CSS pair. Source checks complement the rendered accessibility scans.
function backgroundLuminance(hue, roundChannels = false) {
  const channel = (offset) => {
    const sector = (offset + hue / 30) % 12;
    let value = 0.55 - 0.65 * 0.45 * Math.max(-1, Math.min(sector - 3, 9 - sector, 1));
    if (roundChannels) value = Math.round(value * 255) / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(0) + 0.7152 * channel(8) + 0.0722 * channel(4);
}

test('every possible avatar hue selects the higher-contrast black or white initial', () => {
  const foregrounds = new Set();
  for (let hue = 0; hue < 360; hue++) {
    // A single UTF-16 code unit in this range hashes directly to the desired hue.
    const colors = avatarColors(String.fromCharCode(360 + hue));
    assert.equal(colors.background, `hsl(${hue}, 65%, 55%)`);
    const luminance = backgroundLuminance(hue);
    const contrasts = { '#000': (luminance + 0.05) / 0.05, '#fff': 1.05 / (luminance + 0.05) };
    assert.equal(colors.color, contrasts['#000'] >= contrasts['#fff'] ? '#000' : '#fff');
    assert.ok(contrasts[colors.color] >= 4.5, `hue ${hue} must meet normal-text contrast`);
    const roundedLuminance = backgroundLuminance(hue, true);
    const roundedContrast =
      colors.color === '#000' ? (roundedLuminance + 0.05) / 0.05 : 1.05 / (roundedLuminance + 0.05);
    assert.ok(roundedContrast >= 4.5, `hue ${hue} remains legible after 8-bit channel rounding`);
    foregrounds.add(colors.color);
  }
  assert.deepEqual(foregrounds, new Set(['#000', '#fff']));
});

test('existing name colors remain deterministic across Unicode and hash overflow', () => {
  for (const [name, hue] of [
    ['', 0],
    ['Accessibility Owner', 17],
    ['Alice', 88],
    ['alice', 0],
    ['Bob', 5],
    ['Zoë', 166],
    ['é', 233],
    ['e\u0301', 300],
    ['李雷', 233],
    ['🙂', 325],
    ['👩🏽‍💻', 250],
    ['\ud83d', 277],
    ['a'.repeat(1000), 16],
  ]) {
    const first = avatarColors(name);
    assert.equal(first.background, `hsl(${hue}, 65%, 55%)`);
    assert.deepEqual(avatarColors(name), first);
    assert.notEqual(avatarColors(name), first, 'calls do not share mutable result objects');
  }
});

test('the reviewed low-contrast initial switches foreground without changing its background', () => {
  assert.deepEqual(avatarColors('Accessibility Owner'), {
    background: 'hsl(17, 65%, 55%)',
    color: '#000',
  });
});

test('every initial renderer applies both colors, and a chosen color, from the shared helper', async () => {
  const source = await readFile(new URL('../src/main.ts', import.meta.url), 'utf8');
  const ast = ts.createSourceFile('main.ts', source, ts.ScriptTarget.Latest, true);
  const assignments = [];
  const visit = (node) => {
    if (
      ts.isCallExpression(node) &&
      node.expression.getText(ast) === 'Object.assign' &&
      node.arguments.length === 2 &&
      ts.isCallExpression(node.arguments[1]) &&
      node.arguments[1].expression.getText(ast) === 'avatarColors'
    ) {
      assignments.push(node.getText(ast));
    }
    ts.forEachChild(node, visit);
  };
  visit(ast);
  // Both people lists share one renderer; every tile uses paintTile.
  assert.equal(assignments.length, 2);
  assert.doesNotMatch(source, /\bnameColor\b/);
  for (const assignment of assignments) {
    for (const chosen of [null, 'violet']) {
      const initial = { style: {} };
      const avatar = { style: {} };
      evaluateTypeScript(assignment, {
        globals: {
          avatarColors,
          initial,
          avatar,
          name: 'Accessibility Owner',
          color: chosen,
          p: { name: 'Accessibility Owner', ...(chosen && { chatStyle: { color: chosen } }) },
        },
      });
      const applied = Object.keys(initial.style).length ? initial.style : avatar.style;
      assert.deepEqual(applied, avatarColors('Accessibility Owner', chosen), assignment);
    }
  }
  assert.equal(avatarColors('Accessibility Owner', 'violet').background, CHAT_PALETTE.violet);
});

/** Relative luminance of a #rrggbb color. */
function hexLuminance(hex) {
  const [red, green, blue] = [1, 3, 5].map((start) => {
    const value = parseInt(hex.slice(start, start + 2), 16) / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
}
const contrast = (first, second) => {
  const [light, dark] = [hexLuminance(first), hexLuminance(second)].sort((a, b) => b - a);
  return (light + 0.05) / (dark + 0.05);
};
function hexHue(hex) {
  const [red, green, blue] = [1, 3, 5].map((start) => parseInt(hex.slice(start, start + 2), 16));
  const max = Math.max(red, green, blue);
  const range = max - Math.min(red, green, blue);
  const hue =
    max === red
      ? (green - blue) / range
      : max === green
        ? 2 + (blue - red) / range
        : 4 + (red - green) / range;
  return (hue * 60 + 360) % 360;
}

test('the palette is the server palette, in its order', async () => {
  const rust = await readFile(new URL('../../src/signaling/protocol.rs', import.meta.url), 'utf8');
  const list = /pub const CHAT_COLORS: \[&str; 16\] = \[([^\]]*)\]/.exec(rust)?.[1];
  assert.ok(list, 'protocol.rs declares CHAT_COLORS');
  assert.deepEqual(
    Object.keys(CHAT_PALETTE),
    [...list.matchAll(/"([a-z]+)"/g)].map((match) => match[1]),
  );
});

test('every palette color stays readable as a name or message on the chat surfaces', () => {
  for (const [token, hex] of Object.entries(CHAT_PALETTE)) {
    // --surface-2 (message bubbles) and --surface (people list).
    for (const surface of ['#252525', '#1a1a1a'])
      assert.ok(contrast(hex, surface) >= 4.5, `${token} on ${surface}`);
  }
});

test('a chosen color wins; otherwise the palette hue nearest the avatar identifies them', () => {
  const tokens = Object.keys(CHAT_PALETTE);
  for (let hue = 0; hue < 360; hue++) {
    const name = String.fromCharCode(360 + hue);
    let nearest = tokens[0];
    let distance = Infinity;
    for (const token of tokens) {
      const gap = Math.abs(hexHue(CHAT_PALETTE[token]) - hue);
      const circular = Math.min(gap, 360 - gap);
      if (circular < distance) [nearest, distance] = [token, circular];
    }
    assert.equal(chatColor(name, null), CHAT_PALETTE[nearest], `hue ${hue}`);
    assert.equal(chatColor(name), CHAT_PALETTE[nearest]);
  }
  assert.equal(chatColor('Alice', 'violet'), CHAT_PALETTE.violet);
  // A color from a newer palette, or anything else, falls back to the automatic one.
  assert.equal(chatColor('Alice', 'mauve'), chatColor('Alice', null));
  assert.equal(chatColor('Alice', 'constructor'), chatColor('Alice', null));
});

test('a chosen color also fills the avatar, with a legible initial', () => {
  for (const [token, hex] of Object.entries(CHAT_PALETTE)) {
    const colors = avatarColors('Alice', token);
    assert.equal(colors.background, hex);
    const ink = colors.color === '#000' ? '#000000' : '#ffffff';
    assert.ok(contrast(hex, ink) >= 4.5, `${token} initial`);
  }
  assert.deepEqual(avatarColors('Alice', null), avatarColors('Alice'));
  assert.deepEqual(avatarColors('Alice', 'mauve'), avatarColors('Alice'));
});
