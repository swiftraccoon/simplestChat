import type { AuthManager } from '../auth';
import type { AuthScreen } from './auth';
import type { AppNavigation } from '../app-navigation';
import type { RoomNavigation } from '../room-navigation';
import type { CommunityUI } from '../community-ui';
import type { RoomClient } from '../room';
import type { ServerCapabilities } from '../api-validation';
import { api, button, el, modal } from '../ui';

interface InvitationStore {
  readonly capabilities: ServerCapabilities | null;
  readonly restoredAccount: boolean;
  readonly room: RoomClient | null;
}
interface InvitationServices {
  readonly auth: Pick<AuthManager, 'userId' | 'jwt' | 'isLoggedIn'>;
  readonly navigation: Pick<RoomNavigation, 'revision' | 'selectRoom' | 'cancelPendingJoin'>;
  readonly appNavigation: Pick<AppNavigation, 'preserveLocation' | 'retire'>;
  readonly community: Pick<CommunityUI, 'requestContact'>;
  readonly authScreen: Pick<AuthScreen, 'open' | 'setRegistrationInvite'>;
  takeContactOffer: () => string | null;
  notify: (message: string, duration?: number, kind?: 'info' | 'error') => void;
  observe: (task: Promise<unknown>, message: string) => void;
}

/** Fragment offers own their account/session and navigation intent; acceptance never joins. */
export function createInvitations(store: InvitationStore, services: InvitationServices) {
  // Invitation secrets stay in the fragment and are removed before room navigation.
  let pendingInvite: string | null = null;
  let pendingContactOffer = services.takeContactOffer();

  function reviewPendingContact(): void {
    if (!pendingContactOffer || !store.capabilities?.accounts || !store.restoredAccount) return;
    if (!services.auth.isLoggedIn) {
      services.authScreen.open('login');
      return;
    }
    const peer = pendingContactOffer;
    pendingContactOffer = null;
    services.community.requestContact(peer);
  }
  let pendingInviteKind: 'room' | 'registration' = 'room';
  let inviteAccountEpoch = 0;
  let inviteView: ReturnType<typeof modal> | null = null;
  function readInviteLink(returnRoom = ''): void {
    const url = new URL(window.location.href);
    const fragment = new URLSearchParams(url.hash.slice(1));
    const registration = fragment.get('register-invite');
    const roomCode = fragment.get('invite');
    const code = registration ?? roomCode;
    if (code === null) return;
    url.hash = returnRoom;
    window.history.replaceState(null, '', `${url.pathname}${url.search}${url.hash}`);
    const normalized = code.trim().toLowerCase();
    if (!/^[abcdefghjkmnpqrstuvwxyz023456789]{32}$/.test(normalized)) {
      services.notify('This invitation link is not valid', 4000, 'error');
      return;
    }
    pendingInvite = normalized;
    pendingInviteKind = registration !== null ? 'registration' : 'room';
    services.authScreen.setRegistrationInvite(normalized);
  }

  /** Fragment links can arrive without reloading an already open application. */
  function consumeInviteLocation(selectedRoom: string): boolean {
    const fragment = new URLSearchParams(window.location.hash.slice(1));
    if (fragment.has('contact')) {
      pendingContactOffer = services.takeContactOffer();
      services.appNavigation.preserveLocation();
      services.navigation.cancelPendingJoin();
      if (pendingContactOffer) reviewPendingContact();
      else services.notify('This contact link is not valid', 4000, 'error');
      return true;
    }
    if (!fragment.has('invite') && !fragment.has('register-invite')) return false;
    if (inviteView) {
      services.appNavigation.retire(inviteView.dialog);
      inviteView.close();
    }
    inviteView = null;
    pendingInvite = null;
    services.navigation.cancelPendingJoin();
    readInviteLink(selectedRoom);
    services.appNavigation.preserveLocation();
    services.observe(previewPendingInvite(), 'Invitation not reviewed');
    return true;
  }

  /** A link can preview an offer; only its explicitly clicked acceptance mutates membership. */
  async function previewPendingInvite(): Promise<void> {
    const code = pendingInvite;
    if (!code || !store.capabilities || inviteView?.dialog.open) return;
    if (!store.capabilities.accounts) {
      pendingInvite = null;
      services.notify('Invitations are unavailable on this server.', 4000, 'error');
      return;
    }
    if (!store.restoredAccount) return;
    if (pendingInviteKind === 'registration') {
      services.authScreen.open('register');
      return;
    }
    if (!services.auth.isLoggedIn) {
      const view = modal('Room invitation');
      inviteView = view;
      view.body.append(
        el(
          'p',
          'Sign in to review the room and offered role. Opening this link does not accept it or join a room.',
        ),
        button('Sign in to review', () => {
          view.close();
          services.authScreen.open('login');
        }),
      );
      return;
    }
    pendingInvite = null;
    const epoch = inviteAccountEpoch;
    const userId = services.auth.userId;
    const revision = services.navigation.revision;
    const view = modal('Review room invitation');
    inviteView = view;
    const ownsIntent = (): boolean =>
      view.dialog.open &&
      inviteView === view &&
      epoch === inviteAccountEpoch &&
      userId === services.auth.userId &&
      services.auth.isLoggedIn &&
      revision === services.navigation.revision;
    const description = el('p', 'Loading invitation…');
    view.body.append(description);
    try {
      const offer = await api.previewInvite(services.auth.jwt, code);
      if (!ownsIntent()) return;
      description.textContent = `Room: ${offer.display_name} (${offer.room_id}). Offered role: ${offer.role}.`;
      view.body.append(
        el(
          'p',
          'Accept only if you intended to receive this role. Acceptance never joins the room or starts your microphone or camera. Existing permissions may differ when reusing an invitation.',
        ),
      );
      const accept = button(
        'Accept invitation',
        () => {
          if (!ownsIntent() || accept.disabled) return;
          accept.disabled = true;
          services.observe(
            (async () => {
              try {
                const accepted = await api.redeemInvite(services.auth.jwt, code);
                if (!ownsIntent()) return;
                view.close();
                const alreadyJoined = store.room?.currentRoomId === accepted.room_id;
                if (!alreadyJoined) services.navigation.selectRoom(accepted.room_id);
                services.notify(
                  `Invitation confirmed for ${accepted.display_name}. Current permissions apply.${alreadyJoined ? '' : ' Choose Join when ready.'}`,
                );
              } catch (error) {
                if (!ownsIntent()) return;
                view.error.textContent =
                  error instanceof Error ? error.message : 'The invitation could not be accepted';
                view.error.hidden = false;
                // Retrying remains a separate user action, never an automatic mutation.
                accept.disabled = false;
              }
            })(),
            'The invitation could not be accepted',
          );
        },
        'btn-primary',
      );
      view.body.append(accept);
    } catch (error) {
      if (!ownsIntent()) return;
      description.textContent = 'The invitation could not be reviewed.';
      view.error.textContent =
        error instanceof Error ? error.message : 'Please try the link again.';
      view.error.hidden = false;
    }
  }
  return {
    readInviteLink,
    consumeInviteLocation,
    previewPendingInvite,
    reviewPendingContact,
    get pendingCode() {
      return pendingInvite;
    },
    get pendingKind() {
      return pendingInviteKind;
    },
    get hasPending() {
      return pendingInvite !== null;
    },
    get view() {
      return inviteView;
    },
    get accountEpoch() {
      return inviteAccountEpoch;
    },
    spendRegistrationInvite(code: string) {
      if (pendingInvite && code.trim().toLowerCase() === pendingInvite) pendingInvite = null;
    },
    accountChanged() {
      inviteAccountEpoch++;
      inviteView?.close();
      inviteView = null;
    },
    selectedRoomChanged() {
      if (inviteView?.dialog.open) {
        inviteView.close();
        pendingInvite = null;
      }
    },
    dispose() {
      inviteAccountEpoch++;
      inviteView?.close();
      inviteView = null;
      pendingInvite = null;
      pendingContactOffer = null;
    },
  };
}
