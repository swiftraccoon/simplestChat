import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import ts from '@typescript/typescript6';
import { evaluateTypeScript, loadTypeScript } from './source-loader.mjs';

const { avatarColors } = await loadTypeScript('src/avatar-colors.ts');

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

test('all four initial renderers apply both colors from the shared helper', async () => {
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
  assert.equal(assignments.length, 4);
  assert.doesNotMatch(source, /\bnameColor\b/);
  for (const assignment of assignments) {
    const initial = { style: {} };
    const avatar = { style: {} };
    evaluateTypeScript(assignment, {
      globals: {
        avatarColors,
        initial,
        avatar,
        name: 'Accessibility Owner',
        participantName: 'Accessibility Owner',
        p: { name: 'Accessibility Owner' },
      },
    });
    const applied = Object.keys(initial.style).length ? initial.style : avatar.style;
    assert.deepEqual(applied, avatarColors('Accessibility Owner'));
  }
});
