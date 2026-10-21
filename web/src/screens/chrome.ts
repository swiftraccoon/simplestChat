import type { Elements } from '../dom';
import type { RoomClient } from '../room';
import type { SettingsSheet } from '../settings-sheet';
import type { ClientTelemetry } from '../telemetry';
import type { RoomLayout } from './room-layout';
import { createScope, type Scope } from '../disposable';
import * as icons from '../icons';
import { button, el, modal } from '../ui';

interface ChromeServices {
  getRoom: () => RoomClient | null;
  readonly settings: SettingsSheet;
  readonly telemetry: ClientTelemetry;
  readonly layout: RoomLayout;
  notify: (message: string, duration?: number, kind?: 'info' | 'error') => void;
  observe: (task: Promise<unknown>, message: string) => void;
  asyncAction: (action: () => Promise<unknown>, message: string) => () => void;
}

/**
 * A header menu: its button toggles it; any choice, Escape or a click away closes it. A menu
 * laid out flat (data-flat, the account menu inside More) ignores every closing path.
 */
export function setupMenu(
  toggle: HTMLButtonElement,
  menu: HTMLElement,
  scope = createScope(),
): Scope {
  const wrapper = toggle.parentElement;
  if (!wrapper) throw new Error('A menu button must have a parent.');
  const listen = (
    target: EventTarget,
    type: string,
    listener: EventListener,
    capture = false,
  ): void => {
    target.addEventListener(type, listener, { capture, signal: scope.signal });
  };
  const popup = (): boolean => menu.dataset?.['flat'] !== 'true';
  const setOpen = (open: boolean): void => {
    if (!popup()) return;
    menu.hidden = !open;
    toggle.setAttribute('aria-expanded', String(open));
    if (open) menu.querySelector<HTMLElement>('button:not([hidden]):not(:disabled)')?.focus();
  };
  listen(toggle, 'click', () => setOpen(toggle.getAttribute('aria-expanded') !== 'true'));
  listen(menu, 'mousedown', (rawEvent) => {
    const event = rawEvent as MouseEvent;
    // Safari otherwise blurs the focused choice before click, hiding the menu
    // through focusout before the pointer can activate a different choice.
    const target = event.target as Element;
    if (
      event.button === 0 &&
      (target.closest('button, a, input[type="checkbox"]') ||
        target.closest('label')?.querySelector('input[type="checkbox"]'))
    )
      event.preventDefault();
  });
  // Focus returns to the toggle before a choice runs, so a dialog it opens restores
  // focus there instead of to a menu item that is about to be hidden.
  listen(
    menu,
    'click',
    (event) => {
      if ((event.target as Element).closest('button, a')) toggle.focus();
    },
    true,
  );
  listen(menu, 'click', (event) => {
    if ((event.target as Element).closest('button, a')) setOpen(false);
  });
  listen(wrapper, 'keydown', (rawEvent) => {
    const event = rawEvent as KeyboardEvent;
    if (event.key !== 'Escape' || menu.hidden) return;
    event.stopPropagation();
    setOpen(false);
    toggle.focus();
  });
  listen(wrapper, 'focusout', (rawEvent) => {
    const event = rawEvent as FocusEvent;
    if (!menu.hidden && !wrapper.contains(event.relatedTarget as Node | null)) setOpen(false);
  });
  listen(document, 'click', (event) => {
    if (!menu.hidden && !wrapper.contains(event.target as Node)) setOpen(false);
  });
  return scope;
}

/** Every global shortcut in one place; the call buttons also name their own. */
export function showKeyboardShortcuts(): void {
  const view = modal('Keyboard shortcuts');
  const list = el('dl', undefined, 'shortcut-list');
  const keys = (...names: string[]): HTMLElement => {
    const term = el('dt');
    names.forEach((name, index) => {
      if (index) term.append(' or ');
      term.append(el('kbd', name));
    });
    return term;
  };
  list.append(
    keys('M'),
    el('dd', 'Turn your microphone on or off'),
    keys('V'),
    el('dd', 'Turn your camera on or off'),
    keys('S'),
    el('dd', 'Start or stop sharing your screen'),
    keys('Space', 'T'),
    el('dd', 'Hold to talk, in push-to-talk mode'),
    keys('Esc'),
    el('dd', 'Close a menu or dialog'),
  );
  view.body.append(
    el('p', 'Shortcuts work in a room whenever you are not typing in a text box.', 'setting-hint'),
    list,
    el(
      'p',
      'In chat: Enter sends on desktop; Shift+Enter adds a new line. On touch devices, Enter adds a new line and the send button sends. Ctrl+Enter or ⌘+Enter sends on either.',
      'setting-hint',
    ),
  );
}

