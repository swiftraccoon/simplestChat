import type { AuthManager } from './auth';
import { avatarColors } from './avatar-colors';
import { openPrivateInbox, onInboxChanged } from './chat-history';
import { AccountSecurityFlow, mountAccountSecurity } from './account-security';
import { mountAccountSessions } from './account-sessions';
import { mountSavedRooms, createFavoriteButton, offerContact } from './discovery';
import { appearancePicker } from './appearance';
import type { RoomClient } from './room';
import type { AccountProfile, PublicProfile, RoomListItem } from './protocol';
import type { RegistrationInvite, RoomInvite } from './api-validation';
import {
  api,
  asyncButton,
  button,
  busy,
  el,
  field,
  input,
  modal,
  rasterUpload,
  safeRasterUrl,
  validatePassword,
} from './ui';
import {
  actionRow,
  fieldRow,
  group,
  markSaved,
  note,
  type SheetScreen,
  type SheetSection,
} from './settings-sheet';

interface Options {
  /** Opens the settings sheet at a section; the Account button uses it. */
  openSettings?: (section: string, opener: HTMLElement | null) => void;
  /** Called when the signed-in identity changes; account screens close. */
  onAccountChanged?: () => void;
  mountConversationNotifications?: (
    container: HTMLElement,
    peerId: string,
    current: () => boolean,
  ) => void;
  auth: AuthManager;
  getRoom: () => RoomClient | null;
  notify: (message: string) => void;
  onProfileChanged: (profile: AccountProfile) => void;
  onRoomsChanged: () => void;
  onRoomDeleted: (id: string) => Promise<void>;
  onSignedOut: () => Promise<void>;
  /** Sign out and clear what this browser remembers; main.ts owns the storage. */
  onForgetDevice: () => void;
  /** Leave any current room and join this one as the account. */
  onJoinRoom: (id: string) => void;
}

/** Roles an invitation may grant, as the server numbers them. */
const INVITE_ROLES: readonly { value: number; label: string }[] = [
  { value: 2, label: 'Member' },
  { value: 3, label: 'Moderator' },
  { value: 4, label: 'Admin' },
];

const ROLES = ['guest', 'user', 'member', 'moderator', 'admin', 'owner'];
/** History actions as the server names them, read as what happened to the target. */
const ACTION_LABELS: Record<string, string> = {
  message_removed: 'message removed',
  kick: 'kicked',
  ban: 'banned',
  unban: 'unbanned',
  cam_ban: 'camera banned',
  cam_unban: 'camera unbanned',
  text_mute: 'text muted',
  text_unmute: 'text unmuted',
  report_resolved: 'report resolved',
  report_dismissed: 'report dismissed',
};
const actionLabel = (action: string): string => ACTION_LABELS[action] ?? action.replace(/_/g, ' ');
/** Where a list of invitations lives: a modal or a sheet screen. */
interface Surface {
  body: HTMLElement;
  error: HTMLElement;
  isOpen: () => boolean;
}

export class CommunityUI {
  private readonly messagesButton = button('Messages', () =>
    openPrivateInbox({
      getToken: () => this.options.auth.jwt,
      getAccountId: () => this.options.auth.userId,
      mountConversationNotifications: (container, peerId, current) =>
        this.options.mountConversationNotifications?.(container, peerId, current),
    }),
  );
  private inboxController = new AbortController();
  private inboxPending: Promise<void> | null = null;
  private inboxRefreshAgain = false;
  private readonly accountButton = button('Account', () =>
    this.options.openSettings?.('profile', this.accountButton),
  );
  private readonly roomsButton = button('My rooms', () => {
    const generation = this.generation;
    this.openRooms().catch((error) => {
      if (generation === this.generation)
        this.options.notify(error instanceof Error ? error.message : 'Rooms unavailable');
    });
  });
  private readonly nicknameButton = button('Nickname', () => this.openNickname());
  private readonly manageButton = button('Manage room', () =>
    this.options.openSettings?.('room-people', this.manageButton),
  );
  private profiles = new Map<string, Promise<PublicProfile | null>>();
  private avatarVersions = new WeakMap<HTMLElement, number>();
  private rosterProfiles: Set<string> | null = null;
  private profileActive = 0;
  private profileWaiting: (() => void)[] = [];
  private generation = 0;
  private accountGeneration = 0;
  private accountIdentity = '';
  private accountSeen = false;
  /** An owned room chosen from My rooms while no room is open. */
  private editingRoom: RoomListItem | null = null;
  private identity = '';

  constructor(private readonly options: Options) {
    const accountActions = document.getElementById('community-actions')!;
    accountActions.append(this.accountButton, this.messagesButton, this.roomsButton);
    // Room-scoped actions belong with the room tools; the header keeps account actions.
    (document.getElementById('room-actions') ?? accountActions).append(
      this.nicknameButton,
      this.manageButton,
    );
    const recovery = button('Recover with a saved key', () => this.openRecovery(), 'auth-link-btn');
    document.querySelector('#login-modal .auth-alt-actions')!.append(recovery);
    onInboxChanged(() => {
      this.refreshMessages().catch(() => {});
    });
    this.refresh();
  }

  refresh(): void {
    const room = this.options.getRoom();
    const accountIdentity = this.options.auth.userId ?? '';
    const accountChanged = accountIdentity !== this.accountIdentity;
    if (accountChanged) {
      this.accountIdentity = accountIdentity;
      this.accountGeneration++;
      this.inboxController.abort();
      this.inboxController = new AbortController();
      this.inboxPending = null;
      this.messagesButton.textContent = 'Messages';
      this.messagesButton.setAttribute('aria-label', 'Messages');
      // The first refresh learns the identity; later changes close account screens.
      if (this.accountSeen) this.options.onAccountChanged?.();
      this.accountSeen = true;
    }
    const identity = `${this.options.auth.userId ?? ''}:${room?.currentRoomId ?? ''}:${room?.localParticipantId ?? ''}`;
    if (identity !== this.identity) {
      this.identity = identity;
      this.generation++;
      this.profiles.clear();
      this.rosterProfiles = null;
      for (const resume of this.profileWaiting.splice(0)) resume();
      document.querySelectorAll<HTMLDialogElement>('.community-dialog').forEach((dialog) => {
        if (accountChanged || dialog.getAttribute('data-account-dialog') !== 'true') dialog.close();
      });
    }
    this.accountButton.hidden = !this.options.auth.isLoggedIn;
    this.messagesButton.hidden = !this.options.auth.isLoggedIn;
    this.roomsButton.hidden = !this.options.auth.isLoggedIn;
    this.nicknameButton.hidden = !room?.localParticipantId;
    this.manageButton.hidden = !room?.localParticipantId || ROLES.indexOf(room.role) < 3;
    if (accountChanged) this.refreshMessages().catch(() => {});
  }

