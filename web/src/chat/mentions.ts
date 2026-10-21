import { createScope } from '../disposable';
import type { ChatStyle } from '../protocol';
import type { RoomClient } from '../room';
import { el } from '../ui';
import { chatColor } from '../avatar-colors';

interface MentionContext {
  readonly input: HTMLTextAreaElement;
  getRoom: () => RoomClient | null;
  insertText: (text: string, start: number, end: number) => void;
}

/** Caret-local autocomplete; it changes the draft only after an explicit choice. */
export class ChatMentions {
  private readonly context: MentionContext;

  readonly mentionList = el('ul', undefined, 'mention-list');
  private mentionOptions: { name: string; chatStyle?: ChatStyle }[] = [];
  private mentionActive = 0;
  private readonly events = createScope();

  constructor(context: MentionContext) {
    this.context = context;

    this.mentionList.id = 'mention-list';
    this.mentionList.setAttribute('role', 'listbox');
    this.mentionList.setAttribute('aria-label', 'People to mention');
    this.mentionList.hidden = true;
    // Choosing with the pointer leaves focus in the message box.
    this.mentionList.addEventListener('mousedown', (event) => event.preventDefault(), {
      signal: this.events.signal,
    });
    context.input.setAttribute('aria-autocomplete', 'list');
    context.input.setAttribute('aria-controls', 'mention-list');
  }

  /** Returns whether completion consumed the key before composer shortcuts run. */
  handleKey(event: KeyboardEvent, sendKey: boolean): boolean {
    if (event.key === 'Tab' && !this.mentionOptions.length) this.suggestMentions();
    if (!this.mentionOptions.length) return false;
    const count = this.mentionOptions.length;
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      this.mentionActive =
        (this.mentionActive + (event.key === 'ArrowDown' ? 1 : count - 1)) % count;
      this.renderMentions();
      return true;
    }
    if ((sendKey && !event.ctrlKey && !event.metaKey) || event.key === 'Tab') {
      event.preventDefault();
      this.chooseMention(this.mentionActive);
      return true;
    }
    if (event.key === 'Escape') {
      event.preventDefault();
      event.stopPropagation();
      this.closeMentions();
      return true;
    }
    return false;
  }

  dispose(): void {
    this.events.dispose();
    this.closeMentions();
    this.mentionList.remove();
  }

  /** The "@name" being typed just before the caret, if any. */
  mentionQuery(): { query: string; start: number; end: number } | null {
    const end = this.context.input.selectionStart ?? this.context.input.value.length;
    const match = /(?:^|\s)@([^@\s]*)$/.exec(this.context.input.value.slice(0, end));
    return match ? { query: match[1]!, start: end - match[1]!.length - 1, end } : null;
  }

  /** Typing "@" and part of a name offers the people it could mean, best matches first. */
  suggestMentions(): void {
    const typed = this.mentionQuery()?.query.toLowerCase();
    const people =
      typed === undefined ? [] : [...(this.context.getRoom()?.getParticipants().values() ?? [])];
    const name = (person: { name: string }): string => person.name.toLowerCase();
    const starts = people.filter((person) => name(person).startsWith(typed!));
    const contains = people.filter(
      (person) => !starts.includes(person) && name(person).includes(typed!),
    );
    this.mentionOptions = [...starts, ...contains].slice(0, 6);
    this.mentionActive = 0;
    this.renderMentions();
  }

  renderMentions(): void {
    const open = this.mentionOptions.length > 0;
    this.mentionList.hidden = !open;
    this.mentionList.replaceChildren(
      ...this.mentionOptions.map((person, index) => {
        const option = el('li');
        option.id = `mention-option-${index}`;
        option.setAttribute('role', 'option');
        option.setAttribute('aria-selected', String(index === this.mentionActive));
        const dot = el('span', undefined, 'mention-dot');
        dot.style.setProperty('--person-color', chatColor(person.name, person.chatStyle?.color));
        option.append(dot, el('span', person.name));
        option.addEventListener('click', () => this.chooseMention(index));
        return option;
      }),
    );
    if (open)
      this.context.input.setAttribute(
        'aria-activedescendant',
        `mention-option-${this.mentionActive}`,
      );
    else this.context.input.removeAttribute('aria-activedescendant');
  }

  chooseMention(index: number): void {
    const person = this.mentionOptions[index];
    const query = this.mentionQuery();
    this.closeMentions();
    if (person && query) this.context.insertText(`@${person.name} `, query.start, query.end);
  }

  closeMentions(): void {
    if (!this.mentionOptions.length) return;
    this.mentionOptions = [];
    this.renderMentions();
  }
}