/** Clipboard access can be refused; offer the link in a selectable field instead. */
export function showCopyFallback(url: string): void {
  const view = modal('Copy room link');
  view.body.append(el('p', 'Clipboard access is unavailable here. Select the link to copy it.'));
  const field = el('input');
  field.type = 'text';
  field.readOnly = true;
  field.value = url;
  field.setAttribute('aria-label', 'Room link');
  view.body.append(field);
  field.focus();
  field.select();
}

/** Owned replacement for native alerts at the moments people are removed or refused. */
export function showRoomExitNotice(title: string, message: string): void {
  const view = modal(title);
  view.body.append(el('p', message, 'room-exit-message'));
  view.body.append(button('Back to home', () => view.close(), 'btn-primary'));
}

/** Diagnostics sit on the home card outside a room and in the room tools' "More" menu inside one. */
export function placeDiagnosticsButton(
  elements: Pick<Elements, 'roomMoreRepair' | 'homeTools'>,
  diagnosticsButton: HTMLButtonElement,
  inRoom: boolean,
): void {
  const { roomMoreRepair, homeTools } = elements;
  const target = inRoom ? roomMoreRepair : homeTools;
  if (target && diagnosticsButton.parentNode !== target) target.append(diagnosticsButton);
}

/** Fresh sessions never capture on entry; say so and move focus into the room. */
export function announceRoomEntry(
  roomScreen: HTMLElement,
  notify: ChromeServices['notify'],
  roomName: string,
): void {
  roomScreen.focus();
  notify(`Joined ${roomName}. Your camera and microphone stay off until you turn them on.`, 6000);
}

// --- Set button icons (static SVG from our icons module, no user content) ---
export function setButtonContent(btn: HTMLElement, iconHtml: string, tooltip?: string): void {
  btn.textContent = '';
  btn.insertAdjacentHTML('afterbegin', iconHtml);
  if (tooltip) {
    const span = document.createElement('span');
    span.className = 'btn-tooltip';
    span.textContent = tooltip;
    btn.appendChild(span);
  }
}

/** Replacing the icon must retain the live unread badge used by chat. */
export function initializeUnreadButton(
  scrollBottomBtn: HTMLElement,
  unreadBadge: HTMLElement,
): void {
  scrollBottomBtn.textContent = '';
  scrollBottomBtn.insertAdjacentHTML('afterbegin', icons.scrollDown());
  scrollBottomBtn.appendChild(unreadBadge);
}