  /** Refreshes the existing header badge without revealing another account's unread state. */
  refreshMessages(): Promise<void> {
    const token = this.options.auth.jwt;
    const account = this.options.auth.userId;
    if (!token || !account || document.hidden) return Promise.resolve();
    if (this.inboxPending) {
      this.inboxRefreshAgain = true;
      return this.inboxPending;
    }
    const generation = this.accountGeneration;
    const task = api
      .inboxUnread(token, this.inboxController.signal)
      .then((result) => {
        if (generation !== this.accountGeneration || account !== this.options.auth.userId) return;
        this.messagesButton.textContent = result.unreadCount
          ? `Messages (${result.unreadCount >= 1000 ? '1,000+' : result.unreadCount})`
          : 'Messages';
        this.messagesButton.setAttribute(
          'aria-label',
          result.unreadCount
            ? `Messages, ${result.unreadCount >= 1000 ? '1,000 or more' : result.unreadCount} unread`
            : 'Messages',
        );
      })
      .catch(() => {
        /* Keep the last confirmed count until the next refresh. */
      })
      .finally(() => {
        if (this.inboxPending !== task) return;
        this.inboxPending = null;
        if (this.inboxRefreshAgain) {
          this.inboxRefreshAgain = false;
          this.refreshMessages().catch(() => {});
        }
      });
    this.inboxPending = task;
    return task;
  }

  requestContact(peer: string): void {
    offerContact(peer, {
      getToken: () => this.options.auth.jwt,
      getAccountId: () => this.options.auth.userId,
    });
  }

  /** Cache only the current roster, with a hard memory bound; larger rosters keep initials. */
  retainProfiles(ids: Iterable<string>): void {
    const retained = new Set<string>();
    for (const id of ids) {
      retained.add(id);
      if (retained.size === 512) break;
    }
    this.rosterProfiles = retained;
    for (const id of this.profiles.keys()) if (!retained.has(id)) this.profiles.delete(id);
  }

  decorateAvatar(node: HTMLElement, id: string, authenticated: boolean): void {
    if (
      !authenticated ||
      (this.rosterProfiles && !this.rosterProfiles.has(id)) ||
      (!this.profiles.has(id) && this.profiles.size >= 512)
    )
      return;
    const version = (this.avatarVersions.get(node) ?? 0) + 1;
    this.avatarVersions.set(node, version);
    this.profile(id)
      .then((profile) => {
        if (!node.isConnected || this.avatarVersions.get(node) !== version || !profile) return;
        if (!profile.avatar_url || !safeRasterUrl(profile.avatar_url)) {
          const initial = node.dataset?.['initial'];
          if (node.querySelector('img') && initial !== undefined) node.textContent = initial;
          return;
        }
        if (node.querySelector('img')?.getAttribute('src') === profile.avatar_url) return;
        const image = el('img');
        image.src = profile.avatar_url;
        image.alt = '';
        image.className = 'account-avatar';
        node.replaceChildren(image);
      })
      .catch(() => {
        // Avatars are optional decoration; preserve the existing initials/name.
        // Explicit profile visits report load failures inside their own dialog.
      });
  }

  /**
   * The account's own avatar in the header: its initial on the name's tone at once, then the
   * profile's chosen color, or its picture when it has one.
   */
  decorateOwnAvatar(node: HTMLElement, id: string, name: string): void {
    const initial = [...name.trim()][0]?.toLocaleUpperCase() ?? '';
    node.textContent = initial;
    node.dataset['initial'] = initial;
    Object.assign(node.style, avatarColors(name));
    const version = (this.avatarVersions.get(node) ?? 0) + 1;
    this.avatarVersions.set(node, version);
    this.profile(id)
      .then((profile) => {
        if (!node.isConnected || this.avatarVersions.get(node) !== version || !profile) return;
        Object.assign(node.style, avatarColors(name, profile.profile_style.color));
        if (!profile.avatar_url || !safeRasterUrl(profile.avatar_url)) return;
        const image = el('img');
        image.src = profile.avatar_url;
        image.alt = '';
        image.className = 'account-avatar';
        node.replaceChildren(image);
      })
      .catch(() => {
        // The initial on its tone stands in for a profile that cannot be read.
      });
  }

  async showProfile(id: string): Promise<void> {
    const view = modal('Profile');
    this.profiles.delete(id); // Explicit profile visits refresh the cached account details.
    view.body.append(el('p', 'Loading profile…'));
    const profile = await this.loadProfile(id, false);
    if (!view.dialog.open) return;
    view.body.replaceChildren();
    if (!profile) {
      view.body.append(el('p', 'This profile is unavailable.'));
      return;
    }
    if (safeRasterUrl(profile.avatar_url)) {
      const image = el('img');
      image.src = profile.avatar_url;
      image.alt = `${profile.display_name}’s avatar`;
      image.className = 'profile-image';
      view.body.append(image);
    }
    view.body.append(
      el('h3', profile.display_name),
      el('p', profile.bio || 'No bio yet.', 'profile-bio'),
    );
    view.body.append(el('p', 'Account profile · room nicknames may differ', 'setting-hint'));
  }

  /** Explicit card visits refresh profile edits while sharing the bounded read queue. */
  participantProfile(id: string): Promise<PublicProfile | null> {
    this.profiles.delete(id);
    return this.loadProfile(id, false);
  }

  report(id: string, name: string): void {
    const room = this.options.getRoom();
    if (!room) return;
    const view = modal(`Report ${name}`);
    const reason = el('textarea');
    reason.maxLength = 1000;
    reason.rows = 4;
    view.body.append(
      el(
        'p',
        'Your report and account/room identity will be visible to this room’s moderators. No private conversation is attached automatically.',
      ),
      field('Reason and details', reason),
    );
    const submit = asyncButton(
      'Send report',
      () =>
        busy(submit, view.error, async () => {
          if (!reason.value.trim()) throw new Error('Describe what happened');
          await room.requestSocial('reportParticipant', {
            targetParticipantId: id,
            reason: reason.value.trim(),
          });
          view.close();
          this.options.notify('Report sent to room moderators');
        }),
      (error) => this.showError(view.error, error),
      'btn-primary',
    );
    view.body.append(submit);
  }

