/** Static page elements owned by the application composition root. */
export interface Elements {
  readonly connectionStatus: HTMLSpanElement;
  readonly joinScreen: HTMLDivElement;
  readonly roomScreen: HTMLDivElement;
  readonly roomLabel: HTMLSpanElement;
  readonly roomTopic: HTMLButtonElement;
  readonly homeLink: HTMLAnchorElement;
  readonly nameInput: HTMLInputElement;
  readonly roomInput: HTMLInputElement;
  readonly joinBtn: HTMLButtonElement;
  readonly videoGrid: HTMLDivElement;
  readonly participantList: HTMLUListElement;
  readonly chatMessages: HTMLDivElement;
  readonly chatInput: HTMLTextAreaElement;
  readonly chatSendBtn: HTMLButtonElement;
  readonly micBtn: HTMLButtonElement;
  readonly camBtn: HTMLButtonElement;
  readonly screenBtn: HTMLButtonElement;
  readonly settingsBtn: HTMLButtonElement;
  readonly leaveBtn: HTMLButtonElement;
  readonly qualityIndicator: HTMLSpanElement;
  readonly scrollBottomBtn: HTMLButtonElement;
  readonly unreadBadge: HTMLSpanElement;
  readonly handBtn: HTMLButtonElement;
  readonly roomSettingsBtn: HTMLButtonElement;
  readonly roomTools: HTMLDivElement;
  readonly rosterToggleBtn: HTMLButtonElement;
  readonly chatToggleBtn: HTMLButtonElement;
  readonly lobbyScreen: HTMLDivElement;
  readonly lobbyRoomName: HTMLHeadingElement;
  readonly lobbyTopic: HTMLParagraphElement;
  readonly lobbyCount: HTMLParagraphElement;
  readonly lobbyCancelBtn: HTMLButtonElement;
  readonly lobbyListEl: HTMLUListElement;
  readonly lobbyEmpty: HTMLDivElement;
  readonly authBarGuest: HTMLDivElement;
  readonly authBarUser: HTMLDivElement;
  readonly authDisplayName: HTMLSpanElement;
  readonly authAvatar: HTMLSpanElement;
  readonly signInBtn: HTMLButtonElement;
  readonly logoutBtn: HTMLButtonElement;
  readonly roomBrowser: HTMLDivElement;
  readonly directoryToolbar: HTMLDivElement;
  readonly roomSearchInput: HTMLInputElement;
  readonly roomList: HTMLDivElement;
  readonly roomLoadMore: HTMLButtonElement;
  readonly createRoomBtn: HTMLButtonElement;
  readonly serverMode: HTMLParagraphElement;
  readonly capabilitiesRetry: HTMLButtonElement;
  readonly authRestoreNotice: HTMLDivElement;
  readonly authRestoreStatus: HTMLParagraphElement;
  readonly authRestoreRetry: HTMLButtonElement;
  readonly loginModal: HTMLDialogElement;
  readonly loginClose: HTMLButtonElement;
  readonly loginEmail: HTMLInputElement;
  readonly loginPassword: HTMLInputElement;
  readonly loginSubmit: HTMLButtonElement;
  readonly loginError: HTMLDivElement;
  readonly loginPasskeyBtn: HTMLButtonElement;
  readonly loginToRegister: HTMLButtonElement;
  readonly registerModal: HTMLDialogElement;
  readonly registerClose: HTMLButtonElement;
  readonly registerEmail: HTMLInputElement;
  readonly registerName: HTMLInputElement;
  readonly registerPassword: HTMLInputElement;
  readonly registerConfirm: HTMLInputElement;
  readonly registerInvite: HTMLInputElement;
  readonly registerSubmit: HTMLButtonElement;
  readonly registerError: HTMLDivElement;
  readonly registerPasskeyBtn: HTMLButtonElement;
  readonly registerToLogin: HTMLButtonElement;
  readonly createRoomModal: HTMLDialogElement;
  readonly createRoomClose: HTMLButtonElement;
  readonly crId: HTMLInputElement;
  readonly crName: HTMLInputElement;
  readonly crTopic: HTMLInputElement;
  readonly crPassword: HTMLInputElement;
  readonly crModerated: HTMLInputElement;
  readonly crLobby: HTMLInputElement;
  readonly crSecret: HTMLInputElement;
  readonly crGuests: HTMLInputElement;
  readonly createRoomSubmit: HTMLButtonElement;
  readonly createRoomError: HTMLDivElement;
  readonly toastContainer: HTMLDivElement;
  readonly sidebarCollapseBtn: HTMLButtonElement;
  readonly communityActions: HTMLDivElement;
  readonly roomVolumeSlot: HTMLDivElement;
  readonly homeSettingsHost: HTMLElement;
  readonly sidebarElement: HTMLElement;
  readonly micSetupBtn: HTMLButtonElement;
  readonly copyRoomLink: HTMLButtonElement;
  readonly shortcutsBtn: HTMLButtonElement;
  readonly roomMoreBtn: HTMLButtonElement;
  readonly roomMoreMenu: HTMLDivElement;
  readonly accountMenuBtn: HTMLButtonElement;
  readonly accountMenu: HTMLDivElement;
  readonly roomMoreRepair: HTMLDivElement;
  readonly homeTools: HTMLDivElement;
  readonly registrationHelp: HTMLParagraphElement;
  readonly savedRoomPicker: HTMLElement;
  readonly savedRoomContainer: HTMLDivElement;
  readonly directoryRailToggle: HTMLButtonElement;
  readonly header: HTMLElement;
  readonly headerRight: HTMLDivElement;
  readonly rosterToolLabel: HTMLSpanElement;
  readonly chatToolLabel: HTMLSpanElement;
  readonly roomNavigation: HTMLDivElement;
  readonly lobbyTab: HTMLButtonElement;
  readonly chatTab: HTMLButtonElement;
  readonly usersTab: HTMLButtonElement;
  readonly chatPanel: HTMLDivElement;
  readonly lobbyPanel: HTMLDivElement;
  readonly settingsPanel: HTMLDivElement;
  readonly usersPanel: HTMLDivElement;
  readonly sidebarTabs: readonly HTMLButtonElement[];
  readonly tabContents: readonly HTMLDivElement[];
}

