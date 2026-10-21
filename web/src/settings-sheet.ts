import { button, el } from './ui';
import './settings-sheet.css';

/** What a screen may do while it is shown; `current()` turns false once it was left. */
export interface ScreenContext {
  push(screen: SheetScreen, opener?: HTMLElement | null): void;
  pop(): void;
  close(): void;
  current(): boolean;
}

export interface SheetScreen {
  title: string;
  subtitle?: string;
  /** Builds the body; a promise shows a placeholder until it settles. */
  body: (context: ScreenContext) => HTMLElement | Promise<HTMLElement>;
  /** Runs once when the screen is popped or the sheet closes. */
  leave?: () => void;
  /** False while the screen owns work that must finish first; leaving is then refused. */
  canLeave?: () => boolean;
}

export type SheetSegment = 'you' | 'room';

export interface SheetSection {
  id: string;
  segment: SheetSegment;
  label: string;
  /** Rows with the same group sit together under a small title. */
  group?: string;
  /** The current value, shown on the list row instead of a description. */
  status?: () => string;
  available?: () => boolean;
  screen: () => SheetScreen;
}

export interface SheetLink {
  label: string;
  activate: () => void;
  danger?: boolean;
}

export interface SettingsNavigation {
  section?: string;
  segment: SheetSegment;
  canLeave: () => boolean;
  restore: () => void;
}

export interface SettingsSheetOptions {
  sections: () => SheetSection[];
  footer?: () => SheetLink[];
  /** Where the sheet lives right now: the room's side column or the home dock. */
  host: () => HTMLElement;
  onOpen?: () => void;
  onClose?: () => void;
  onNavigate?: (navigation: SettingsNavigation | null, action: 'push' | 'pop') => void;
  onBack?: () => boolean;
  roomContext?: () => string;
  segmentLabels?: { you: string; room: string };
}

interface Frame {
  screen: SheetScreen;
  opener: HTMLElement | null;
  generation: number;
  body: HTMLElement | null;
}

const SAVED_MS = 1800;
const savedTimers = new WeakMap<HTMLElement, ReturnType<typeof setTimeout>>();
let labelIds = 0;

/** The row's label names its control by id, so the Saved mark beside it never joins the name. */
function rowLabel(text: string, control: HTMLElement): HTMLElement {
  const label = el('span', text, 'sheet-row-label');
  label.id = `sheet-label-${++labelIds}`;
  control.setAttribute('aria-labelledby', label.id);
  return label;
}

/** Whether focus can land on the node right now: attached and laid out. */
function focusable(node: HTMLElement | null): node is HTMLElement {
  return Boolean(
    node?.isConnected &&
    !node.hidden &&
    (typeof node.getClientRects !== 'function' || node.getClientRects().length > 0),
  );
}

/** One settings surface: a root list and a stack of single-purpose screens. */
export class SettingsSheet {
  private readonly options: SettingsSheetOptions;

  readonly element: HTMLElement;
  private readonly title: HTMLElement;
  private readonly back: HTMLButtonElement;
  private readonly body: HTMLElement;
  private readonly stack: Frame[] = [];
  private opener: HTMLElement | null = null;
  /** Where focus was when the sheet opened: the fallback when the opener is hidden by then. */
  private restoreTo: HTMLElement | null = null;
  private segment: SheetSegment = 'you';
  private generation = 0;
  private openState = false;
  private section: string | undefined;
  private restoringNavigation = false;

  constructor(options: SettingsSheetOptions) {
    this.options = options;

    this.element = el('section', undefined, 'settings-sheet');
    this.element.id = 'settings-sheet';
    this.element.setAttribute('role', 'region');
    this.element.setAttribute('aria-label', 'Settings');
    this.element.hidden = true;
    const header = el('div', undefined, 'sheet-header');
    this.back = button('', () => this.pop(), 'sheet-back');
    this.back.setAttribute('aria-label', 'Back');
    this.back.hidden = true;
    this.title = el('h2', 'Settings', 'sheet-title');
    this.title.id = 'settings-sheet-title';
    this.title.tabIndex = -1;
    const close = button('', () => this.close(), 'sheet-close');
    close.setAttribute('aria-label', 'Close settings');
    header.append(this.back, this.title, close);
    this.body = el('div', undefined, 'sheet-body');
    this.element.append(header, this.body);
    this.element.addEventListener('keydown', (event) => {
      if (event.key !== 'Escape') return;
      event.preventDefault();
      if (this.stack.length) this.pop();
      else this.close();
    });
  }