  /** `reportId` names the open report the ban answers; `onDone` runs after it is applied. */
  ban(id: string, name: string, options: { reportId?: string; onDone?: () => void } = {}): void {
    const room = this.options.getRoom();
    if (!room) return;
    let pending = false;
    const membership = room.membershipVersion;
    const current = () => this.options.getRoom() === room && room.membershipVersion === membership;
    const view = modal(`Ban ${name}`, () => !pending);
    const reason = input('', 'text', 500);
    const duration = el('select');
    for (const [value, label] of [
      ['3600', '1 hour'],
      ['86400', '1 day'],
      ['604800', '7 days'],
      ['', 'Until removed'],
    ]) {
      const option = el('option', label);
      option.value = value!;
      duration.append(option);
    }
    view.body.append(field('Reason (optional)', reason), field('Duration', duration));
    const submit = asyncButton(
      'Ban from room',
      () =>
        busy(submit, view.error, async () => {
          if (pending || !view.dialog.open) return;
          if (!current()) {
            view.close();
            return;
          }
          pending = true;
          reason.disabled = true;
          duration.disabled = true;
          try {
            // A plain ban sends exactly the three arguments it always did.
            const linked: [] | [string] = options.reportId === undefined ? [] : [options.reportId];
            await room.ban(
              id,
              reason.value.trim() || undefined,
              duration.value ? Number(duration.value) : undefined,
              ...linked,
            );
            pending = false;
            view.close();
            options.onDone?.();
          } finally {
            pending = false;
            reason.disabled = false;
            duration.disabled = false;
            if (!current()) view.close();
          }
        }),
      (error) => this.showError(view.error, error),
      'btn-secondary danger',
    );
    view.body.append(submit);
  }

  /** At most eight profile reads in flight, with membership-owned queued requests. */
  private async loadProfile(id: string, cached = true): Promise<PublicProfile | null> {
    const generation = this.generation;
    while (this.profileActive >= 8) {
      await new Promise<void>((resolve) => {
        // A requested profile takes the next slot before optional avatar decoration.
        if (cached) this.profileWaiting.push(resolve);
        else this.profileWaiting.unshift(resolve);
      });
      if (generation !== this.generation || (cached && !this.profiles.has(id))) {
        this.profileWaiting.shift()?.();
        return null;
      }
    }
    this.profileActive++;
    try {
      return await api.publicProfile(id);
    } catch {
      return null;
    } finally {
      this.profileActive--;
      this.profileWaiting.shift()?.();
    }
  }

  private async profile(id: string): Promise<PublicProfile | null> {
    let promise = this.profiles.get(id);
    if (!promise) {
      promise = this.loadProfile(id);
      if (this.profiles.size < 512) this.profiles.set(id, promise);
    }
    return promise;
  }

  /** Account screens for the settings sheet; a pending mutation refuses to be left. */
  sections(): SheetSection[] {
    const available = (): boolean => this.options.auth.isLoggedIn;
    return [
      {
        id: 'profile',
        segment: 'you',
        group: 'Account',
        label: 'Profile',
        status: () => this.options.auth.displayName ?? '',
        available,
        screen: () => this.profileScreen(),
      },
      {
        id: 'security',
        segment: 'you',
        group: 'Account',
        label: 'Sign-in & security',
        available,
        screen: () => this.securityScreen(),
      },
      {
        id: 'invites',
        segment: 'you',
        group: 'Account',
        label: 'Invites',
        available,
        screen: () => this.invitesScreen(),
      },
    ];
  }

  /** Binds a screen to the account that opened it. */
  private accountScope(current: () => boolean): () => boolean {
    const accountId = this.options.auth.userId;
    const generation = this.accountGeneration;
    return () =>
      current() && generation === this.accountGeneration && this.options.auth.userId === accountId;
  }

  private profileScreen(): SheetScreen {
    let flow: AccountSecurityFlow | null = null;
    return {
      title: 'Profile',
      canLeave: () => flow?.canDismiss ?? true,
      leave: () => flow?.dispose(),
      body: async (context) => {
        const body = el('div', undefined, 'sheet-screen');
        const error = el('p', '', 'auth-error');
        error.hidden = true;
        error.setAttribute('role', 'alert');
        const stillCurrent = this.accountScope(() => context.current());
        const token = this.options.auth.jwt;
        try {
          const profile = await api.accountProfile(token);
          if (!stillCurrent()) return body;
          const name = input(profile.display_name, 'text', 64);
          const bio = el('textarea');
          bio.value = profile.bio;
          bio.maxLength = 1000;
          bio.rows = 3;
          bio.setAttribute('aria-label', 'Bio');
          const avatar = this.imagePicker(profile.avatar_url, error);
          const appearance = appearancePicker({
            label: 'Hovercard appearance',
            initial: profile.profile_style,
            name: () => name.value.trim(),
          });
          name.addEventListener('input', appearance.refreshPreview);
          const save = asyncButton(
            'Save profile',
            async () => {
              if (!stillCurrent() || !flow?.canStart) return;
              const update = {
                display_name: name.value.trim(),
                bio: bio.value.trim(),
                avatar_url: avatar.value(),
                profile_style: appearance.chosen(),
              };
              await flow.change(
                (currentToken, signal) => api.updateProfile(currentToken, update, signal),
                (updated) => {
                  this.profiles.delete(updated.id);
                  this.options.onProfileChanged(updated);
                  this.options.notify('Profile saved');
                },
              );
            },
            (failure) => this.showError(error, failure),
            'btn-primary sheet-button',
          );
          flow = new AccountSecurityFlow({
            token: () => this.options.auth.jwt,
            current: stillCurrent,
            changed: () => {
              save.disabled = !(flow?.canStart ?? false);
              if (flow?.message) {
                error.textContent = flow.message;
                error.hidden = false;
              }
            },
            completed: () => {},
          });
          const bioBlock = el('div', undefined, 'sheet-block');
          bioBlock.append(el('h3', 'Bio'), bio);
          body.append(
            el('p', profile.email, 'sheet-note'),
            avatar.wrapper,
            group(null, [fieldRow('Name', name)]),
            bioBlock,
            appearance.element,
            save,
            error,
          );
        } catch (failure) {
          if (stillCurrent()) {
            this.showError(error, failure);
            body.append(error);
          }
        }
        return body;
      },
    };
  }

