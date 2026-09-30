import type { AuthManager } from './auth';
import { mountAccountSecurity, type AccountSecurityFlow } from './account-security';
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
} from './ui';

interface Options {
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

export class CommunityUI {
  private readonly accountButton = button('Account', () => {
    const generation = this.generation;
    this.openAccount().catch((error) => {
      if (generation === this.generation)
        this.options.notify(error instanceof Error ? error.message : 'Account unavailable');
    });
  });
  private readonly roomsButton = button('My rooms', () => {
    const generation = this.generation;
    this.openRooms().catch((error) => {
      if (generation === this.generation)
        this.options.notify(error instanceof Error ? error.message : 'Rooms unavailable');
    });
  });
  private readonly nicknameButton = button('Nickname', () => this.openNickname());
  private readonly manageButton = button('Manage room', () => this.openManagement());
  private profiles = new Map<string, Promise<PublicProfile | null>>();
  private avatarVersions = new WeakMap<HTMLElement, number>();
  private rosterProfiles: Set<string> | null = null;
  private profileActive = 0;
  private profileWaiting: (() => void)[] = [];
  private generation = 0;
  private accountGeneration = 0;
  private accountIdentity = '';
  private identity = '';

  constructor(private readonly options: Options) {
    const accountActions = document.getElementById('community-actions')!;
    accountActions.append(this.accountButton, this.roomsButton);
    // Room-scoped actions belong with the room tools; the header keeps account actions.
    (document.getElementById('room-actions') ?? accountActions).append(
      this.nicknameButton,
      this.manageButton,
    );
    const recovery = button('Recover with a saved key', () => this.openRecovery(), 'auth-link-btn');
    document.querySelector('#login-modal .auth-alt-actions')!.append(recovery);
    this.refresh();
  }