  get isOpen(): boolean {
    return this.openState;
  }

  /** Shows the root list, or one section's screen straight away. */
  open(
    options: { section?: string; segment?: SheetSegment; opener?: HTMLElement | null } = {},
  ): void {
    if (this.openState) {
      if (!this.canLeaveAll()) return;
      this.leaveAll();
    } else {
      this.opener = options.opener ?? null;
      this.restoreTo = document.activeElement as HTMLElement | null;
      this.openState = true;
    }
    const host = this.options.host();
    if (this.element.parentNode !== host) host.append(this.element);
    this.element.hidden = false;
    if (options.segment) this.segment = options.segment;
    this.section = undefined;
    this.renderRoot();
    this.options.onOpen?.();
    this.navigationChanged();
    this.title.focus();
    if (options.section) {
      const section = this.options
        .sections()
        .find((entry) => entry.id === options.section && (entry.available?.() ?? true));
      if (section) {
        if (section.segment !== this.segment) {
          this.segment = section.segment;
          this.renderRoot();
        }
        this.push(section.screen(), this.rowFor(section.id));
      }
    }
  }

  /** `force` drops screens that would otherwise wait, as an identity change must. */
  close(options: { force?: boolean } = {}): void {
    if (!this.openState || (!options.force && !this.canLeaveAll())) return;
    this.leaveAll();
    this.openState = false;
    this.element.hidden = true;
    this.body.replaceChildren();
    this.options.onClose?.();
    if (!this.restoringNavigation) this.options.onNavigate?.(null, 'pop');
    const opener = this.opener;
    const fallback = this.restoreTo;
    this.opener = null;
    this.restoreTo = null;
    if (focusable(opener)) opener.focus();
    else if (focusable(fallback)) fallback.focus();
  }

  /** Re-renders the root list so its status values are current. */
  refresh(): void {
    if (!this.openState || this.stack.length) return;
    const focused = this.focusedSection();
    this.renderRoot();
    if (focused) this.rowFor(focused)?.focus();
  }

  push(screen: SheetScreen, opener: HTMLElement | null = null): void {
    const frame: Frame = { screen, opener, generation: ++this.generation, body: null };
    if (!this.stack.length) this.section = opener?.dataset['section'];
    this.stack.push(frame);
    this.show(frame);
    this.title.focus();
    this.navigationChanged();
  }

  pop(): void {
    const top = this.stack[this.stack.length - 1];
    if (!top || top.screen.canLeave?.() === false) return;
    if (this.options.onBack?.()) return;
    const frame = this.stack.pop()!;
    this.generation++;
    frame.screen.leave?.();
    const parent = this.stack[this.stack.length - 1];
    if (parent) {
      this.show(parent);
      this.title.focus();
      this.navigationChanged('pop');
      return;
    }
    this.section = undefined;
    this.renderRoot();
    this.navigationChanged('pop');
    if (frame.opener?.isConnected) frame.opener.focus();
    else {
      const section = frame.opener?.dataset['section'];
      const row = section ? this.rowFor(section) : null;
      (row ?? this.title).focus();
    }
  }

  private navigationChanged(action: 'push' | 'pop' = 'push'): void {
    if (this.restoringNavigation || !this.openState) return;
    const screens = this.stack.map((frame) => frame.screen);
    const section = this.section;
    const segment = this.segment;
    const roomContext =
      segment === 'room' || section?.startsWith('room-') ? this.options.roomContext?.() : undefined;
    this.options.onNavigate?.(
      {
        ...(section && { section }),
        segment,
        canLeave: () => this.canLeaveAll(),
        restore: () => {
          this.restoringNavigation = true;
          try {
            // Section factories own abort controllers and account state. Rebuild
            // them after leaving instead of reviving their retired instances.
            const sameRoom =
              roomContext === undefined || roomContext === this.options.roomContext?.();
            if (!sameRoom) this.close();
            this.open({ segment, ...(sameRoom && section && { section }) });
            const available =
              sameRoom &&
              (!section ||
                this.options
                  .sections()
                  .some((entry) => entry.id === section && (entry.available?.() ?? true)));
            if (available) {
              for (const screen of screens.slice(section ? 1 : 0)) this.push(screen);
              this.section = section;
            }
          } finally {
            this.restoringNavigation = false;
          }
          this.navigationChanged();
        },
      },
      action,
    );
  }

