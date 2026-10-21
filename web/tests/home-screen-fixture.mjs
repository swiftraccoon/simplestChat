import { createHomeScreen } from '../src/screens/home.ts';

/** Native modules with plain event targets; no DOM renderer or source evaluation is needed. */
export function homeScreenFixture({ capabilities = null } = {}) {
  const document = { activeElement: null };
  const elements = Object.fromEntries(
    [
      'signInBtn',
      'communityActions',
      'roomBrowser',
      'directoryToolbar',
      'createRoomBtn',
      'authBarGuest',
      'authBarUser',
      'authDisplayName',
      'authAvatar',
      'accountMenuBtn',
      'nameInput',
      'roomInput',
      'joinBtn',
      'serverMode',
      'capabilitiesRetry',
      'authRestoreNotice',
      'authRestoreStatus',
      'authRestoreRetry',
    ].map((name) => {
      const element = new EventTarget();
      Object.assign(element, {
        hidden: false,
        disabled: false,
        value: '',
        textContent: '',
        focus() {
          document.activeElement = this;
        },
        click() {
          if (!this.disabled) this.dispatchEvent(new Event('click'));
        },
      });
      return [name, element];
    }),
  );
  elements.authRestoreNotice.hidden = true;
  elements.capabilitiesRetry.hidden = true;
  const store = {
    capabilities,
    restoredAccount: false,
    room: null,
    navigationPending: false,
    departureInProgress: null,
    roomSelectionVersion: 0,
  };
  const observed = [];
  const errors = [];
  const state = { restoreHandler: null };
  const services = {
    document,
    auth: {
      isLoggedIn: false,
      displayName: null,
      userId: null,
      restoreState: 'idle',
      canCheckSharedSession: true,
      tryRestore: async () => 'unauthenticated',
      setOnRestoreStateChange: (handler) => {
        state.restoreHandler = handler;
      },
    },
    signaling: { connected: true, connect() {} },
    directoryScreen: { load: async () => {}, refreshSavedRooms() {} },
    community: { decorateOwnAvatar() {}, refresh() {} },
    pwa: { accountChanged() {} },
    accountUpdates: { synchronize() {} },
    accountPreferences: { refresh: async () => {} },
    notificationPreferences: { refresh: async () => {} },
    appNavigation: { synchronize() {} },
    participantHovercard: { refresh() {} },
    authScreen: { applyCapabilities() {} },
    navigation: { resumePendingJoin() {}, cancelPendingJoin() {} },
    loadCapabilities: async () => {
      throw new Error('Unexpected capability discovery');
    },
    previewPendingInvite: async () => {},
    reviewPendingContact() {},
    hasPendingInvite: () => false,
    observeUiTask(task, message) {
      observed.push({ task: task.catch((error) => errors.push({ error, message })), message });
    },
    asyncUiAction: (action, message) => () => services.observeUiTask(action(), message),
  };
  const screen = createHomeScreen(elements, store, services);
  return { screen, elements, store, services, document, observed, errors, state };
}
