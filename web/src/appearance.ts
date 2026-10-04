import type { ChatStyle, ChatStyleKind } from './protocol';
import { CHAT_PALETTE, chatColor } from './avatar-colors';
import { el } from './ui';
import './appearance.css';

const STYLES: readonly { kind: ChatStyleKind; label: string }[] = [
  { kind: 'accent', label: 'Accent' },
  { kind: 'text', label: 'Colored text' },
  { kind: 'bubble', label: 'Tinted background' },
];
let pickerId = 0;

/** Palette-only appearance shared by profile cards and room headings, separate from chat. */
export function applyAppearance(
  node: HTMLElement,
  look: ChatStyle | null | undefined,
  name: string,
): void {
  const color = chatColor(name, look?.color);
  node.classList.add('appearance-custom');
  node.dataset['appearance'] = STYLES.some(({ kind }) => kind === look?.style)
    ? look!.style
    : 'accent';
  node.style.setProperty('--appearance-color', color);
  node.style.setProperty('--appearance-tint', `${color}24`);
}

/** Accessible radio groups with a live preview; callers persist their own chosen value. */
export function appearancePicker(options: {
  label: string;
  initial?: ChatStyle | null;
  name: () => string;
  description?: string;
}): { element: HTMLElement; chosen: () => ChatStyle; refreshPreview: () => void } {
  const id = `appearance-${++pickerId}`;
  const current = options.initial ?? { color: null, style: 'accent' };
  const element = el('section', undefined, 'appearance-picker');
  element.append(el('h3', options.label));
  if (options.description) element.append(el('p', options.description, 'setting-hint'));
  const preview = el('div', undefined, 'appearance-preview');
  const radio = (group: string, value: string, checked: boolean): HTMLInputElement => {
    const node = el('input');
    node.type = 'radio';
    node.name = `${id}-${group}`;
    node.value = value;
    node.checked = checked;
    node.addEventListener('change', () => refreshPreview());
    return node;
  };
  const swatch = (value: string, color: string): HTMLInputElement => {
    const node = radio('color', value, (current.color ?? '') === value);
    node.className = 'chat-swatch';
    node.style.setProperty('--swatch', color);
    return node;
  };
  const automatic = swatch('', chatColor(options.name(), null));
  const automaticLabel = el('label', undefined, 'chat-swatch-automatic');
  automaticLabel.append(automatic, el('span', 'Automatic'));
  const palette = el('div', undefined, 'chat-palette');
  const swatches = el('div', undefined, 'chat-swatches');
  swatches.append(automaticLabel, palette);
  const colors = [automatic];
  for (const [token, hex] of Object.entries(CHAT_PALETTE)) {
    const node = swatch(token, hex);
    const name = token.charAt(0).toUpperCase() + token.slice(1);
    node.title = name;
    node.setAttribute('aria-label', name);
    colors.push(node);
    palette.append(node);
  }
  const colorGroup = el('fieldset', undefined, 'chat-look-group');
  colorGroup.append(el('legend', 'Color'), swatches);
  const styles = STYLES.map(({ kind, label }) => {
    const node = radio('style', kind, current.style === kind);
    const wrapper = el('label', undefined, 'chat-look-choice');
    wrapper.append(node, el('span', label));
    return { node, kind, wrapper };
  });
  const styleGroup = el('fieldset', undefined, 'chat-look-group');
  styleGroup.append(el('legend', 'Style'), ...styles.map(({ wrapper }) => wrapper));
  const chosen = (): ChatStyle => ({
    color: colors.find((node) => node.checked)?.value || null,
    style: styles.find(({ node }) => node.checked)?.kind ?? 'accent',
  });
  const refreshPreview = (): void => {
    const name = options.name();
    preview.textContent = name || 'Preview';
    automatic.style.setProperty('--swatch', chatColor(name, null));
    applyAppearance(preview, chosen(), name);
  };
  refreshPreview();
  element.append(colorGroup, styleGroup, preview);
  return { element, chosen, refreshPreview };
}