  private canLeaveAll(): boolean {
    return this.stack.every((frame) => frame.screen.canLeave?.() !== false);
  }

  private leaveAll(): void {
    this.generation++;
    while (this.stack.length) this.stack.pop()?.screen.leave?.();
  }

  private context(frame: Frame): ScreenContext {
    return {
      push: (screen, opener) => this.push(screen, opener ?? null),
      pop: () => this.pop(),
      close: () => this.close(),
      current: () => this.openState && frame.generation === this.generation,
    };
  }

  private show(frame: Frame): void {
    this.back.hidden = false;
    this.title.textContent = frame.screen.title;
    this.body.replaceChildren();
    if (frame.screen.subtitle) this.body.append(el('p', frame.screen.subtitle, 'sheet-subtitle'));
    if (frame.body) {
      this.body.append(frame.body);
      return;
    }
    const context = this.context(frame);
    const built = frame.screen.body(context);
    if (built instanceof Promise) {
      const placeholder = el('p', 'Loading…', 'sheet-loading');
      placeholder.setAttribute('role', 'status');
      this.body.append(placeholder);
      built
        .then((node) => {
          if (!context.current()) return;
          frame.body = node;
          placeholder.remove();
          this.body.append(node);
        })
        .catch((error: unknown) => {
          if (!context.current()) return;
          placeholder.textContent =
            error instanceof Error ? error.message : 'This section could not be loaded.';
          placeholder.className = 'sheet-loading sheet-error';
        });
      return;
    }
    frame.body = built;
    this.body.append(built);
  }

  private renderRoot(): void {
    this.back.hidden = true;
    this.title.textContent = 'Settings';
    this.body.replaceChildren();
    const sections = this.options.sections().filter((section) => section.available?.() ?? true);
    const segments = new Set(sections.map((section) => section.segment));
    if (segments.size > 1) {
      if (!segments.has(this.segment)) this.segment = segments.has('you') ? 'you' : 'room';
      const labels = this.options.segmentLabels ?? { you: 'You', room: 'This room' };
      const switcher = el('div', undefined, 'sheet-segments');
      switcher.setAttribute('role', 'group');
      switcher.setAttribute('aria-label', 'Settings for');
      for (const segment of ['you', 'room'] as const) {
        const choice = button(labels[segment], () => {
          this.segment = segment;
          this.renderRoot();
          this.navigationChanged();
        });
        choice.setAttribute('aria-pressed', String(segment === this.segment));
        switcher.append(choice);
      }
      this.body.append(switcher);
    } else if (segments.size === 1) this.segment = [...segments][0]!;
    const groups = new Map<string, HTMLElement[]>();
    for (const section of sections) {
      if (section.segment !== this.segment) continue;
      const row = navRow(section.label, section.status?.() ?? '', () =>
        this.push(section.screen(), row),
      );
      row.dataset['section'] = section.id;
      const key = section.group ?? '';
      groups.set(key, [...(groups.get(key) ?? []), row]);
    }
    for (const [title, rows] of groups) this.body.append(group(title || null, rows));
    const links = this.options.footer?.() ?? [];
    if (links.length) {
      const footer = el('div', undefined, 'sheet-footer');
      for (const link of links) {
        const node = button(link.label, link.activate, 'sheet-link');
        if (link.danger) node.classList.add('danger');
        footer.append(node);
      }
      this.body.append(footer);
    }
  }

  private rowFor(section: string): HTMLElement | null {
    for (const row of this.body.querySelectorAll<HTMLElement>('button'))
      if (row.dataset['section'] === section) return row;
    return null;
  }

  private focusedSection(): string | null {
    const active = document.activeElement as HTMLElement | null;
    return active && this.element.contains(active) ? (active.dataset['section'] ?? null) : null;
  }
}

/** A list row that opens a screen; the status shows the current value. */
export function navRow(label: string, status: string, activate: () => void): HTMLButtonElement {
  const row = button('', activate, 'sheet-row sheet-row-nav');
  row.append(el('span', label, 'sheet-row-label'), el('span', status, 'sheet-row-status'));
  return row;
}