/** Mount shared app controls without taking ownership of account or room state. */
export function mountChrome(elements: Elements, services: ChromeServices) {
  const scope = createScope();
  const listen = (target: EventTarget, type: string, listener: EventListener): void => {
    target.addEventListener(type, listener, { signal: scope.signal });
  };
  const {
    micSetupBtn,
    copyRoomLink,
    rosterToggleBtn,
    chatToggleBtn,
    roomMoreBtn,
    shortcutsBtn,
    header,
    roomTools,
    toastContainer,
    roomMoreMenu,
    accountMenuBtn,
    accountMenu,
    roomScreen,
    headerRight,
    chatSendBtn,
    micBtn,
    camBtn,
    screenBtn,
    handBtn,
    roomSettingsBtn,
    settingsBtn,
    leaveBtn,
    sidebarCollapseBtn,
    scrollBottomBtn,
    unreadBadge,
  } = elements;
  listen(micSetupBtn, 'click', () => {
    services.settings.open({
      section: 'camera',
      opener: document.activeElement as HTMLElement | null,
    });
  });
  listen(
    copyRoomLink,
    'click',
    services.asyncAction(async () => {
      if (scope.signal.aborted) return;
      const activeRoom = services.getRoom();
      if (!activeRoom?.currentRoomId) return;
      const current = (): boolean => !scope.signal.aborted && services.getRoom() === activeRoom;
      const url = new URL(window.location.href);
      url.hash = activeRoom.currentRoomId;
      url.search = '';
      // Phones offer their share sheet; desktops copy, as people expect there.
      if (!services.layout.desktop && typeof navigator.share === 'function') {
        if (!current()) return;
        try {
          await navigator.share({
            title: activeRoom.roomSettings?.displayName ?? activeRoom.currentRoomId,
            url: url.toString(),
          });
          return;
        } catch (error) {
          if (!current()) return;
          if (error instanceof DOMException && error.name === 'AbortError') return;
        }
      }
      if (!current()) return;
      try {
        await navigator.clipboard.writeText(url.toString());
        if (current()) services.notify('Room link copied');
      } catch {
        if (current()) showCopyFallback(url.toString());
      }
    }, 'Could not copy the room link'),
  );

  for (const [control, icon] of [
    [copyRoomLink, icons.link()],
    [rosterToggleBtn, icons.userIcon()],
    [chatToggleBtn, icons.chatIcon()],
  ] as const)
    control.insertAdjacentHTML('afterbegin', icon);
  roomMoreBtn.insertAdjacentHTML('beforeend', icons.chevronDown());
  listen(shortcutsBtn, 'click', showKeyboardShortcuts);
  setupRoomMenu();
  setupAccountMenu();
  if (typeof ResizeObserver !== 'undefined') {
    const bars = new ResizeObserver(placeToasts);
    bars.observe(header);
    bars.observe(roomTools);
    scope.onDispose(() => bars.disconnect());
  }

  /** Toasts sit below the shared header, clear of its controls. */
  function placeToasts(): void {
    const top = Math.round(header.getBoundingClientRect().bottom) + 12;
    toastContainer.style.setProperty('--toast-top', `${top}px`);
  }

  /** "More" holds the room tools needed now and then. */
  function setupRoomMenu(): void {
    setupMenu(roomMoreBtn, roomMoreMenu, scope);
  }

  /** The avatar button opens the account's actions on the home page. */
  function setupAccountMenu(): void {
    setupMenu(accountMenuBtn, accountMenu, scope);
  }

  /** Keep secondary room tools in the header and return home utilities when leaving. */
  function setRoomToolsVisible(visible: boolean): void {
    roomTools.hidden = !visible;
    header.classList.toggle('in-room', visible);
    const utilities = headerRight;
    const target = visible ? roomMoreMenu : header;
    if (utilities.parentElement !== target) target.append(utilities);
    roomMoreMenu.hidden = true;
    roomMoreBtn.setAttribute('aria-expanded', 'false');
    // In a room the account menu's items sit flat inside More; at home the avatar opens them.
    accountMenu.dataset['flat'] = String(visible);
    accountMenu.hidden = !visible;
    accountMenuBtn.setAttribute('aria-expanded', String(visible));
  }

  setButtonContent(chatSendBtn, icons.send());
  setButtonContent(micBtn, icons.micOn(), 'Mic (M)');
  setButtonContent(camBtn, icons.camOn(), 'Cam (V)');
  setButtonContent(screenBtn, icons.screenShare(), 'Screen (S)');
  // Hide screen share button if getDisplayMedia is not supported (mobile)
  if (!navigator.mediaDevices?.getDisplayMedia) {
    screenBtn.hidden = true;
  }
  setButtonContent(handBtn, icons.handRaised(), 'Raise Hand');
  setButtonContent(roomSettingsBtn, icons.roomSettings(), 'Room Settings');
  setButtonContent(settingsBtn, icons.settings(), 'Your settings');
  setButtonContent(leaveBtn, icons.leave(), 'Leave');
  setButtonContent(sidebarCollapseBtn, icons.chevronDown());

  // --- Scroll-to-bottom for chat ---
  initializeUnreadButton(scrollBottomBtn, unreadBadge);

  const diagnosticsButton = button(
    'Diagnostics',
    () => {
      if (scope.signal.aborted) return;
      const view = modal('Diagnostic summary');
      const current = (): boolean => !scope.signal.aborted && view.dialog.open;
      view.body.append(
        el(
          'p',
          'Review this summary before copying it to support. Its reference identifies this local report, not a server session. It contains app events, browser family and release information; it excludes names, messages, credentials and network addresses.',
        ),
      );
      const sharing = el('input');
      sharing.type = 'checkbox';
      sharing.checked = services.telemetry.sharingEnabled;
      sharing.addEventListener('change', () => services.telemetry.setSharing(sharing.checked));
      const sharingLabel = el('label', ' Share anonymous reliability measurements');
      sharingLabel.prepend(sharing);
      view.body.append(
        sharingLabel,
        el(
          'p',
          'Optional. Sends fixed event categories and timings to this server without account, room or support identifiers. Media measurements run only while enabled and the page is visible. Turning this off discards queued uploads.',
        ),
      );
      const preview = el('textarea');
      preview.readOnly = true;
      preview.rows = 16;
      preview.value = services.telemetry.summary();
      preview.setAttribute('aria-label', 'Diagnostic summary preview');
      const copy = button('Copy summary', () => {
        if (!current()) return;
        navigator.clipboard
          .writeText(preview.value)
          .then(() => {
            if (current()) services.notify('Diagnostic summary copied');
          })
          .catch(() => {
            if (!current()) return;
            preview.focus();
            preview.select();
          });
      });
      const refresh = button('Refresh preview', () => {
        preview.value = services.telemetry.summary();
      });
      view.body.append(preview, refresh, copy);
    },
    'auth-link-btn',
  );
  diagnosticsButton.id = 'diagnostics-btn';
  placeDiagnosticsButton(elements, diagnosticsButton, false);
  return {
    diagnosticsButton,
    setRoomToolsVisible,
    showKeyboardShortcuts,
    showCopyFallback,
    showRoomExitNotice,
    setButtonContent,
    announceRoomEntry: (name: string) => announceRoomEntry(roomScreen, services.notify, name),
    placeDiagnosticsButton: (inRoom: boolean) =>
      placeDiagnosticsButton(elements, diagnosticsButton, inRoom),
    dispose: () => {
      diagnosticsButton.remove();
      scope.dispose();
    },
  };
}
