import type { AccountPreferences } from '../account-preferences';
import type { AccountUpdates } from '../account-updates';
import type { ServerCapabilities } from '../api-validation';
import type { AppNavigation } from '../app-navigation';
import type { AuthManager, RestoreState } from '../auth';
import type { CommunityUI } from '../community-ui';
import { createScope } from '../disposable.ts';
import type { Elements } from '../dom';
import type { NotificationPreferences } from '../notification-preferences';
import type { ParticipantHovercard } from '../participant-hovercard';
import type { PwaControls } from '../pwa';
import type { RoomClient } from '../room';
import type { RoomNavigation } from '../room-navigation';
import type { SignalingClient } from '../signaling';
import type { AuthScreen } from './auth';
import type { DirectoryScreen } from './directory';

export type HomeElements = Pick<
  Elements,
  | 'signInBtn'
  | 'communityActions'
  | 'roomBrowser'
  | 'directoryToolbar'
  | 'createRoomBtn'
  | 'authBarGuest'
  | 'authBarUser'
  | 'authDisplayName'
  | 'authAvatar'
  | 'accountMenuBtn'
  | 'nameInput'
  | 'roomInput'
  | 'joinBtn'
  | 'serverMode'
  | 'capabilitiesRetry'
  | 'authRestoreNotice'
  | 'authRestoreStatus'
  | 'authRestoreRetry'
>;

/** Live application intent; the home screen never owns a second copy of identity or room state. */
export interface HomeStore {
  capabilities: ServerCapabilities | null;
  restoredAccount: boolean;
  room: RoomClient | null;
  navigationPending: boolean;
  departureInProgress: Promise<void> | null;
  roomSelectionVersion: number;
}

export interface HomeServices {
  readonly auth: Pick<
    AuthManager,
    | 'isLoggedIn'
    | 'displayName'
    | 'userId'
    | 'restoreState'
    | 'canCheckSharedSession'
    | 'tryRestore'
    | 'setOnRestoreStateChange'
  >;
  readonly signaling: Pick<SignalingClient, 'connected' | 'connect'>;
  readonly directoryScreen: Pick<DirectoryScreen, 'load' | 'refreshSavedRooms'>;
  readonly community: Pick<CommunityUI, 'decorateOwnAvatar' | 'refresh'>;
  readonly pwa: Pick<PwaControls, 'accountChanged'>;
  readonly accountUpdates: Pick<AccountUpdates, 'synchronize'>;
  readonly accountPreferences: Pick<AccountPreferences, 'refresh'>;
  readonly notificationPreferences: Pick<NotificationPreferences, 'refresh'>;
  readonly appNavigation: Pick<AppNavigation, 'synchronize'>;
  readonly participantHovercard: Pick<ParticipantHovercard, 'refresh'>;
  readonly authScreen: Pick<AuthScreen, 'applyCapabilities'>;
  readonly navigation: Pick<RoomNavigation, 'resumePendingJoin' | 'cancelPendingJoin'>;
  readonly document?: Pick<Document, 'activeElement'>;
  loadCapabilities(): Promise<ServerCapabilities>;
  previewPendingInvite(): Promise<void>;
  reviewPendingContact(): void;
  hasPendingInvite(): boolean;
  observeUiTask(task: Promise<unknown>, failureMessage: string): void;
  asyncUiAction(action: () => Promise<void>, failureMessage: string): () => void;
}

/**
 * Mount home controls without starting account or network work. Call start once
 * every service getter can resolve. Only verified restore outcomes release account
 * navigation; synchronous focus and rendering callbacks can retire that outcome.
 * Dispose removes listeners and prevents late discovery results from changing UI.
 */