  refresh(): void {
    const room = this.options.getRoom();
    const accountIdentity = this.options.auth.userId ?? '';
    const accountChanged = accountIdentity !== this.accountIdentity;
    if (accountChanged) {
      this.accountIdentity = accountIdentity;
      this.accountGeneration++;
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
    this.roomsButton.hidden = !this.options.auth.isLoggedIn;
    this.nicknameButton.hidden = !room?.localParticipantId;
    this.manageButton.hidden = !room?.localParticipantId || ROLES.indexOf(room.role) < 3;
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

  private async openAccount(): Promise<void> {
    let security: AccountSecurityFlow | null = null;
    const view = modal('Account', () => security?.canDismiss ?? true);
    view.dialog.setAttribute('data-account-dialog', 'true');
    const token = this.options.auth.jwt;
    const accountId = this.options.auth.userId;
    const generation = this.accountGeneration;
    const stillCurrent = (): boolean =>
      generation === this.accountGeneration &&
      this.options.auth.userId === accountId &&
      view.dialog.open;
    try {
      const profile = await api.accountProfile(token);
      if (!stillCurrent()) return;
      const name = input(profile.display_name, 'text', 64);
      const bio = el('textarea');
      bio.value = profile.bio;
      bio.maxLength = 1000;
      bio.rows = 3;
      const avatar = this.imagePicker(profile.avatar_url, view.error);
      view.body.append(
        el('p', profile.email),
        field('Account display name', name),
        field('Bio', bio),
        avatar.wrapper,
      );
      const save = asyncButton(
        'Save profile',
        async () => {
          if (!stillCurrent() || !security?.canStart) return;
          const update = {
            display_name: name.value.trim(),
            bio: bio.value.trim(),
            avatar_url: avatar.value(),
          };
          await security.change(
            (currentToken, signal) => api.updateProfile(currentToken, update, signal),
            (updated) => {
              this.profiles.delete(updated.id);
              this.options.onProfileChanged(updated);
              this.options.notify('Profile saved');
            },
          );
        },
        (error) => this.showError(view.error, error),
        'btn-primary',
      );
      view.body.append(save);
      let changePassword: HTMLButtonElement | null = null;
      const passwordFields: HTMLInputElement[] = [];
      security = mountAccountSecurity({
        container: view.body,
        dialog: view.dialog,
        token: () => this.options.auth.jwt,
        current: stillCurrent,
        interactionChanged: (canStart) => {
          save.disabled = !canStart;
          if (changePassword) changePassword.disabled = !canStart;
          if (!canStart) for (const secret of passwordFields) secret.value = '';
        },
        removed: () => {
          view.dialog.close();
          this.options.notify('Passkey removed. Sign in again with a remaining sign-in method.');
          this.options.onSignedOut().catch(() => {
            this.options.notify('Passkey removed. Reload to finish signing out.');
          });
        },
      });
      await security.load();
      if (!stillCurrent() || !security.settings?.password_enabled) return;
      view.body.append(el('h3', 'Change password'));
      view.body.append(el('p', 'Changing your password signs out all sessions.', 'setting-hint'));
      const current = input('', 'password', 128);
      current.autocomplete = 'current-password';
      const password = input('', 'password', 128);
      password.autocomplete = 'new-password';
      const confirm = input('', 'password', 128);
      confirm.autocomplete = 'new-password';
      passwordFields.push(current, password, confirm);
      view.body.append(
        field('Current password', current),
        field('New password', password),
        field('Confirm new password', confirm),
      );
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
              view.dialog.close();
              this.options.notify('Password changed. Sign in again with your new password.');
              this.options
                .onSignedOut()
                .catch(() =>
                  this.options.notify('Password changed. Reload to finish signing out.'),
                );
            },
          );
        },
        (error) => this.showError(view.error, error),
      );
      changePassword = change;
      view.dialog.addEventListener(
        'close',
        () => {
          current.value = '';
          password.value = '';
          confirm.value = '';
        },
        { once: true },
      );
      view.body.append(change);
      // Registration invitations: the way in while registration is closed.
      const invites = el('div');
      const refreshInvites = async (): Promise<void> => {
        const codes = await api.registrationInvites(token);
        if (!stillCurrent()) return;
        invites.replaceChildren();
        if (!codes.length) invites.append(el('p', 'No unused invite codes.', 'setting-hint'));
        for (const code of codes)
          invites.append(
            this.inviteRow(code, view, refreshInvites, () =>
              api.revokeRegistrationInvite(token, code.code),
            ),
          );
      };
      const mint = asyncButton(
        'New invite code',
        () =>
          busy(mint, view.error, async () => {
            await api.createRegistrationInvite(token);
            await refreshInvites();
          }),
        (error) => this.showError(view.error, error),
      );
      view.body.append(
        el('h3', 'Invite someone to register'),
        el(
          'p',
          'While registration is closed, a code lets one person create an account. You can hold five unused codes; each lasts a week.',
          'setting-hint',
        ),
        mint,
        invites,
      );
      refreshInvites().catch((error) => {
        if (stillCurrent()) this.showError(view.error, error);
      });
      view.body.append(
        el('h3', 'This device'),
        el(
          'p',
          'Signing out this way also removes the name, devices, layout and chat preferences this browser remembers.',
          'setting-hint',
        ),
        button('Sign out and forget this device', () => {
          view.close();
          this.options.onForgetDevice();
        }),
      );
    } catch (error) {
      if (stillCurrent()) this.showError(view.error, error);
    }
  }

  private openRecovery(): void {
    const view = modal('Recover account');
    const email = input('', 'email', 254);
    email.autocomplete = 'username';
    const key = input('', 'password', 128);
    key.autocomplete = 'off';
    const password = input('', 'password', 128);
    password.autocomplete = 'new-password';
    const confirm = input('', 'password', 128);
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
    const refresh = async (): Promise<void> => {
      const rooms = await api.ownRooms(token);
      if (!view.dialog.open) return;
      view.body.replaceChildren();
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
          button('Edit room', () =>
            this.editRoom(room, () => {
              refresh().catch((error) => this.showError(view.error, error));
            }),
          ),
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
        row.append(button('Invites…', () => this.roomInvites(room)));
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
      const memberships = await api.memberships(token);
      if (!view.dialog.open) return;
      view.body.append(el('h3', 'Rooms you belong to'));
      if (!memberships.length)
        view.body.append(
          el('p', 'None yet. An invitation link from a room admin adds you here.', 'setting-hint'),
        );
      for (const membership of memberships) {
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
        view.body.append(row);
      }
    };
    try {
      await refresh();
    } catch (error) {
      this.showError(view.error, error);
    }
  }

  /** Codes that grant a role in one of the account's rooms, made and revoked here. */
  private roomInvites(room: RoomListItem): void {
    const view = modal(`Invitations for ${room.display_name}`);
    const token = this.options.auth.jwt;
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
    const list = el('div');
    const refresh = async (): Promise<void> => {
      const invites = await api.roomInvites(token, room.id);
      if (!view.dialog.open) return;
      list.replaceChildren();
      if (!invites.length) list.append(el('p', 'No unused invitations.', 'setting-hint'));
      for (const invite of invites)
        list.append(
          this.inviteRow(
            invite,
            view,
            () => refresh(),
            () => api.revokeRoomInvite(token, room.id, invite.code),
          ),
        );
    };
    const create = asyncButton(
      'Create invitation',
      () =>
        busy(create, view.error, async () => {
          await api.createRoomInvite(token, room.id, {
            role: Number(role.value),
            uses: Number(uses.value) || 1,
            days: Number(days.value) || 7,
          });
          await refresh();
        }),
      (error) => this.showError(view.error, error),
      'btn-primary',
    );
    view.body.append(
      el(
        'p',
        'Whoever opens an invitation link while signed in gets the role in this room; nobody is ever demoted by one. A room keeps at most 20 unused invitations.',
        'setting-hint',
      ),
      field('Role granted', role),
      field('Uses', uses),
      field('Valid for (days)', days),
      create,
      el('h3', 'Unused invitations'),
      list,
    );
    refresh().catch((error) => this.showError(view.error, error));
  }

  /** One invitation with its link and revocation; `revoke` runs against the right endpoint. */
  private inviteRow(
    invite: RegistrationInvite | RoomInvite,
    view: ReturnType<typeof modal>,
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
    row.append(el('code', invite.code, 'invite-code'), el('p', summary));
    const copy = asyncButton(
      'Copy link',
      () =>
        navigator.clipboard.writeText(inviteLink(invite.code)).then(() => {
          if (view.dialog.open) this.options.notify('Invitation link copied');
        }),
      (error) => this.showError(view.error, error),
    );
    const remove = asyncButton(
      'Revoke',
      () =>
        busy(remove, view.error, async () => {
          await revoke();
          await refresh();
        }),
      (error) => this.showError(view.error, error),
      'btn-secondary danger',
    );
    row.append(copy, remove);
    return row;
  }

  private editRoom(room: RoomListItem, done: () => void): void {
    const view = modal(`Edit ${room.display_name}`);
    const token = this.options.auth.jwt;
    const generation = this.generation;
    const name = input(room.display_name, 'text', 100);
    const topic = input(room.topic ?? '', 'text', 500);
    const description = el('textarea');
    description.value = room.description ?? '';
    description.maxLength = 1024;
    description.rows = 4;
    const image = this.imagePicker(room.image_url ?? null, view.error, true);
    view.body.append(
      field('Room display name', name),
      field('Topic', topic),
      field('Description / room rules', description),
      image.wrapper,
      el(
        'p',
        'Room images are uploaded intentionally. Camera thumbnails are not captured or published.',
        'setting-hint',
      ),
    );
    const save = asyncButton(
      'Save room',
      () =>
        busy(save, view.error, async () => {
          if (!view.dialog.open || generation !== this.generation) return;
          await api.updateRoomIdentity(room.id, token, {
            display_name: name.value.trim(),
            topic: topic.value.trim() || null,
            description: description.value.trim(),
            image_url: image.value(),
          });
          view.close();
          this.options.onRoomsChanged();
          done();
        }),
      (error) => this.showError(view.error, error),
      'btn-primary',
    );
    view.body.append(save);
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

  private openManagement(): void {
    const room = this.options.getRoom();
    if (!room) return;
    const view = modal('Manage room');
    const tabs = el('div', undefined, 'community-row');
    const content = el('div');
    let current: 'members' | 'bans' | 'reports' | 'history' = 'members';
    let offset = 0;
    let revision = 0;
    const render = async (): Promise<void> => {
      const requested = ++revision;
      view.error.hidden = true;
      content.replaceChildren(el('p', 'Loading…'));
      try {
        const page =
          current === 'members'
            ? {
                kind: 'members' as const,
                data: await room.requestSocial('listRoomMembers', { offset }),
              }
            : current === 'bans'
              ? {
                  kind: 'bans' as const,
                  data: await room.requestSocial('listRoomBans', { offset }),
                }
              : current === 'reports'
                ? {
                    kind: 'reports' as const,
                    data: await room.requestSocial('listRoomReports', { offset }),
                  }
                : {
                    kind: 'history' as const,
                    data: await room.requestSocial('listModerationEvents', { offset }),
                  };
        if (revision !== requested || !view.dialog.open) return;
        content.replaceChildren();
        const list =
          page.kind === 'members'
            ? page.data.members
            : page.kind === 'bans'
              ? page.data.bans
              : page.kind === 'reports'
                ? page.data.reports
                : page.data.events;
        if (!list.length) content.append(el('p', `No ${current} on this page.`));
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
                  busy(save, view.error, async () => {
                    await room.requestSocial('setMemberRole', {
                      targetUserId: person.userId,
                      role: Number(role.value),
                    });
                    await render();
                  }),
                (error) => this.showError(view.error, error),
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
                busy(remove, view.error, async () => {
                  await room.requestSocial('removeRoomBan', { banId: ban.banId });
                  await render();
                }),
              (error) => this.showError(view.error, error),
            );
            row.append(remove);
            content.append(row);
          }
        } else if (page.kind === 'reports') {
          for (const report of page.data.reports) {
            const row = el('div', undefined, 'management-entry');
            row.append(
              el('strong', `${report.targetName} · ${report.status}`),
              el('p', `${report.reporterName} · ${new Date(report.createdAt).toLocaleString()}`),
              el('p', report.reason, 'profile-bio'),
            );
            if (report.status === 'open') {
              for (const status of ['resolved', 'dismissed'] as const) {
                const update = asyncButton(
                  status === 'resolved' ? 'Mark resolved' : 'Dismiss report',
                  () =>
                    busy(update, view.error, async () => {
                      await room.requestSocial('resolveRoomReport', {
                        reportId: report.reportId,
                        status,
                      });
                      await render();
                    }),
                  (error) => this.showError(view.error, error),
                );
                row.append(update);
              }
              // Acting on the report resolves it and links the outcome to it.
              const kick = asyncButton(
                'Kick',
                () =>
                  busy(kick, view.error, async () => {
                    await room.kick(report.targetParticipantId, undefined, report.reportId);
                    await render();
                  }),
                (error) => this.showError(view.error, error),
                'btn-secondary danger',
              );
              const ban = button(
                'Ban…',
                () =>
                  this.ban(report.targetParticipantId, report.targetName, {
                    reportId: report.reportId,
                    onDone: refresh,
                  }),
                'btn-secondary danger',
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
            if (event.targetIp) row.append(el('p', `Address ${event.targetIp}`, 'setting-hint'));
            content.append(row);
          }
        }
        const pagination = el('div', undefined, 'community-row');
        if (offset)
          pagination.append(
            button('Previous page', () => {
              offset = Math.max(0, offset - 100);
              refresh();
            }),
          );
        if (page.data.hasMore)
          pagination.append(
            button('Next page', () => {
              offset += 100;
              refresh();
            }),
          );
        content.append(pagination);
      } catch (error) {
        if (revision === requested) {
          content.replaceChildren();
          this.showError(view.error, error);
        }
      }
    };
    const refresh = () => {
      const generation = this.generation;
      render().catch((error) => {
        if (this.generation === generation && view.dialog.isConnected)
          this.showError(view.error, error);
      });
    };
    for (const section of ['members', 'bans', 'reports', 'history'] as const) {
      if (section === 'bans' && ROLES.indexOf(room.role) < 4) continue;
      tabs.append(
        button(
          section === 'members'
            ? 'Members & roles'
            : section === 'bans'
              ? 'Bans'
              : section === 'reports'
                ? 'Reports'
                : 'History',
          () => {
            current = section;
            offset = 0;
            refresh();
          },
        ),
      );
    }
    view.body.append(
      tabs,
      el(
        'p',
        'Viewing/entry permission and broadcast permission are separate. Room Settings → Access controls who may enter; Guests Can Broadcast and moderated-room member roles control participation.',
        'setting-hint',
      ),
      content,
    );
    refresh();
  }

  private imagePicker(
    initial: string | null,
    error: HTMLElement,
    room = false,
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

function validatePassword(password: string, confirmation: string): void {
  const length = new TextEncoder().encode(password).length;
  if (length < 8 || length > 128) throw new Error('Use a password between 8 and 128 bytes');
  // The credential policy intentionally rejects C0/C1 control characters.
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f-\u009f]/.test(password))
    throw new Error('Passwords cannot contain control characters');
  if (password !== confirmation) throw new Error('New passwords do not match');
}

export function roomLink(id: string): string {
  const url = new URL(window.location.href);
  url.hash = id;
  url.search = '';
  return url.href;
}

/** The page with `?invite=CODE`: accepted once the viewer is signed in. */
export function inviteLink(code: string): string {
  const url = new URL(window.location.href);
  url.hash = '';
  url.search = `?invite=${encodeURIComponent(code)}`;
  return url.href;
}
