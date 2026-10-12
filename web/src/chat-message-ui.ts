import type { ChatEntry } from './protocol';
import { asyncButton, button, el, field, modal } from './ui';

/** Links stay whole; outside them, each occurrence of `mention` is marked, in any case. */
export function appendLinkedText(parent: HTMLElement, text: string, mention?: string): void {
  const pattern = /https?:\/\/[^\s<>]+/g;
  // Default-ignorable characters (the server's labels::is_hidden_format set:
  // bidi controls, zero-width space, word joiner, soft hyphen, fillers, the BOM
  // and unassigned format characters) can make a link read as another.
  // The combining grapheme joiner sits outside the class: a combining mark in
  // a class reads as a sequence with its neighbour.
  const hidden =
    /\u034F|[\u00AD\u115F\u1160\u17B4\u17B5\u200B\u2028\u2029\u202A-\u202E\u2060-\u206F\u3164\uFEFF\uFFA0\uFFF0-\uFFF8\u{1BCA0}-\u{1BCA3}\u{1D173}-\u{1D17A}\u{E0000}-\u{E001F}\u{E0080}-\u{E0FFF}]/u;
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
