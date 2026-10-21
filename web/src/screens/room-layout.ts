import type { Elements } from '../dom';
import * as icons from '../icons';
import { createScope, type Scope } from '../disposable';
import { group, markSaved, note, segmentRow, type SheetSection } from '../settings-sheet';

export interface PanelPreferences {
  rosterWidth: number;
  chatWidth: number;
  rosterCollapsed: boolean;
  chatCollapsed: boolean;
  mobilePanelCollapsed: boolean;
}
interface LayoutActions {
  readPreference: (key: string) => string | null;
  writePreference: (key: string, value: string) => void;
  renderParticipants: () => void;
}
export function isDesktopLayout(): boolean {
  return !window.matchMedia(
    '(max-width: 768px), (max-width: 900px) and (max-height: 500px) and (orientation: landscape)',
  ).matches;
}

/** Landscape phone CSS always shows the sidebar, even after a portrait collapse. */
export function isLandscapePhoneLayout(): boolean {
  return window.matchMedia(
    '(max-width: 900px) and (max-height: 500px) and (orientation: landscape)',
  ).matches;
}

/** Owns panel preferences, responsive presentation and pointer/keyboard resizing. */
export function createRoomLayout(elements: Elements, actions: LayoutActions) {
  const scope = createScope();
  const {
    roomScreen,
    sidebarTabs,
    tabContents,
    sidebarCollapseBtn,
    lobbyTab,
    chatTab,
    usersTab,
    rosterToggleBtn,
    chatToggleBtn,
    sidebarElement,
  } = elements;
  const readLocalPreference = actions.readPreference;
  const writeLocalPreference = actions.writePreference;
  const isRecord = (value: unknown): value is Record<string, unknown> =>
    typeof value === 'object' && value !== null;
  function listen<K extends keyof HTMLElementEventMap>(
    target: HTMLElement | Window,
    type: K,
    listener: (event: HTMLElementEventMap[K]) => void,
  ): void {
    target.addEventListener(type, listener as EventListener, { signal: scope.signal });
  }
  // --- Layout Management ---
  /**
   * Phones stack the room and landscape phones up to 900 px wide split it,
   * without the desktop panels; the queries are style.css's own.
   */
  function getLayout(): 'modern' | 'classic' {
    return readLocalPreference('layout') === 'modern' ? 'modern' : 'classic';
  }

  function isMobilePanelCollapsed(): boolean {
    return !isDesktopLayout() && panelPreferences.mobilePanelCollapsed && !isLandscapePhoneLayout();
  }

  function setLayout(layout: 'modern' | 'classic'): void {
    writeLocalPreference('layout', layout);
    roomScreen.classList.remove('layout-modern', 'layout-classic');
    roomScreen.classList.add(`layout-${layout}`);

    // In classic mode, hide the Users tab from the sidebar (users are in the left panel)
    // and force the Chat tab active
    if (usersTab) usersTab.hidden = layout === 'classic' && isDesktopLayout();
    if (usersTab?.hidden && usersTab.classList.contains('active')) selectSidebarTab('chat');
    applyPanelPreferences();

    // Re-render participants for classic mode
    actions.renderParticipants();
  }

  // --- Sidebar Tabs ---
  /** On phones a tab press also reopens a panel that was collapsed to its tab bar. */
  function expandMobilePanel(): void {
    if (panelPreferences.mobilePanelCollapsed && !isDesktopLayout()) {
      panelPreferences.mobilePanelCollapsed = false;
      savePanelPreferences();
    }
  }

  sidebarTabs.forEach((tab) => {
    listen(tab, 'click', () => {
      const target = tab.dataset['tab'];
      sidebarTabs.forEach((t) => t.classList.toggle('active', t === tab));
      tabContents.forEach((c) => c.classList.toggle('active', c.id === `${target}-panel`));
      expandMobilePanel();
      actions.renderParticipants();
    });
  });
  listen(sidebarCollapseBtn, 'click', () => {
    panelPreferences.mobilePanelCollapsed = !panelPreferences.mobilePanelCollapsed;
    savePanelPreferences();
    actions.renderParticipants();
  });

  // Hidden tabs are part of the validated static shell too.
  listen(lobbyTab, 'click', () => {
    sidebarTabs.forEach((t) => t.classList.toggle('active', t === lobbyTab));
    tabContents.forEach((c) => c.classList.toggle('active', c.id === 'lobby-panel'));
    expandMobilePanel();
    actions.renderParticipants();
  });

  // Set tab content with icons (these are static SVG literals from our icons module)
  if (chatTab) {
    chatTab.textContent = '';
    chatTab.insertAdjacentHTML('afterbegin', icons.chatIcon());
    chatTab.append(' Chat');
  }
  if (usersTab) {
    usersTab.textContent = '';
    usersTab.insertAdjacentHTML('afterbegin', icons.userIcon());
    usersTab.append(' People');
  }

  const panelPreferences: PanelPreferences = (() => {
    try {
      const saved: unknown = JSON.parse(localStorage.getItem('panelPreferences') ?? '{}');
      if (!isRecord(saved)) throw new Error('Invalid panel preferences');
      return {
        rosterWidth: Math.max(160, Math.min(360, Number(saved['rosterWidth']) || 220)),
        chatWidth: Math.max(240, Math.min(520, Number(saved['chatWidth']) || 320)),
        rosterCollapsed: saved['rosterCollapsed'] === true,
        chatCollapsed: saved['chatCollapsed'] === true,
        mobilePanelCollapsed: saved['mobilePanelCollapsed'] === true,
      };
    } catch {
      return {
        rosterWidth: 220,
        chatWidth: 320,
        rosterCollapsed: false,
        chatCollapsed: false,
        mobilePanelCollapsed: false,
      };
    }
  })();

  function selectSidebarTab(tab: 'chat' | 'users' | 'lobby' | 'settings'): void {
    sidebarTabs.forEach((button) =>
      button.classList.toggle('active', button.dataset['tab'] === tab),
    );
    tabContents.forEach((content) =>
      content.classList.toggle('active', content.id === `${tab}-panel`),
    );
  }

  function applyPanelPreferences(): void {
    const desktop = isDesktopLayout();
    const rosterCollapsed =
      desktop && panelPreferences.rosterCollapsed && getLayout() === 'classic';
    const chatCollapsed = desktop && panelPreferences.chatCollapsed;
    const rosterWidth = `${rosterCollapsed ? 0 : Math.min(panelPreferences.rosterWidth, window.innerWidth * 0.3)}px`;
    const chatWidth = `${chatCollapsed ? 0 : Math.min(panelPreferences.chatWidth, window.innerWidth * 0.4)}px`;
    roomScreen.style.setProperty('--roster-width', rosterWidth);
    roomScreen.style.setProperty('--chat-width', chatWidth);
    // The header and room tools sit outside the room grid but align with its columns.
    document.documentElement.style.setProperty('--roster-width', rosterWidth);
    document.documentElement.style.setProperty('--chat-width', chatWidth);
    roomScreen.classList.toggle('roster-collapsed', rosterCollapsed);
    roomScreen.classList.toggle('chat-collapsed', chatCollapsed);
    elements.rosterToolLabel.textContent = rosterCollapsed ? 'Show people' : 'People';
    elements.chatToolLabel.textContent = chatCollapsed ? 'Show chat' : 'Chat';
    rosterToggleBtn.setAttribute('aria-expanded', String(!rosterCollapsed));
    chatToggleBtn.setAttribute('aria-expanded', String(!chatCollapsed));
    const roster = document.getElementById('classic-users-panel');
    if (roster) roster.inert = rosterCollapsed;
    sidebarElement.inert = chatCollapsed;
    if (usersTab) usersTab.hidden = desktop && getLayout() === 'classic';
    if (usersTab?.hidden && usersTab.classList.contains('active')) selectSidebarTab('chat');
    const mobileCollapsed = isMobilePanelCollapsed();
    roomScreen.classList.toggle('mobile-panel-collapsed', mobileCollapsed);
    sidebarCollapseBtn.setAttribute('aria-expanded', String(!mobileCollapsed));
    sidebarCollapseBtn.setAttribute(
      'aria-label',
      mobileCollapsed ? 'Show the chat panel' : 'Collapse the chat panel',
    );
  }

  function savePanelPreferences(): void {
    try {
      localStorage.setItem('panelPreferences', JSON.stringify(panelPreferences));
    } catch {
      /* Retain session preferences. */
    }
    applyPanelPreferences();
  }

  function attachPanelResize(panel: HTMLElement, side: 'roster' | 'chat'): void {
    if (panel.querySelector('.panel-resize-handle')) return;
    const handle = document.createElement('div');
    handle.className = `panel-resize-handle resize-${side}`;
    handle.dataset['side'] = side;
    handle.tabIndex = 0;
    handle.setAttribute('role', 'separator');
    handle.setAttribute('aria-orientation', 'vertical');
    handle.setAttribute('aria-label', `Resize ${side === 'roster' ? 'people' : 'chat'} panel`);
    handle.setAttribute('aria-valuemin', side === 'roster' ? '160' : '240');
    handle.setAttribute('aria-valuemax', side === 'roster' ? '360' : '520');
    handle.setAttribute(
      'aria-valuenow',
      String(panelPreferences[side === 'roster' ? 'rosterWidth' : 'chatWidth']),
    );
    panel.append(handle);
  }
  let drag: Scope | null = null;
  scope.onDispose(() => drag?.dispose());
  const resizeHandle = (event: Event): HTMLElement | null =>
    event.target instanceof Element
      ? event.target.closest<HTMLElement>('.panel-resize-handle')
      : null;
  function resize(handle: HTMLElement, width: number): void {
    const side = handle.dataset['side'];
    const key = side === 'roster' ? 'rosterWidth' : 'chatWidth';
    panelPreferences[key] = Math.max(
      side === 'roster' ? 160 : 240,
      Math.min(side === 'roster' ? 360 : 520, width),
    );
    handle.setAttribute('aria-valuenow', String(Math.round(panelPreferences[key])));
    applyPanelPreferences();
  }
  listen(roomScreen, 'pointerdown', (event) => {
    const handle = resizeHandle(event);
    if (!handle || !isDesktopLayout() || event.button !== 0) return;
    event.preventDefault();
    drag?.dispose();
    drag = createScope();
    const owner = drag;
    const side = handle.dataset['side'];
    const startWidth = panelPreferences[side === 'roster' ? 'rosterWidth' : 'chatWidth'];
    const finish = (): void => {
      owner.dispose();
      if (drag === owner) drag = null;
    };
    window.addEventListener(
      'pointermove',
      (next) => {
        if (next.pointerId === event.pointerId)
          resize(
            handle,
            startWidth + (next.clientX - event.clientX) * (side === 'roster' ? 1 : -1),
          );
      },
      { signal: owner.signal },
    );
    window.addEventListener(
      'pointerup',
      (next) => {
        if (next.pointerId === event.pointerId) finish();
      },
      { signal: owner.signal },
    );
    window.addEventListener('pointercancel', finish, { signal: owner.signal });
    window.addEventListener('blur', finish, { signal: owner.signal });
    owner.onDispose(savePanelPreferences);
  });
  listen(roomScreen, 'keydown', (event) => {
    const handle = resizeHandle(event);
    if (!handle || (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight')) return;
    event.preventDefault();
    const side = handle.dataset['side'];
    const key = side === 'roster' ? 'rosterWidth' : 'chatWidth';
    resize(
      handle,
      panelPreferences[key] +
        (event.key === 'ArrowRight' ? 20 : -20) * (side === 'roster' ? 1 : -1),
    );
    savePanelPreferences();
  });

  listen(rosterToggleBtn, 'click', () => {
    if (getLayout() === 'classic' && isDesktopLayout()) {
      panelPreferences.rosterCollapsed = !panelPreferences.rosterCollapsed;
    } else {
      panelPreferences.chatCollapsed = false;
      selectSidebarTab('users');
      expandMobilePanel();
    }
    savePanelPreferences();
    actions.renderParticipants();
  });
  listen(chatToggleBtn, 'click', () => {
    if (isDesktopLayout()) panelPreferences.chatCollapsed = !panelPreferences.chatCollapsed;
    selectSidebarTab('chat');
    savePanelPreferences();
    actions.renderParticipants();
  });
  attachPanelResize(sidebarElement, 'chat');
  let rosterOnDesktop = isDesktopLayout();
  let rosterInLandscapePhone = isLandscapePhoneLayout();
  listen(window, 'resize', () => {
    applyPanelPreferences();
    const desktop = isDesktopLayout();
    const landscape = isLandscapePhoneLayout();
    if (desktop !== rosterOnDesktop || landscape !== rosterInLandscapePhone)
      actions.renderParticipants();
    rosterOnDesktop = desktop;
    rosterInLandscapePhone = landscape;
  });
  const layoutSection: SheetSection = {
    id: 'layout',
    segment: 'you',
    label: 'Layout',
    status: () => (getLayout() === 'modern' ? 'Video & chat' : 'People, video & chat'),
    screen: () => ({
      title: 'Layout',
      body: () => {
        const choice = segmentRow(
          'Layout',
          [
            { value: 'classic', label: 'People, video & chat', id: 'layout-classic' },
            { value: 'modern', label: 'Video & chat', id: 'layout-modern' },
          ],
          getLayout(),
          (value) => {
            setLayout(value as 'modern' | 'classic');
            markSaved(choice.row);
          },
        );
        return group(null, [
          choice.row,
          note('Only your view changes. Video & chat gives the stage the people column.'),
        ]);
      },
    }),
  };

  return {
    preferences: panelPreferences,
    section: layoutSection,
    get layout() {
      return getLayout();
    },
    get desktop() {
      return isDesktopLayout();
    },
    get mobileCollapsed() {
      return isMobilePanelCollapsed();
    },
    apply: applyPanelPreferences,
    setLayout,
    selectTab: selectSidebarTab,
    attachPanelResize,
    dispose: () => scope.dispose(),
  };
}
export type RoomLayout = ReturnType<typeof createRoomLayout>;