  private securityScreen(): SheetScreen {
    let security: AccountSecurityFlow | null = null;
    let released = false;
    const leaves: (() => void)[] = [];
    return {
      title: 'Sign-in & security',
      canLeave: () => released || (security?.canDismiss ?? true),
      leave: () => {
        for (const callback of leaves.splice(0)) callback();
      },
      body: async (context) => {
        const body = el('div', undefined, 'sheet-screen');
        const error = el('p', '', 'auth-error');
        error.hidden = true;
        error.setAttribute('role', 'alert');
        const stillCurrent = this.accountScope(() => context.current());
        const finish = (message: string): void => {
          released = true;
          context.close();
          this.options.notify(message);
        };
        let sessions: ReturnType<typeof mountAccountSessions> | null = null;
        let changePassword: HTMLButtonElement | null = null;
        const passwordFields: HTMLInputElement[] = [];
        try {
          security = mountAccountSecurity({
            container: body,
            onLeave: (callback) => leaves.push(callback),
            token: () => this.options.auth.jwt,
            current: stillCurrent,
            interactionChanged: (canStart) => {
              if (changePassword) changePassword.disabled = !canStart;
              if (!canStart) for (const secret of passwordFields) secret.value = '';
              sessions?.refreshControls();
            },
            completed: (kind) => {
              const action = kind === 'replaced' ? 'Passkey replaced.' : 'Passkey removed.';
              finish(
                kind === 'replaced'
                  ? `${action} Sign in again with your new passkey.`
                  : `${action} Sign in again with a remaining sign-in method.`,
              );
              this.options.onSignedOut().catch(() => {
                this.options.notify(`${action} Reload to finish signing out.`);
              });
            },
          });
          await security.load();
          if (!stillCurrent()) return body;
          if (security.settings?.password_enabled) {
            const current = input('', 'password', 512);
            current.autocomplete = 'current-password';
            const password = input('', 'password', 512);
            password.autocomplete = 'new-password';
            const confirm = input('', 'password', 512);
            confirm.autocomplete = 'new-password';
            passwordFields.push(current, password, confirm);
            const change = asyncButton(
              'Change password',
              async () => {
                if (!stillCurrent() || !security?.canStart) return;
                validatePassword(password.value, confirm.value);
                const update = {
                  current_password: current.value,
                  new_password: password.value,
                };
                await security.change(
                  (currentToken, signal) => api.changePassword(currentToken, update, signal),
                  () => {
                    finish('Password changed. Sign in again with your new password.');
                    this.options
                      .onSignedOut()
                      .catch(() =>
                        this.options.notify('Password changed. Reload to finish signing out.'),
                      );
                  },
                );
              },
              (failure) => this.showError(error, failure),
              'sheet-button',
            );
            changePassword = change;
            leaves.push(() => {
              current.value = '';
              password.value = '';
              confirm.value = '';
            });
            body.append(
              group('Password', [
                fieldRow('Current password', current),
                fieldRow('New password', password),
                fieldRow('Confirm new password', confirm),
              ]),
              note('Changing your password signs out every session.'),
              change,
            );
          }
          sessions = mountAccountSessions({
            container: body,
            onLeave: (callback) => leaves.push(callback),
            security,
            token: () => this.options.auth.jwt,
            current: stillCurrent,
            notify: this.options.notify,
            signedOut: () => {
              finish('Session signed out.');
              this.options.onSignedOut().catch(() => {
                this.options.notify('Session signed out. Reload to finish signing out.');
              });
            },
          });
          body.append(
            group(null, [
              actionRow(
                'Sign out and forget this device',
                () => {
                  released = true;
                  context.close();
                  this.options.onForgetDevice();
                },
                { danger: true },
              ),
            ]),
            note(
              'Forgetting this device removes the saved name, devices, layout and chat preferences from this browser.',
            ),
            error,
          );
        } catch (failure) {
          if (stillCurrent()) {
            this.showError(error, failure);
            body.append(error);
          }
        }
        return body;
      },
    };
  }

  /** Registration invitations: the way in while registration is closed. */
  private invitesScreen(): SheetScreen {
    return {
      title: 'Invites',
      body: (context) => {
        const body = el('div', undefined, 'sheet-screen');
        const error = el('p', '', 'auth-error');
        error.hidden = true;
        error.setAttribute('role', 'alert');
        const stillCurrent = this.accountScope(() => context.current());
        const token = this.options.auth.jwt;
        const surface: Surface = { body, error, isOpen: stillCurrent };
        const invites = el('div', undefined, 'sheet-group');
        const refreshInvites = async (): Promise<void> => {
          const codes = await api.registrationInvites(token);
          if (!stillCurrent()) return;
          invites.replaceChildren();
          if (!codes.length) invites.append(note('No unused invite codes.'));
          for (const code of codes)
            invites.append(
              this.inviteRow(code, surface, refreshInvites, () =>
                api.revokeRegistrationInvite(token, code.id),
              ),
            );
        };
        const mint = asyncButton(
          'New invite code',
          () =>
            busy(mint, error, async () => {
              if (!stillCurrent()) return;
              const invite = await api.createRegistrationInvite(this.options.auth.jwt);
              if (!stillCurrent()) return;
              this.showCreatedInvite(invite.code, surface, 'registration');
              await refreshInvites();
            }),
          (failure) => this.showError(error, failure),
          'sheet-button',
        );
        body.append(
          note(
            'A code lets one person create an account while registration is closed. Hold up to five; each lasts a week.',
          ),
          mint,
          invites,
          error,
        );
        refreshInvites().catch((failure) => {
          if (stillCurrent()) this.showError(error, failure);
        });
        return body;
      },
    };
  }

  private openRecovery(): void {
    const view = modal('Recover account');
    const email = input('', 'email', 254);
    email.autocomplete = 'username';
    const key = input('', 'password', 512);
    key.autocomplete = 'off';
    const password = input('', 'password', 512);
    password.autocomplete = 'new-password';
    const confirm = input('', 'password', 512);
    confirm.autocomplete = 'new-password';
    view.body.append(
      el(
        'p',
        'Use the one-time key you previously saved from Account settings. Without a saved key, use your existing password or passkey. This does not send a recovery email.',
      ),
      field('Email', email),
      field('Saved recovery key', key),
      field('New password', password),
      field('Confirm new password', confirm),
    );
    const submit = asyncButton(
      'Reset password with key',
      () =>
        busy(submit, view.error, async () => {
          validatePassword(password.value, confirm.value);
          await api.redeemRecovery({
            email: email.value.trim(),
            recovery_key: key.value.trim(),
            new_password: password.value,
          });
          key.value = '';
          view.close();
          this.options.notify('Password reset. Sign in with your new password.');
        }),
      (error) => this.showError(view.error, error),
      'btn-primary',
    );
    view.body.append(submit);
  }