export function createHomeScreen(elements: HomeElements, store: HomeStore, services: HomeServices) {
  const scope = createScope();
  const browserDocument = services.document ?? document;
  let started = false;
  let capabilitiesLoading = false;

  function updateAuthUI(): void {
    if (scope.signal.aborted) return;
    const { auth } = services;
    const { capabilities } = store;
    elements.signInBtn.hidden = capabilities?.accounts !== true;
    elements.communityActions.hidden = capabilities?.accounts !== true;
    elements.roomBrowser.hidden = capabilities?.roomDirectory !== true;
    elements.directoryToolbar.hidden = elements.roomBrowser.hidden;
    elements.createRoomBtn.hidden = !auth.isLoggedIn || capabilities?.roomCreation !== true;
    if (auth.isLoggedIn) {
      elements.authBarGuest.hidden = true;
      elements.authBarUser.hidden = false;
      elements.authDisplayName.textContent = auth.displayName ?? '';
      services.community.decorateOwnAvatar(
        elements.authAvatar,
        auth.userId ?? '',
        auth.displayName ?? '',
      );
      if (auth.displayName && !elements.nameInput.value.trim())
        elements.nameInput.value = auth.displayName;
    } else {
      elements.authBarGuest.hidden = false;
      elements.authBarUser.hidden = true;
    }
    if (!elements.roomBrowser.hidden)
      services.observeUiTask(
        services.directoryScreen.load(),
        'Could not refresh the room directory',
      );
    updateJoinBtn();
    services.community.refresh();
    services.pwa.accountChanged();
    services.accountUpdates.synchronize();
    services.appNavigation.synchronize();
    services.observeUiTask(services.accountPreferences.refresh(), 'Could not refresh privacy');
    services.observeUiTask(
      services.notificationPreferences.refresh(),
      'Could not refresh notification rules',
    );
    services.directoryScreen.refreshSavedRooms();
    services.participantHovercard.refresh();
    updateRestoreUI();
  }

  /** A failed cookie check is unconfirmed sign-in, rather than confirmed sign-out. */
  function updateRestoreUI(): void {
    if (scope.signal.aborted) return;
    const { auth } = services;
    const state = auth.restoreState;
    const visible =
      !auth.isLoggedIn &&
      store.capabilities?.accounts !== false &&
      (state === 'checking' || state === 'unavailable');
    const returnFocus =
      !visible &&
      browserDocument.activeElement === elements.authRestoreRetry &&
      (auth.isLoggedIn || state === 'unauthenticated');
    elements.authRestoreNotice.hidden = !visible;
    elements.authRestoreRetry.disabled = state === 'checking' || !auth.canCheckSharedSession;
    elements.authRestoreStatus.textContent = visible
      ? state === 'checking'
        ? 'Checking saved sign-in…'
        : 'Couldn’t check your saved sign-in. Check your connection and try again.'
      : '';
    if (returnFocus) (auth.isLoggedIn ? elements.accountMenuBtn : elements.signInBtn).focus();
  }

  /** Runs synchronously inside the current restore's ownership notification. */
  function handleRestoreState(state: RestoreState): void {
    if (scope.signal.aborted) return;
    const { auth } = services;
    if (state === 'checking' || state === 'unavailable' || state === 'superseded')
      store.restoredAccount = false;
    updateRestoreUI();
    if (scope.signal.aborted || auth.restoreState !== state) return;
    if (state === 'unavailable') {
      services.signaling.connect();
      return;
    }
    if (state !== 'restored' && state !== 'unauthenticated') return;
    store.restoredAccount = true;
    updateAuthUI();
    if (scope.signal.aborted || auth.restoreState !== state) return;
    services.reviewPendingContact();
    if (state === 'unauthenticated') services.signaling.connect();
    if (services.hasPendingInvite())
      services.observeUiTask(services.previewPendingInvite(), 'Invitation not reviewed');
  }

  /** Explicit retries share the same bounded cookie check as initial restoration. */
  function restoreSavedSignIn(): void {
    const { auth } = services;
    if (
      scope.signal.aborted ||
      auth.isLoggedIn ||
      auth.restoreState === 'checking' ||
      !auth.canCheckSharedSession ||
      store.capabilities?.accounts === false
    )
      return;
    services.observeUiTask(auth.tryRestore(), 'Could not check saved sign-in.');
  }

  /** Apply a verified contract before any account or directory intent resumes. */
  function applyCapabilities(value: ServerCapabilities): void {
    if (scope.signal.aborted) return;
    store.capabilities = value;
    elements.signInBtn.hidden = !value.accounts;
    services.authScreen.applyCapabilities(value, services.hasPendingInvite());
    elements.serverMode.hidden = value.accounts || value.roomDirectory;
    elements.serverMode.textContent = value.adHocRooms
      ? 'Guest rooms are available. Enter your name and a room ID to join.'
      : 'Rooms are not available on this server yet. Contact its host for help.';
    updateAuthUI();
  }

  /** A single bounded discovery read; failure never guesses available actions. */
  async function loadCapabilities(): Promise<void> {
    if (scope.signal.aborted || store.capabilities || capabilitiesLoading) return;
    capabilitiesLoading = true;
    elements.capabilitiesRetry.disabled = true;
    elements.serverMode.hidden = false;
    elements.serverMode.textContent = 'Checking server features…';
    try {
      const value = await services.loadCapabilities();
      if (scope.signal.aborted) return;
      applyCapabilities(value);
      if (scope.signal.aborted) return;
      services.navigation.resumePendingJoin();
      services.observeUiTask(services.previewPendingInvite(), 'Invitation not reviewed');
      services.reviewPendingContact();
    } catch {
      if (!scope.signal.aborted)
        elements.serverMode.textContent =
          'Server features could not be loaded. Check your connection and try again.';
    } finally {
      capabilitiesLoading = false;
      if (!scope.signal.aborted) {
        elements.capabilitiesRetry.disabled = false;
        if (store.capabilities && browserDocument.activeElement === elements.capabilitiesRetry)
          elements.nameInput.focus();
        elements.capabilitiesRetry.hidden = store.capabilities !== null;
      }
    }
  }

  function updateJoinBtn(): void {
    if (scope.signal.aborted) return;
    elements.joinBtn.disabled =
      store.capabilities === null ||
      (!store.capabilities.roomDirectory && !store.capabilities.adHocRooms) ||
      store.navigationPending ||
      store.departureInProgress !== null ||
      store.room !== null ||
      !services.signaling.connected ||
      !elements.nameInput.value.trim() ||
      !/^[A-Za-z0-9_-]{1,128}$/.test(elements.roomInput.value.trim());
  }

  elements.capabilitiesRetry.addEventListener(
    'click',
    services.asyncUiAction(loadCapabilities, 'Could not load server features'),
    { signal: scope.signal },
  );
  elements.authRestoreRetry.addEventListener('click', restoreSavedSignIn, { signal: scope.signal });
  elements.nameInput.addEventListener(
    'input',
    () => {
      updateJoinBtn();
      services.navigation.resumePendingJoin();
    },
    { signal: scope.signal },
  );
  elements.roomInput.addEventListener(
    'input',
    () => {
      services.navigation.cancelPendingJoin();
      store.roomSelectionVersion++;
      updateJoinBtn();
    },
    { signal: scope.signal },
  );
  for (const input of [elements.nameInput, elements.roomInput])
    input.addEventListener(
      'keydown',
      (event) => {
        if (event.key === 'Enter') elements.joinBtn.click();
      },
      { signal: scope.signal },
    );

  function start(): void {
    if (scope.signal.aborted || started) return;
    started = true;
    services.auth.setOnRestoreStateChange(handleRestoreState);
    services.observeUiTask(loadCapabilities(), 'Could not load server features');
    restoreSavedSignIn();
  }
  scope.onDispose(() => {
    if (started) services.auth.setOnRestoreStateChange(null);
  });

  return {
    updateAuthUI,
    updateRestoreUI,
    handleRestoreState,
    restoreSavedSignIn,
    applyCapabilities,
    loadCapabilities,
    updateJoinBtn,
    start,
    dispose: () => scope.dispose(),
  };
}

export type HomeScreen = ReturnType<typeof createHomeScreen>;