/** A switch that applies as soon as it changes. */
export function toggleRow(
  label: string,
  checked: boolean,
  onChange: (value: boolean) => void,
  options: { id?: string; disabled?: boolean; describedBy?: string } = {},
): { row: HTMLLabelElement; input: HTMLInputElement } {
  const row = el('label', undefined, 'sheet-row sheet-row-toggle');
  const input = el('input', undefined, 'sheet-switch');
  input.type = 'checkbox';
  input.setAttribute('role', 'switch');
  input.checked = checked;
  if (options.id) input.id = options.id;
  if (options.disabled) input.disabled = true;
  if (options.describedBy) input.setAttribute('aria-describedby', options.describedBy);
  input.addEventListener('change', () => onChange(input.checked));
  row.append(rowLabel(label, input), el('span', '', 'sheet-row-saved'), input);
  return { row, input };
}

/** A row holding a select the caller filled; the caller listens for change. */
export function selectRow(label: string, select: HTMLSelectElement): HTMLLabelElement {
  const row = el('label', undefined, 'sheet-row sheet-row-select');
  row.append(rowLabel(label, select), el('span', '', 'sheet-row-saved'), select);
  return row;
}

/** A row holding one text-like input; the caller listens for change. */
export function fieldRow(label: string, input: HTMLElement): HTMLLabelElement {
  const row = el('label', undefined, 'sheet-row sheet-row-field');
  row.append(rowLabel(label, input), el('span', '', 'sheet-row-saved'), input);
  return row;
}

/** Two or three exclusive choices shown side by side. */
export function segmentRow(
  label: string,
  choices: { value: string; label: string; id?: string }[],
  value: string,
  onChange: (value: string) => void,
  options: { disabled?: boolean } = {},
): { row: HTMLElement; set: (value: string) => void; buttons: HTMLButtonElement[] } {
  const row = el('div', undefined, 'sheet-row sheet-row-segment');
  const control = el('div', undefined, 'sheet-segment');
  control.setAttribute('role', 'group');
  control.setAttribute('aria-label', label);
  const buttons = choices.map((choice) => {
    const node = button(choice.label, () => {
      if (node.getAttribute('aria-pressed') === 'true') return;
      set(choice.value);
      onChange(choice.value);
    });
    if (choice.id) node.id = choice.id;
    node.dataset['value'] = choice.value;
    if (options.disabled) node.disabled = true;
    return node;
  });
  const set = (next: string): void => {
    for (const node of buttons)
      node.setAttribute('aria-pressed', String(node.dataset['value'] === next));
  };
  set(value);
  control.append(...buttons);
  row.append(el('span', label, 'sheet-row-label'), el('span', '', 'sheet-row-saved'), control);
  return { row, set, buttons };
}

/** A row that performs an action, such as signing out. */
export function actionRow(
  label: string,
  activate: () => void,
  options: { danger?: boolean; status?: string } = {},
): HTMLButtonElement {
  const row = button('', activate, 'sheet-row sheet-row-action');
  if (options.danger) row.classList.add('danger');
  row.append(
    el('span', label, 'sheet-row-label'),
    el('span', options.status ?? '', 'sheet-row-status'),
  );
  return row;
}

export function group(title: string | null, rows: HTMLElement[]): HTMLElement {
  const node = el('div', undefined, 'sheet-group');
  if (title) node.append(el('div', title, 'sheet-group-title'));
  node.append(...rows);
  return node;
}

export function note(text: string): HTMLElement {
  return el('p', text, 'sheet-note');
}

/** A quiet confirmation inside the row; it fades after a moment. */
export function markSaved(row: HTMLElement, text = 'Saved'): void {
  const mark = row.querySelector<HTMLElement>('.sheet-row-saved');
  if (!mark) return;
  mark.textContent = text;
  mark.setAttribute('role', 'status');
  row.classList.add('is-saved');
  const previous = savedTimers.get(row);
  if (previous !== undefined) clearTimeout(previous);
  savedTimers.set(
    row,
    setTimeout(() => {
      mark.textContent = '';
      row.classList.remove('is-saved');
      savedTimers.delete(row);
    }, SAVED_MS),
  );
}