  private openNickname(): void {
    const room = this.options.getRoom();
    if (!room) return;
    const view = modal('Room nickname');
    const nickname = input(room.nickname, 'text', 64);
    view.body.append(
      field('Nickname', nickname),
      el('p', 'Changes your name in this room, not your account identity.', 'setting-hint'),
    );
    const save = asyncButton(
      'Change nickname',
      () =>
        busy(save, view.error, async () => {
          await room.requestSocial('changeNickname', { nickname: nickname.value.trim() });
          view.close();
        }),
      (error) => this.showError(view.error, error),
      'btn-primary',
    );
    view.body.append(save);
  }

  private async openRooms(): Promise<void> {
    const view = modal('My rooms');
    const token = this.options.auth.jwt;
    const account = this.options.auth.userId;
    const current = (): boolean =>
      view.dialog.open && account === this.options.auth.userId && !!this.options.auth.jwt;
    view.dialog.setAttribute('data-account-dialog', 'true');
    const savedHost = el('div');
    const saved = mountSavedRooms(savedHost, {
      getToken: () => this.options.auth.jwt,
      getAccountId: () => this.options.auth.userId,
      current,
      onJoin: (id) => {
        view.close();
        this.options.onJoinRoom(id);
      },
    });
    view.dialog.addEventListener('close', saved.dispose, { once: true });
    const refresh = async (): Promise<void> => {
      const [rooms, shortcuts] = await Promise.all([
        api.ownRooms(token),
        api.savedRooms(token!, new AbortController().signal),
      ]);
      if (!current()) return;
      const favorites = new Set(
        shortcuts.rooms.filter((entry) => entry.favorite).map((entry) => entry.room.id),
      );
      view.body.replaceChildren(savedHost);
      if (!rooms.length)
        view.body.append(el('p', 'No owned rooms yet. Use Create Room on the join screen.'));
      else view.body.append(el('h3', 'Rooms you own'));
      for (const room of rooms) {
        const row = el('div', undefined, 'owned-room');
        row.append(
          el('h3', room.display_name),
          el(
            'p',
            `${room.id} · ${room.secret ? 'Unlisted' : 'Public'} · ${room.participant_count ?? '?'} online`,
          ),
        );
        row.append(
          button('Edit room', () => {
            view.close();
            this.editingRoom = room;
            this.options.openSettings?.('room-identity', null);
          }),
        );
        row.append(
          asyncButton(
            'Copy link',
            () =>
              navigator.clipboard.writeText(roomLink(room.id)).then(() => {
                if (view.dialog.open) this.options.notify('Room link copied');
              }),
            (error) => this.showError(view.error, error),
          ),
        );
        row.append(
          button('Invites…', () => {
            view.close();
            this.editingRoom = room;
            this.options.openSettings?.('room-invitations', null);
          }),
        );
        row.append(
          createFavoriteButton(room.id, favorites.has(room.id), {
            getToken: () => this.options.auth.jwt,
            getAccountId: () => this.options.auth.userId,
            current,
            onError: (error) => this.showError(view.error, error),
            onChange: () => {
              saved.refresh().catch((error) => this.showError(view.error, error));
            },
          }),
        );
        row.append(
          button(
            'Delete room…',
            () =>
              this.deleteRoom(room, () => {
                refresh().catch((error) => this.showError(view.error, error));
              }),
            'btn-secondary danger',
          ),
        );
        view.body.append(row);
      }
      // Rooms someone else owns where this account holds a role.
      view.body.append(el('h3', 'Rooms you belong to'));
      const membershipList = el('div');
      const seen = new Set<string>();
      let cursor: string | undefined;
      const loadMore = asyncButton(
        'Load more memberships',
        () => busy(loadMore, view.error, loadMemberships),
        (error) => this.showError(view.error, error),
      );
      const loadMemberships = async (): Promise<void> => {
        const page = await api.memberships(token, cursor);
        if (!view.dialog.open || !membershipList.isConnected) return;
        for (const membership of page.items) {
          if (seen.has(membership.id)) continue;
          seen.add(membership.id);
          const row = el('div', undefined, 'owned-room');
          row.append(
            el('h3', membership.display_name),
            el(
              'p',
              `${membership.id} · ${membership.role} · ${membership.participant_count ?? '?'} online`,
            ),
            button('Join', () => {
              view.close();
              this.options.onJoinRoom(membership.id);
            }),
          );
          membershipList.append(row);
        }
        if (!seen.size)
          membershipList.append(
            el(
              'p',
              'None yet. Accept an invitation from a room admin to add a room here.',
              'setting-hint',
            ),
          );
        loadMore.hidden = page.next_cursor === null || page.next_cursor === cursor;
        cursor = page.next_cursor ?? undefined;
      };
      view.body.append(membershipList, loadMore);
      await busy(loadMore, view.error, loadMemberships);
    };
    try {
      await refresh();
    } catch (error) {
      this.showError(view.error, error);
    }
  }

  /** Secrets are available only in this creation response, never in management lists. */
  private showCreatedInvite(code: string, view: Surface, kind: 'room' | 'registration'): void {
    const panel = el('div', undefined, 'created-invitation');
    const link = input(inviteLink(code, kind));
    link.readOnly = true;
    panel.append(
      el('h3', 'Save this invitation link'),
      el(
        'p',
        'This secret is shown only now. Share it privately; anyone holding it can accept it.',
        'setting-hint',
      ),
      el('code', code, 'invite-code'),
      field('Invitation link', link),
      asyncButton(
        'Copy link',
        () =>
          navigator.clipboard.writeText(link.value).then(() => {
            if (view.isOpen()) this.options.notify('Invitation link copied');
          }),
        (error) => this.showError(view.error, error),
      ),
    );
    view.body.querySelector('.created-invitation')?.remove();
    view.body.append(panel);
    link.focus();
    link.select();
  }

  /** One invitation with its link and revocation; `revoke` runs against the right endpoint. */
  private inviteRow(
    invite: RegistrationInvite | RoomInvite,
    view: Surface,
    refresh: () => Promise<void>,
    revoke: () => Promise<void>,
  ): HTMLElement {
    const row = el('div', undefined, 'management-entry');
    const summary = [
      'role' in invite ? invite.role : null,
      `${invite.uses_left} ${invite.uses_left === 1 ? 'use' : 'uses'} left`,
      `until ${new Date(invite.expires_at).toLocaleDateString()}`,
    ]
      .filter(Boolean)
      .join(' · ');
    row.append(el('span', `Invitation ${invite.id.slice(0, 8)}`), el('p', summary));
    const remove = asyncButton(
      'Revoke',
      () =>
        busy(remove, view.error, async () => {
          if (!view.isOpen()) return;
          await revoke();
          await refresh();
        }),
      (error) => this.showError(view.error, error),
      'btn-secondary danger',
    );
    row.append(remove);
    return row;
  }