interface ElementConstructor<T extends HTMLElement> {
  new (): T;
  readonly name: string;
}

const startupError = (detail: string): Error =>
  new Error(`Could not start simplestChat: ${detail}`);

/**
 * Resolve and validate the static page before mounting any screen. The returned
 * references survive moves between containers; dynamic room/dialog content stays
 * with its owner. A missing or mistyped element fails startup with its selector.
 */
export function queryElements(document: Document): Elements {
  const view = document.defaultView;
  if (!view) throw startupError('the document has no browser window.');

  function validate<T extends HTMLElement>(
    element: Element | null,
    selector: string,
    constructor: ElementConstructor<T>,
  ): T {
    if (element === null) throw startupError(`required element ${selector} is missing.`);
    if (!(element instanceof constructor)) {
      throw startupError(`${selector} must be ${constructor.name}; found <${element.localName}>.`);
    }
    return element;
  }

  function required<T extends HTMLElement>(
    selector: string,
    constructor: ElementConstructor<T>,
  ): T {
    return validate(document.querySelector(selector), selector, constructor);
  }

  function requiredAll<T extends HTMLElement>(
    selector: string,
    constructor: ElementConstructor<T>,
  ): readonly T[] {
    const elements = document.querySelectorAll(selector);
    if (elements.length === 0) throw startupError(`required elements ${selector} are missing.`);
    return Array.from(elements, (element) => validate(element, selector, constructor));
  }

  function member<T extends HTMLElement>(
    elements: readonly T[],
    matches: (element: T) => boolean,
    selector: string,
  ): T {
    const element = elements.find(matches);
    if (!element) throw startupError(`required element ${selector} is missing.`);
    return element;
  }

  const sidebarTabs = requiredAll('#sidebar-tabs .tab', view.HTMLButtonElement);
  const tabContents = requiredAll('#sidebar-content .tab-content', view.HTMLDivElement);

  return {
    connectionStatus: required('#connection-status', view.HTMLSpanElement),
    joinScreen: required('#join-screen', view.HTMLDivElement),
    roomScreen: required('#room-screen', view.HTMLDivElement),
    roomLabel: required('#room-label', view.HTMLSpanElement),
    roomTopic: required('#room-topic', view.HTMLButtonElement),
    homeLink: required('#home-link', view.HTMLAnchorElement),
    nameInput: required('#name-input', view.HTMLInputElement),
    roomInput: required('#room-input', view.HTMLInputElement),
    joinBtn: required('#join-btn', view.HTMLButtonElement),
    videoGrid: required('#video-grid', view.HTMLDivElement),
    participantList: required('#participant-list', view.HTMLUListElement),
    chatMessages: required('#chat-messages', view.HTMLDivElement),
    chatInput: required('#chat-input', view.HTMLTextAreaElement),
    chatSendBtn: required('#chat-send-btn', view.HTMLButtonElement),
    micBtn: required('#mic-btn', view.HTMLButtonElement),
    camBtn: required('#cam-btn', view.HTMLButtonElement),
    screenBtn: required('#screen-btn', view.HTMLButtonElement),
    settingsBtn: required('#settings-btn', view.HTMLButtonElement),
    leaveBtn: required('#leave-btn', view.HTMLButtonElement),
    qualityIndicator: required('#quality-indicator', view.HTMLSpanElement),
    scrollBottomBtn: required('#scroll-bottom-btn', view.HTMLButtonElement),
    unreadBadge: required('#unread-badge', view.HTMLSpanElement),
    handBtn: required('#hand-btn', view.HTMLButtonElement),
    roomSettingsBtn: required('#room-settings-btn', view.HTMLButtonElement),
    roomTools: required('#room-tools', view.HTMLDivElement),
    rosterToggleBtn: required('#toggle-roster', view.HTMLButtonElement),
    chatToggleBtn: required('#toggle-chat', view.HTMLButtonElement),
    lobbyScreen: required('#lobby-screen', view.HTMLDivElement),
    lobbyRoomName: required('#lobby-room-name', view.HTMLHeadingElement),
    lobbyTopic: required('#lobby-topic', view.HTMLParagraphElement),
    lobbyCount: required('#lobby-count', view.HTMLParagraphElement),
    lobbyCancelBtn: required('#lobby-cancel-btn', view.HTMLButtonElement),
    lobbyListEl: required('#lobby-list', view.HTMLUListElement),
    lobbyEmpty: required('#lobby-empty', view.HTMLDivElement),
    authBarGuest: required('#auth-bar-guest', view.HTMLDivElement),
    authBarUser: required('#auth-bar-user', view.HTMLDivElement),
    authDisplayName: required('#auth-display-name', view.HTMLSpanElement),
    authAvatar: required('#auth-avatar', view.HTMLSpanElement),
    signInBtn: required('#sign-in-btn', view.HTMLButtonElement),
    logoutBtn: required('#logout-btn', view.HTMLButtonElement),
    roomBrowser: required('#room-browser', view.HTMLDivElement),
    directoryToolbar: required('#directory-toolbar', view.HTMLDivElement),
    roomSearchInput: required('#room-search-input', view.HTMLInputElement),
    roomList: required('#room-list', view.HTMLDivElement),
    roomLoadMore: required('#room-load-more', view.HTMLButtonElement),
    createRoomBtn: required('#create-room-btn', view.HTMLButtonElement),
    serverMode: required('#server-mode', view.HTMLParagraphElement),
    capabilitiesRetry: required('#server-features-retry', view.HTMLButtonElement),
    authRestoreNotice: required('#auth-restore-notice', view.HTMLDivElement),
    authRestoreStatus: required('#auth-restore-status', view.HTMLParagraphElement),
    authRestoreRetry: required('#auth-restore-retry', view.HTMLButtonElement),
    loginModal: required('#login-modal', view.HTMLDialogElement),
    loginClose: required('#login-close', view.HTMLButtonElement),
    loginEmail: required('#login-email', view.HTMLInputElement),
    loginPassword: required('#login-password', view.HTMLInputElement),
    loginSubmit: required('#login-submit', view.HTMLButtonElement),
    loginError: required('#login-error', view.HTMLDivElement),
    loginPasskeyBtn: required('#login-passkey-btn', view.HTMLButtonElement),
    loginToRegister: required('#login-to-register', view.HTMLButtonElement),
    registerModal: required('#register-modal', view.HTMLDialogElement),
    registerClose: required('#register-close', view.HTMLButtonElement),
    registerEmail: required('#register-email', view.HTMLInputElement),
    registerName: required('#register-name', view.HTMLInputElement),
    registerPassword: required('#register-password', view.HTMLInputElement),
    registerConfirm: required('#register-confirm', view.HTMLInputElement),
    registerInvite: required('#register-invite', view.HTMLInputElement),
    registerSubmit: required('#register-submit', view.HTMLButtonElement),
    registerError: required('#register-error', view.HTMLDivElement),
    registerPasskeyBtn: required('#register-passkey-btn', view.HTMLButtonElement),
    registerToLogin: required('#register-to-login', view.HTMLButtonElement),
    createRoomModal: required('#create-room-modal', view.HTMLDialogElement),
    createRoomClose: required('#create-room-close', view.HTMLButtonElement),
    crId: required('#cr-id', view.HTMLInputElement),
    crName: required('#cr-name', view.HTMLInputElement),
    crTopic: required('#cr-topic', view.HTMLInputElement),
    crPassword: required('#cr-password', view.HTMLInputElement),
    crModerated: required('#cr-moderated', view.HTMLInputElement),
    crLobby: required('#cr-lobby', view.HTMLInputElement),
    crSecret: required('#cr-secret', view.HTMLInputElement),
    crGuests: required('#cr-guests', view.HTMLInputElement),
    createRoomSubmit: required('#create-room-submit', view.HTMLButtonElement),
    createRoomError: required('#create-room-error', view.HTMLDivElement),
    toastContainer: required('#toast-container', view.HTMLDivElement),
    sidebarCollapseBtn: required('#sidebar-collapse', view.HTMLButtonElement),
    communityActions: required('#community-actions', view.HTMLDivElement),
    roomVolumeSlot: required('#room-volume-slot', view.HTMLDivElement),
    homeSettingsHost: required('#home-settings-host', view.HTMLElement),
    sidebarElement: required('#sidebar', view.HTMLElement),
    micSetupBtn: required('#mic-setup-btn', view.HTMLButtonElement),
    copyRoomLink: required('#copy-room-link', view.HTMLButtonElement),
    shortcutsBtn: required('#shortcuts-btn', view.HTMLButtonElement),
    roomMoreBtn: required('#room-more-btn', view.HTMLButtonElement),
    roomMoreMenu: required('#room-more-menu', view.HTMLDivElement),
    accountMenuBtn: required('#account-menu-btn', view.HTMLButtonElement),
    accountMenu: required('#account-menu', view.HTMLDivElement),
    roomMoreRepair: required('#room-more-repair', view.HTMLDivElement),
    homeTools: required('#home-tools', view.HTMLDivElement),
    registrationHelp: required('#registration-help', view.HTMLParagraphElement),
    savedRoomPicker: required('#directory-rail', view.HTMLElement),
    savedRoomContainer: required('#directory-rail-body', view.HTMLDivElement),
    directoryRailToggle: required('#directory-rail-toggle', view.HTMLButtonElement),
    header: required('header', view.HTMLElement),
    headerRight: required('.header-right', view.HTMLDivElement),
    rosterToolLabel: required('#toggle-roster .tool-label', view.HTMLSpanElement),
    chatToolLabel: required('#toggle-chat .tool-label', view.HTMLSpanElement),
    roomNavigation: required('#room-tools .room-navigation', view.HTMLDivElement),
    lobbyTab: member(sidebarTabs, (element) => element.id === 'lobby-tab', '#lobby-tab'),
    chatTab: member(
      sidebarTabs,
      (element) => element.dataset['tab'] === 'chat',
      '#sidebar-tabs .tab[data-tab="chat"]',
    ),
    usersTab: member(
      sidebarTabs,
      (element) => element.dataset['tab'] === 'users',
      '#sidebar-tabs .tab[data-tab="users"]',
    ),
    chatPanel: member(tabContents, (element) => element.id === 'chat-panel', '#chat-panel'),
    lobbyPanel: member(tabContents, (element) => element.id === 'lobby-panel', '#lobby-panel'),
    settingsPanel: member(
      tabContents,
      (element) => element.id === 'settings-panel',
      '#settings-panel',
    ),
    usersPanel: member(tabContents, (element) => element.id === 'users-panel', '#users-panel'),
    sidebarTabs,
    tabContents,
  };
}
