import type { ChatEntry } from './protocol';
import { asyncButton, button, el, field, modal } from './ui';

/** Links stay whole; outside them, each occurrence of `mention` is marked, in any case. */
export function appendLinkedText(parent: HTMLElement, text: string, mention?: string): void {
  const pattern = /https?:\/\/[^\s<>]+/g;
  // Bidi controls, zero-width space and the BOM can make a link read as another.
  const hidden = /[\u200B\u202A-\u202E\u2066-\u2069\u2028\u2029\uFEFF]/;
  let start = 0;
  for (const match of text.matchAll(pattern)) {
    const index = match.index;
    if (hidden.test(match[0])) continue;
    appendMarkedText(parent, text.slice(start, index), mention);
    const link = el('a', match[0]);
    link.href = match[0];
    link.target = '_blank';
    link.rel = 'noopener noreferrer';
    parent.append(link);
    start = index + match[0].length;
  }
  appendMarkedText(parent, text.slice(start), mention);
}

function appendMarkedText(parent: HTMLElement, text: string, mention: string | undefined): void {
  if (!mention) {
    parent.append(document.createTextNode(text));
    return;
  }
  // Match on the original text: lowercasing can change a string's length.
  const pattern = new RegExp(mention.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'giu');
  let start = 0;
  for (const match of text.matchAll(pattern)) {
    parent.append(document.createTextNode(text.slice(start, match.index)));
    parent.append(el('mark', match[0], 'mention'));
    start = match.index + match[0].length;
  }
  parent.append(document.createTextNode(text.slice(start)));
}

/** Shared edit affordance for live chat and retained messages. */
export function editMessageDialog(
  message: ChatEntry,
  options: {
    current: () => boolean;
    save: (content: string, revision: number) => Promise<ChatEntry>;
    changed: (message: ChatEntry) => void;
  },
): void {
  const view = modal('Edit message');
  const revision = message.revision;
  const composer = el('textarea');
  composer.rows = 3;
  composer.maxLength = 2000;
  composer.value = message.content;
  const save = asyncButton(
    'Save changes',
    async () => {
      const content = composer.value.trim();
      if (!content || save.disabled || !view.dialog.open || !options.current()) return;
      save.disabled = true;
      try {
        const result = await options.save(content, revision);
        if (!view.dialog.open || !options.current()) return;
        options.changed(result);
        view.close();
      } finally {
        save.disabled = false;
      }
    },
    (error) => {
      if (!view.dialog.open || !options.current()) return;
      view.error.textContent =
        error instanceof Error ? error.message : 'Message could not be edited';
      view.error.hidden = false;
    },
  );
  composer.addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && !event.isComposing && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      save.click();
    }
  });
  view.body.append(field('Message', composer), button('Cancel', view.close), save);
}

export function appendEditedLabel(node: HTMLElement, message: ChatEntry): void {
  if (!message.editedAt || message.removedAt) return;
  const label = el('span', 'edited', 'msg-edited');
  label.title = `Edited ${new Date(message.editedAt).toLocaleString()}`;
  node.append(label);
}