  private deleteRoom(room: RoomListItem, done: () => void): void {
    const view = modal('Delete room');
    const token = this.options.auth.jwt;
    const generation = this.generation;
    const confirmation = input('', 'text', 128);
    view.body.append(
      el(
        'p',
        `Delete “${room.display_name}”? This removes its settings, membership records, sanctions and reports, and disconnects its participants. This cannot be undone.`,
      ),
      field(`Type ${room.id} to confirm`, confirmation),
    );
    const remove = asyncButton(
      'Permanently delete room',
      () =>
        busy(remove, view.error, async () => {
          if (!view.dialog.open || generation !== this.generation) return;
          if (confirmation.value !== room.id) throw new Error('Type the exact room ID to confirm');
          await api.deleteRoom(room.id, token);
          await this.options.onRoomDeleted(room.id);
          view.close();
          this.options.onRoomsChanged();
          done();
        }),
      (error) => this.showError(view.error, error),
      'btn-secondary danger',
    );
    view.body.append(remove);
  }

  /** The sheet's This room screens: identity over HTTP, moderation over the room socket. */
  roomSections(): SheetSection[] {
    const live = () => this.options.getRoom();
    const rank = () => ROLES.indexOf(live()?.role ?? 'guest');
    const owner = () =>
      live() ? live()?.role === 'owner' && this.options.auth.isLoggedIn : this.editingRoom !== null;
    const moderator = () => Boolean(live()?.localParticipantId) && rank() >= 3;
    const admin = () => Boolean(live()?.localParticipantId) && rank() >= 4;
    const name = () =>
      live() ? (live()?.roomSettings?.displayName ?? '') : (this.editingRoom?.display_name ?? '');
    return [
      {
        id: 'room-identity',
        segment: 'room',
        label: 'Name & topic',
        status: name,
        available: owner,
        screen: () => this.roomIdentityScreen(),
      },
      {
        id: 'room-people',
        segment: 'room',
        label: 'People',
        available: moderator,
        screen: () => this.managementScreen('members', 'People'),
      },
      {
        id: 'room-bans',
        segment: 'room',
        label: 'Bans',
        available: admin,
        screen: () => this.managementScreen('bans', 'Bans'),
      },
      {
        id: 'room-reports',
        segment: 'room',
        label: 'Reports',
        available: moderator,
        screen: () => this.managementScreen('reports', 'Reports'),
      },
      {
        id: 'room-history',
        segment: 'room',
        label: 'History',
        available: moderator,
        screen: () => this.managementScreen('history', 'History'),
      },
      {
        id: 'room-invitations',
        segment: 'room',
        label: 'Invitations',
        available: owner,
        screen: () => this.invitationsScreen(),
      },
    ];
  }

  /** Forgets the room chosen from My rooms once the sheet closes. */
  closeRoomContext(): void {
    this.editingRoom = null;
  }

  /** The owned room a room screen edits: the live room's record, or the one chosen from My rooms. */
  private async ownedRoom(): Promise<RoomListItem | null> {
    const live = this.options.getRoom();
    if (live?.currentRoomId) {
      const rooms = await api.ownRooms(this.options.auth.jwt);
      return rooms.find((entry) => entry.id === live.currentRoomId) ?? null;
    }
    return this.editingRoom;
  }

  private roomIdentityScreen(): SheetScreen {
    return {
      title: 'Name & topic',
      body: async (context) => {
        const body = el('div', undefined, 'sheet-screen');
        const error = el('p', '', 'auth-error');
        error.hidden = true;
        error.setAttribute('role', 'alert');
        const generation = this.generation;
        const current = (): boolean => context.current() && generation === this.generation;
        let item: RoomListItem | null = null;
        try {
          item = await this.ownedRoom();
        } catch (failure) {
          if (current()) {
            this.showError(error, failure);
            body.append(error);
          }
          return body;
        }
        if (!current()) return body;
        if (!item) {
          this.showError(error, new Error('This room is not in your owned rooms.'));
          body.append(error);
          return body;
        }
        const room = item;
        const token = this.options.auth.jwt;
        const name = input(room.display_name, 'text', 100);
        const topic = input(room.topic ?? '', 'text', 500);
        const description = el('textarea');
        description.value = room.description ?? '';
        description.maxLength = 1024;
        description.rows = 4;
        description.setAttribute('aria-label', 'Description');
        const nameAppearance = appearancePicker({
          label: 'Room name appearance',
          initial: room.name_style,
          name: () => name.value,
        });
        const topicAppearance = appearancePicker({
          label: 'Header description appearance',
          initial: room.topic_style,
          name: () => topic.value,
        });
        name.addEventListener('input', nameAppearance.refreshPreview);
        topic.addEventListener('input', topicAppearance.refreshPreview);
        let saving: Promise<void> = Promise.resolve();
        let image: { wrapper: HTMLElement; value: () => string | null } | null = null;
        const save = (row: HTMLElement): void => {
          saving = saving.then(async () => {
            if (!current()) return;
            try {
              await api.updateRoomIdentity(room.id, token, {
                display_name: name.value.trim(),
                topic: topic.value.trim() || null,
                description: description.value.trim(),
                image_url: image?.value() ?? null,
                name_style: nameAppearance.chosen(),
                topic_style: topicAppearance.chosen(),
              });
              if (!current()) return;
              error.hidden = true;
              markSaved(row);
              this.options.onRoomsChanged();
            } catch (failure) {
              if (current()) this.showError(error, failure);
            }
          });
        };
        image = this.imagePicker(room.image_url ?? null, error, true, () => save(imageBlock));
        const imageBlock = el('div', undefined, 'sheet-block');
        imageBlock.append(el('span', '', 'sheet-row-saved'), image.wrapper);
        const nameRow = fieldRow('Name', name);
        name.addEventListener('change', () => save(nameRow));
        const topicRow = fieldRow('Topic', topic);
        topic.addEventListener('change', () => save(topicRow));
        const descriptionBlock = el('div', undefined, 'sheet-block');
        descriptionBlock.append(
          el('span', '', 'sheet-row-saved'),
          el('h3', 'Description'),
          description,
        );
        description.addEventListener('change', () => save(descriptionBlock));
        for (const picker of [nameAppearance, topicAppearance]) {
          picker.element.append(el('span', '', 'sheet-row-saved'));
          picker.element.addEventListener('change', () => save(picker.element));
        }
        body.append(
          group(null, [nameRow, topicRow]),
          descriptionBlock,
          imageBlock,
          nameAppearance.element,
          topicAppearance.element,
          group(null, [
            actionRow('Delete room…', () => this.deleteRoom(room, () => context.close()), {
              danger: true,
            }),
          ]),
          error,
        );
        return body;
      },
    };
  }

