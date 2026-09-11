import type { AuthManager } from './auth';
import type { RoomClient } from './room';
import type { AccountProfile, PublicProfile, RoomListItem } from './protocol';
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
}

const ROLES = ['guest', 'user', 'member', 'moderator', 'admin', 'owner'];

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
  private generation = 0;
  private identity = '';

  constructor(private readonly options: Options) {
    document
      .getElementById('community-actions')!
      .append(this.accountButton, this.roomsButton, this.nicknameButton, this.manageButton);
    const recovery = button('Recover with a saved key', () => this.openRecovery(), 'auth-link-btn');
    document.querySelector('#login-modal .auth-alt-actions')!.append(recovery);
    this.refresh();
  }

  refresh(): void {
    const room = this.options.getRoom();
    const identity = `${this.options.auth.userId ?? ''}:${room?.currentRoomId ?? ''}:${room?.localParticipantId ?? ''}`;
    if (identity !== this.identity) {
      this.identity = identity;
      this.generation++;
      this.profiles.clear();
      document
        .querySelectorAll<HTMLDialogElement>('.community-dialog')
        .forEach((dialog) => dialog.close());
    }
    this.accountButton.hidden = !this.options.auth.isLoggedIn;
    this.roomsButton.hidden = !this.options.auth.isLoggedIn;
    this.nicknameButton.hidden = !room?.localParticipantId;
    this.manageButton.hidden = !room?.localParticipantId || ROLES.indexOf(room.role) < 3;
  }

  decorateAvatar(node: HTMLElement, id: string, authenticated: boolean): void {
    if (!authenticated) return;
    this.profile(id)
      .then((profile) => {
        if (!node.isConnected || !profile?.avatar_url || !safeRasterUrl(profile.avatar_url)) return;
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
    const profile = await this.profile(id);
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

  ban(id: string, name: string): void {
    const room = this.options.getRoom();
    if (!room) return;
    const view = modal(`Ban ${name}`);
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
    view.body.append(
      button(
        'Ban from room',
        () => {
          room.ban(
            id,
            reason.value.trim() || undefined,
            duration.value ? Number(duration.value) : undefined,
          );
          view.close();
        },
        'btn-secondary danger',
      ),
    );
  }

  private async profile(id: string): Promise<PublicProfile | null> {
    let promise = this.profiles.get(id);
    if (!promise) {
      promise = api.publicProfile(id, this.options.auth.jwt).catch(() => null);
      if (this.profiles.size >= 100) this.profiles.delete(this.profiles.keys().next().value!);
      this.profiles.set(id, promise);
    }
    return promise;
  }

  private async openAccount(): Promise<void> {
    const view = modal('Account');
    const token = this.options.auth.jwt;
    const accountId = this.options.auth.userId;
    const generation = this.generation;
    const stillCurrent = (): boolean =>
      generation === this.generation && this.options.auth.userId === accountId && view.dialog.open;
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
        () =>
          busy(save, view.error, async () => {
            if (!stillCurrent()) return;
            const updated = await api.updateProfile(token, {
              display_name: name.value.trim(),
              bio: bio.value.trim(),
              avatar_url: avatar.value(),
            });
            if (!stillCurrent()) return;
            this.profiles.delete(updated.id);
            this.options.onProfileChanged(updated);
            this.options.notify('Profile saved');
          }),
        (error) => this.showError(view.error, error),
        'btn-primary',
      );
      view.body.append(save, el('h3', 'Password and recovery'));
      view.body.append(
        el(
          'p',
          'Password changes sign out all sessions. These controls require your current password; passkey-only accounts can continue signing in with their passkey.',
          'setting-hint',
        ),
      );
      const current = input('', 'password', 128);
      current.autocomplete = 'current-password';
      const password = input('', 'password', 128);
      password.autocomplete = 'new-password';
      const confirm = input('', 'password', 128);
      confirm.autocomplete = 'new-password';
      view.body.append(
        field('Current password', current),
        field('New password', password),
        field('Confirm new password', confirm),
      );
      const change = asyncButton(
        'Change password',
        () =>
          busy(change, view.error, async () => {
            if (!stillCurrent()) return;
            validatePassword(password.value, confirm.value);
            await api.changePassword(token, {
              current_password: current.value,
              new_password: password.value,
            });
            view.close();
            if (this.options.auth.userId === accountId) await this.options.onSignedOut();
            this.options.notify('Password changed. Sign in again with your new password.');
          }),
        (error) => this.showError(view.error, error),
      );
      const generate = asyncButton(
        profile.recovery_enabled ? 'Replace recovery key' : 'Generate recovery key',
        () =>
          busy(generate, view.error, async () => {
            if (!stillCurrent()) return;
            if (!current.value)
              throw new Error('Enter your current password to generate a recovery key');
            if (
              profile.recovery_enabled &&
              !window.confirm(
                'Replace your saved recovery key? The previous key will stop working.',
              )
            )
              return;
            const result = await api.recoveryKey(token, { current_password: current.value });
            if (!stillCurrent()) {
              result.recovery_key = '';
              return;
            }
            profile.recovery_enabled = true;
            generate.textContent = 'Replace recovery key';
            const keyView = modal('Save your recovery key');
            const key = el('textarea');
            key.readOnly = true;
            key.value = result.recovery_key;
            key.rows = 3;
            keyView.body.append(
              el(
                'p',
                'Store this key in a password manager. It is shown only now and can reset your password once. Replacing it invalidates the old key. No email is sent.',
              ),
              field('Recovery key', key),
            );
            const copy = asyncButton(
              'Copy recovery key',
              () =>
                busy(copy, keyView.error, async () => {
                  await navigator.clipboard.writeText(key.value);
                  copy.textContent = 'Copied';
                }),
              (error) => this.showError(keyView.error, error),
            );
            keyView.body.append(copy);
            keyView.dialog.addEventListener('close', () => {
              key.value = '';
              result.recovery_key = '';
            });
            current.value = '';
          }),
        (error) => this.showError(view.error, error),
      );
      view.body.append(change, generate);
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
      for (const room of rooms) {
        const row = el('div', undefined, 'owned-room');
        row.append(
          el('h3', room.display_name),
          el(
            'p',
            `${room.id} · ${room.secret ? 'Unlisted' : 'Public'} · ${room.participant_count} online`,
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
    };
    try {
      await refresh();
    } catch (error) {
      this.showError(view.error, error);
    }
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
    let current: 'members' | 'bans' | 'reports' = 'members';
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
              : {
                  kind: 'reports' as const,
                  data: await room.requestSocial('listRoomReports', { offset }),
                };
        if (revision !== requested || !view.dialog.open) return;
        content.replaceChildren();
        const list =
          page.kind === 'members'
            ? page.data.members
            : page.kind === 'bans'
              ? page.data.bans
              : page.data.reports;
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
        } else {
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
            }
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
    for (const section of ['members', 'bans', 'reports'] as const) {
      if (section === 'bans' && ROLES.indexOf(room.role) < 4) continue;
      tabs.append(
        button(
          section === 'members' ? 'Members & roles' : section === 'bans' ? 'Bans' : 'Reports',
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
