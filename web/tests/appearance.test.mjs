import assert from 'node:assert/strict';
import test from 'node:test';
import { uiFixture } from './ui-fixture.mjs';
import { loadAppearanceFixture } from './appearance-fixture.mjs';

test('independent appearance pickers preserve each other and update the preview', async () => {
  const dom = await uiFixture();
  const { appearancePicker } = await loadAppearanceFixture(dom);
  let name = 'Room name';
  const room = appearancePicker({
    label: 'Room appearance',
    initial: { color: 'teal', style: 'bubble' },
    name: () => name,
  });
  const profile = appearancePicker({ label: 'Profile appearance', name: () => 'Alice' });
  const roomRadios = room.element.querySelectorAll('input');
  const profileRadios = profile.element.querySelectorAll('input');
  assert.equal(
    profileRadios.some((radio) => radio.name === roomRadios[0].name),
    false,
  );
  assert.deepEqual(room.chosen(), { color: 'teal', style: 'bubble' });
  assert.deepEqual(profile.chosen(), { color: null, style: 'accent' });
  for (const radio of roomRadios)
    radio.checked = radio.name.endsWith('-color')
      ? radio.value === 'violet'
      : radio.value === 'text';
  name = '<b>Current room name</b>';
  room.refreshPreview();
  const preview = room.element.querySelector('.appearance-preview');
  assert.equal(preview.textContent, '<b>Current room name</b>');
  assert.equal(preview.dataset.appearance, 'text');
  assert.equal(preview.style['--appearance-color'], '#a78bfa');
  assert.deepEqual(profile.chosen(), { color: null, style: 'accent' });
});

test('appearance rendering accepts only palette colors and known style kinds', async () => {
  const dom = await uiFixture();
  const { applyAppearance } = await loadAppearanceFixture(dom);
  const content = dom.ui.el('div');
  applyAppearance(content, { color: 'red', style: 'bubble' }, 'Alice');
  assert.equal(content.dataset.appearance, 'bubble');
  assert.equal(content.style['--appearance-color'], '#f87171');
  applyAppearance(content, { color: 'url(https://example.invalid)', style: 'invalid' }, 'Alice');
  assert.equal(content.dataset.appearance, 'accent');
  assert.match(content.style['--appearance-color'], /^#[a-f0-9]{6}$/);
  assert.equal(content.style['--appearance-tint'], `${content.style['--appearance-color']}24`);
});