  /** Members, bans, reports and history, one page at a time over the room socket. */
  private managementScreen(
    kind: 'members' | 'bans' | 'reports' | 'history',
    title: string,
  ): SheetScreen {
    return {
      title,
      body: (context) => {
        const body = el('div', undefined, 'sheet-screen');
        const room = this.options.getRoom();
        const error = el('p', '', 'auth-error');
        error.hidden = true;
        error.setAttribute('role', 'alert');
        if (!room) return body;
        const generation = this.generation;
        const current = (): boolean =>
          context.current() && generation === this.generation && this.options.getRoom() === room;
        const content = el('div', undefined, 'management-list');
        let offset = 0;
        let revision = 0;
        const render = async (): Promise<void> => {
          const requested = ++revision;
          error.hidden = true;
          content.replaceChildren(el('p', 'Loading…', 'sheet-loading'));
          try {
            const page =
              kind === 'members'
                ? {
                    kind: 'members' as const,
                    data: await room.requestSocial('listRoomMembers', { offset }),
                  }
                : kind === 'bans'
                  ? {
                      kind: 'bans' as const,
                      data: await room.requestSocial('listRoomBans', { offset }),
                    }
                  : kind === 'reports'
                    ? {
                        kind: 'reports' as const,
                        data: await room.requestSocial('listRoomReports', { offset }),
                      }
                    : {
                        kind: 'history' as const,
                        data: await room.requestSocial('listModerationEvents', { offset }),
                      };
            if (revision !== requested || !current()) return;
            content.replaceChildren();
            const list =
              page.kind === 'members'
                ? page.data.members
                : page.kind === 'bans'
                  ? page.data.bans
                  : page.kind === 'reports'
                    ? page.data.reports
                    : page.data.events;
            if (!list.length) content.append(note(`No ${kind} on this page.`));
            if (page.kind === 'members') {
              for (const person of page.data.members) {
                const row = el('div', undefined, 'management-entry');
                row.append(
                  el(
                    'span',
                    `${person.displayName} · ${person.role} · ${person.online ? 'online' : 'offline'}`,
                  ),
                );
                const rank = ROLES.indexOf(room.role);
                const targetRank = ROLES.indexOf(person.role);
                if (
                  person.authenticated &&
                  rank > targetRank &&
                  person.userId !== room.localParticipantId
                ) {
                  const role = el('select');
                  role.setAttribute('aria-label', `Role for ${person.displayName}`);
                  for (let index = 1; index < rank; index++) {
                    const option = el('option', ROLES[index]);
                    option.value = String(index);
                    role.append(option);
                  }
                  role.value = String(targetRank);
                  const save = asyncButton(
                    'Update role',
                    () =>
                      busy(save, error, async () => {
                        await room.requestSocial('setMemberRole', {
                          targetUserId: person.userId,
                          role: Number(role.value),
                        });
                        await render();
                      }),
                    (failure) => this.showError(error, failure),
                    'sheet-button',
                  );
                  row.append(role, save);
                }
                content.append(row);
              }
            } else if (page.kind === 'bans') {
              for (const ban of page.data.bans) {
                const row = el('div', undefined, 'management-entry');
                row.append(
                  el('strong', ban.displayName),
                  el('p', ban.reason || 'No reason recorded'),
                  el(
                    'p',
                    ban.expiresAt
                      ? `Expires ${new Date(ban.expiresAt).toLocaleString()}`
                      : 'Until removed',
                  ),
                );
                const remove = asyncButton(
                  'Unban',
                  () =>
                    busy(remove, error, async () => {
                      await room.requestSocial('removeRoomBan', { banId: ban.banId });
                      await render();
                    }),
                  (failure) => this.showError(error, failure),
                  'sheet-button',
                );
                row.append(remove);
                content.append(row);
              }
            } else if (page.kind === 'reports') {
              for (const report of page.data.reports) {
                const row = el('div', undefined, 'management-entry');
                row.append(
                  el('strong', `${report.targetName} · ${report.status}`),
                  el(
                    'p',
                    `${report.reporterName} · ${new Date(report.createdAt).toLocaleString()}`,
                  ),
                  el('p', report.reason, 'profile-bio'),
                );
                if (report.status === 'open') {
                  for (const status of ['resolved', 'dismissed'] as const) {
                    const update = asyncButton(
                      status === 'resolved' ? 'Mark resolved' : 'Dismiss report',
                      () =>
                        busy(update, error, async () => {
                          await room.requestSocial('resolveRoomReport', {
                            reportId: report.reportId,
                            status,
                          });
                          await render();
                        }),
                      (failure) => this.showError(error, failure),
                      'sheet-button',
                    );
                    row.append(update);
                  }
                  // Acting on the report resolves it and links the outcome to it.
                  const kick = asyncButton(
                    'Kick',
                    () =>
                      busy(kick, error, async () => {
                        await room.kick(report.targetParticipantId, undefined, report.reportId);
                        await render();
                      }),
                    (failure) => this.showError(error, failure),
                    'sheet-button danger',
                  );
                  const ban = button(
                    'Ban…',
                    () =>
                      this.ban(report.targetParticipantId, report.targetName, {
                        reportId: report.reportId,
                        onDone: refresh,
                      }),
                    'sheet-button danger',
                  );
                  row.append(kick, ban);
                } else if (report.outcome) {
                  row.append(
                    el(
                      'p',
                      `Led to: ${actionLabel(report.outcome.action)} · ${new Date(report.outcome.createdAt).toLocaleString()}`,
                    ),
                  );
                }
                content.append(row);
              }
            } else {
              for (const event of page.data.events) {
                const row = el('div', undefined, 'management-entry');
                row.append(
                  el('strong', `${event.targetName} · ${actionLabel(event.action)}`),
                  el(
                    'p',
                    `${event.actorName} · ${new Date(event.createdAt).toLocaleString()}${event.reportId ? ' · from a report' : ''}`,
                  ),
                );
                if (event.reason) row.append(el('p', event.reason, 'profile-bio'));
                if (event.expiresAt)
                  row.append(el('p', `Until ${new Date(event.expiresAt).toLocaleString()}`));
                // Only the owner receives an address; it is theirs to read, never to show.
                if (event.targetIp) row.append(el('p', `Address ${event.targetIp}`, 'sheet-note'));
                content.append(row);
              }
            }
            const pagination = el('div', undefined, 'sheet-actions');
            if (offset)
              pagination.append(
                button(
                  'Previous page',
                  () => {
                    offset = Math.max(0, offset - 100);
                    refresh();
                  },
                  'sheet-button',
                ),
              );
            if (page.data.hasMore)
              pagination.append(
                button(
                  'Next page',
                  () => {
                    offset += 100;
                    refresh();
                  },
                  'sheet-button',
                ),
              );
            if (pagination.children.length) content.append(pagination);
          } catch (failure) {
            if (revision === requested && current()) {
              content.replaceChildren();
              this.showError(error, failure);
            }
          }
        };
        const refresh = (): void => {
          render().catch((failure) => {
            if (current()) this.showError(error, failure);
          });
        };
        body.append(content, error);
        refresh();
        return body;
      },
    };
  }

  /** Codes that grant a role in one of the account's rooms, made and revoked here. */
  private invitationsScreen(): SheetScreen {
    return {
      title: 'Invitations',
      body: async (context) => {
        const body = el('div', undefined, 'sheet-screen');
        const error = el('p', '', 'auth-error');
        error.hidden = true;
        error.setAttribute('role', 'alert');
        const stillCurrent = this.accountScope(() => context.current());
        let room: RoomListItem | null = null;
        try {
          room = await this.ownedRoom();
        } catch (failure) {
          if (stillCurrent()) {
            this.showError(error, failure);
            body.append(error);
          }
          return body;
        }
        if (!stillCurrent()) return body;
        if (!room) {
          this.showError(error, new Error('This room is not in your owned rooms.'));
          body.append(error);
          return body;
        }
        const owned = room;
        const surface: Surface = { body, error, isOpen: stillCurrent };
        const role = el('select');
        role.setAttribute('aria-label', 'Role granted');
        for (const option of INVITE_ROLES) {
          const node = el('option', option.label);
          node.value = String(option.value);
          role.append(node);
        }
        const uses = input('1', 'number', 3);
        uses.min = '1';
        uses.max = '100';
        const days = input('7', 'number', 2);
        days.min = '1';
        days.max = '30';
        const list = el('div', undefined, 'sheet-group');
        const refresh = async (): Promise<void> => {
          const invites = await api.roomInvites(this.options.auth.jwt, owned.id);
          if (!stillCurrent()) return;
          list.replaceChildren();
          if (!invites.length) list.append(note('No unused invitations.'));
          for (const invite of invites)
            list.append(
              this.inviteRow(
                invite,
                surface,
                () => refresh(),
                () => api.revokeRoomInvite(this.options.auth.jwt, owned.id, invite.id),
              ),
            );
        };
        const create = asyncButton(
          'Create invitation',
          () =>
            busy(create, error, async () => {
              if (!stillCurrent()) return;
              const invite = await api.createRoomInvite(this.options.auth.jwt, owned.id, {
                role: Number(role.value),
                uses: Number(uses.value) || 1,
                days: Number(days.value) || 7,
              });
              if (!stillCurrent()) return;
              this.showCreatedInvite(invite.code, surface, 'room');
              await refresh();
            }),
          (failure) => this.showError(error, failure),
          'btn-primary sheet-button',
        );
        body.append(
          group(null, [
            fieldRow('Role granted', role),
            fieldRow('Uses', uses),
            fieldRow('Valid for (days)', days),
          ]),
          note(
            'Recipients review the room and the offered role, then accept; accepting never joins or lowers an existing role.',
          ),
          create,
          group('Unused invitations', [list]),
          error,
        );
        refresh().catch((failure) => {
          if (stillCurrent()) this.showError(error, failure);
        });
        return body;
      },
    };
  }

  private imagePicker(
    initial: string | null,
    error: HTMLElement,
    room = false,
    onChange?: () => void,
  ): { wrapper: HTMLElement; value: () => string | null } {
    let value = initial;
    let selection = 0;
    const wrapper = el('div', undefined, 'image-picker');
    const preview = el('img');
    preview.className = room ? 'room-image-preview' : 'profile-image';
    preview.alt = room ? 'Room image preview' : 'Avatar preview';
    const show = (): void => {
      preview.hidden = !safeRasterUrl(value);
      if (safeRasterUrl(value)) preview.src = value;
      else preview.removeAttribute('src');
    };
    show();
    const file = input('', 'file');
    file.accept = 'image/png,image/jpeg,image/webp';
    file.addEventListener('change', () => {
      const chosen = file.files?.[0];
      if (!chosen) return;
      const generation = ++selection;
      file.disabled = true;
      rasterUpload(chosen, room ? 480 : 192, room ? 270 : 192)
        .then((result) => {
          if (selection === generation) {
            value = result;
            show();
            file.disabled = false;
            onChange?.();
          }
        })
        .catch((failure) => this.showError(error, failure))
        .finally(() => {
          file.disabled = false;
        });
    });
    wrapper.append(
      preview,
      field(room ? 'Room image (PNG, JPEG or WebP)' : 'Avatar (PNG, JPEG or WebP)', file),
      button('Remove image', () => {
        selection++;
        value = null;
        file.value = '';
        show();
        onChange?.();
      }),
    );
    return {
      wrapper,
      value: () => {
        if (file.disabled) throw new Error('Wait for the image to finish processing');
        return value;
      },
    };
  }

  private showError(node: HTMLElement, error: unknown): void {
    if (!node.isConnected) return;
    node.textContent = error instanceof Error ? error.message : 'Unable to complete action';
    node.hidden = false;
  }
}

export function roomLink(id: string): string {
  const url = new URL(window.location.href);
  url.hash = id;
  url.search = '';
  return url.href;
}

/** Keep invitation secrets in the fragment, which browsers never send in an HTTP request. */
export function inviteLink(code: string, kind: 'room' | 'registration' = 'room'): string {
  const url = new URL(window.location.href);
  url.search = '';
  url.hash = `${kind === 'registration' ? 'register-invite' : 'invite'}=${encodeURIComponent(code)}`;
  return url.href;
}
