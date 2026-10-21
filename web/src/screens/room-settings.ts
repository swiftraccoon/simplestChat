import type { RoomClient } from '../room';
import type { RoomSettingsPatch } from '../protocol';
import {
  fieldRow,
  group,
  navRow,
  note,
  toggleRow,
  type SheetScreen,
  type SheetSection,
} from '../settings-sheet';
import { button, el } from '../ui';

type BooleanRoomSetting = {
  [K in keyof RoomSettingsPatch]-?: NonNullable<RoomSettingsPatch[K]> extends boolean ? K : never;
}[keyof RoomSettingsPatch];

interface RoomSettingsOptions {
  store: { getRoom: () => RoomClient | null };
  notify: (message: string) => void;
  asyncAction: (action: () => Promise<void>, failureMessage: string) => () => void;
}

/** Live-room access and participation settings share one confirmation owner.
 * Leaving a view retires its drafts and retry action; a membership transition must
 * call retire() so an older request cannot update the replacement room's controls. */
export class RoomSettingsScreen {
  private readonly options: RoomSettingsOptions;

  private controls: Record<string, HTMLInputElement> = {};
  private screenBody: HTMLElement | null = null;
  private screenOpen = false;
  private settingsPending = false;
  private attempt = 0;
  private viewVersion = 0;
  private disposed = false;
  private status: HTMLElement = el('p', '', 'sheet-status');
  private retry = button('Retry change', () => {}, 'sheet-button');
  private readonly passwordState: {
    hint: HTMLElement | null;
    remove: HTMLButtonElement | null;
    row: HTMLElement | null;
  } = { hint: null, remove: null, row: null };

  constructor(options: RoomSettingsOptions) {
    this.options = options;
  }

  get pending(): boolean {
    return this.settingsPending;
  }

  get isOpen(): boolean {
    return this.screenOpen;
  }

  sections(): SheetSection[] {
    return [
      {
        id: 'room-access',
        segment: 'room',
        label: 'Who can enter',
        available: () => this.roomAdmin(),
        status: () => {
          const settings = this.options.store.getRoom()?.roomSettings;
          if (!settings) return '';
          return [
            settings.requireRegistration
              ? 'Accounts'
              : settings.guestsAllowed
                ? 'Guests'
                : 'Members',
            settings.inviteOnly ? 'Invite only' : null,
            settings.secret ? 'Unlisted' : null,
            settings.lobbyEnabled ? 'Waiting room' : null,
          ]
            .filter(Boolean)
            .join(' · ');
        },
        screen: () => ({
          title: 'Who can enter',
          body: (context) => {
            const rows = [
              this.toggle('rs-guests', 'Allow guests', 'guestsAllowed'),
              this.toggle('rs-require-reg', 'Require an account', 'requireRegistration'),
              this.toggle('rs-invite-only', 'Invite only', 'inviteOnly'),
              this.toggle('rs-secret', 'Unlisted room', 'secret'),
              this.toggle('rs-lobby', 'Waiting room', 'lobbyEnabled'),
            ];
            const passwordRow = navRow(
              'Password',
              this.options.store.getRoom()?.roomSettings?.passwordProtected ? 'Set' : 'None',
              () => context.push(this.passwordScreen(), passwordRow),
            );
            this.passwordState.row = passwordRow;
            rows.push(passwordRow);
            return this.shell(rows);
          },
          leave: () => this.leave(),
        }),
      },
      {
        id: 'room-participation',
        segment: 'room',
        label: 'Chat & broadcasting',
        available: () => this.roomAdmin(),
        status: () => {
          const settings = this.options.store.getRoom()?.roomSettings;
          if (!settings) return '';
          return [
            settings.moderated ? 'Moderated' : 'Open',
            settings.allowChat ? null : 'Chat off',
            settings.pushToTalk ? 'Hold to talk' : null,
          ]
            .filter(Boolean)
            .join(' · ');
        },
        screen: () => ({
          title: 'Chat & broadcasting',
          body: () =>
            this.shell(
              [
                this.toggle('rs-guests-broadcast', 'Let guests broadcast', 'guestsCanBroadcast'),
                this.toggle('rs-moderated', 'Moderated participation', 'moderated'),
                this.toggle('rs-video', 'Allow cameras', 'allowVideo'),
                this.toggle('rs-chat', 'Allow chat', 'allowChat'),
                this.toggle('rs-screen', 'Allow screen sharing', 'allowScreenSharing'),
                this.toggle('rs-ptt', 'Require push to talk', 'pushToTalk'),
                this.limitRow('rs-max-broadcasters', 'Broadcaster limit', 'maxBroadcasters'),
                this.limitRow('rs-max-participants', 'Participant limit', 'maxParticipants'),
              ],
              [note('A blank limit uses the server limit; 0 allows no one.')],
            ),
          leave: () => this.leave(),
        }),
      },
    ];
  }

  /** Mirrors confirmed settings, without overwriting an in-flight draft. */
  sync(): void {
    if (this.settingsPending || this.disposed) return;
    const settings = this.options.store.getRoom()?.roomSettings;
    if (!settings) return;
    const checks: [string, boolean][] = [
      ['rs-moderated', settings.moderated],
      ['rs-lobby', settings.lobbyEnabled],
      ['rs-screen', settings.allowScreenSharing],
      ['rs-chat', settings.allowChat],
      ['rs-guests', settings.guestsAllowed],
      ['rs-guests-broadcast', settings.guestsCanBroadcast],
      ['rs-require-reg', settings.requireRegistration],
      ['rs-invite-only', settings.inviteOnly],
      ['rs-secret', settings.secret],
      ['rs-video', settings.allowVideo],
      ['rs-ptt', settings.pushToTalk],
    ];
    for (const [id, value] of checks) {
      const control = this.controls[id];
      if (control) control.checked = value;
    }
    const broadcasters = this.controls['rs-max-broadcasters'];
    if (broadcasters) broadcasters.value = settings.maxBroadcasters?.toString() ?? '';
    const participants = this.controls['rs-max-participants'];
    if (participants) participants.value = settings.maxParticipants?.toString() ?? '';
    const password = this.controls['rs-password'];
    if (password) password.value = '';
    if (this.passwordState.remove) this.passwordState.remove.hidden = !settings.passwordProtected;
    if (this.passwordState.hint)
      this.passwordState.hint.textContent = settings.passwordProtected
        ? 'A password is set. Enter a new one to replace it, or remove it.'
        : 'No password. Enter one to require it when joining.';
    const shown = this.passwordState.row?.querySelector<HTMLElement>('.sheet-row-status');
    if (shown) shown.textContent = settings.passwordProtected ? 'Set' : 'None';
  }

  /** Retires pending confirmation before reconnecting or leaving a room. */
  retire(): void {
    this.attempt++;
    this.settingsPending = false;
    this.screenBody?.setAttribute('aria-busy', 'false');
    for (const field of Object.values(this.controls)) field.disabled = false;
    this.retry.hidden = true;
    this.retry.onclick = null;
    this.status.textContent = '';
    this.status.hidden = true;
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.retire();
    this.leave();
  }

  private roomAdmin(): boolean {
    const role = this.options.store.getRoom()?.role;
    return !this.disposed && (role === 'owner' || role === 'admin');
  }

  private statusLine(): HTMLElement[] {
    this.status = el('p', '', 'sheet-status');
    this.status.id = 'room-settings-result';
    this.status.setAttribute('role', 'status');
    this.status.hidden = true;
    this.retry = button('Retry change', () => {}, 'sheet-button');
    this.retry.hidden = true;
    return [this.status, this.retry];
  }

  private toggle(id: string, label: string, key: BooleanRoomSetting): HTMLElement {
    const toggle = toggleRow(
      label,
      Boolean(this.options.store.getRoom()?.roomSettings?.[key]),
      () => {},
      { id },
    );
    toggle.input.addEventListener(
      'change',
      this.options.asyncAction(() => {
        const value = toggle.input.checked;
        return this.apply((owner) => owner.updateRoomSettings({ [key]: value }));
      }, 'Could not save the room setting'),
    );
    this.controls[id] = toggle.input;
    return toggle.row;
  }

  private limitRow(
    id: string,
    label: string,
    key: 'maxBroadcasters' | 'maxParticipants',
  ): HTMLElement {
    const field = el('input');
    field.type = 'number';
    field.id = id;
    field.min = '0';
    field.step = '1';
    field.placeholder = 'No room limit';
    field.addEventListener(
      'change',
      this.options.asyncAction(async () => {
        const value = this.limit(field);
        if (value === undefined) return;
        await this.apply((owner) => owner.updateRoomSettings({ [key]: value }));
      }, 'Could not update the limit'),
    );
    this.controls[id] = field;
    return fieldRow(label, field);
  }

  private shell(rows: HTMLElement[], extra: HTMLElement[] = []): HTMLElement {
    const body = el('div', undefined, 'sheet-screen');
    this.viewVersion++;
    this.screenBody = body;
    this.screenOpen = true;
    body.append(group(null, rows), ...extra, ...this.statusLine());
    this.sync();
    return body;
  }

  private leave(): void {
    this.viewVersion++;
    const password = this.controls['rs-password'];
    if (password) password.value = '';
    this.controls = {};
    this.screenBody = null;
    this.screenOpen = false;
    this.passwordState.hint = null;
    this.passwordState.remove = null;
    this.passwordState.row = null;
  }

  private passwordScreen(): SheetScreen {
    let field: HTMLInputElement | null = null;
    let passwordBody: HTMLElement | null = null;
    let parent: {
      body: HTMLElement | null;
      status: HTMLElement;
      retry: HTMLButtonElement;
    } | null = null;
    return {
      title: 'Password',
      body: () => {
        parent = { body: this.screenBody, status: this.status, retry: this.retry };
        const password = el('input');
        field = password;
        password.type = 'password';
        password.id = 'rs-password';
        password.autocomplete = 'new-password';
        password.placeholder = 'New password';
        password.addEventListener(
          'change',
          this.options.asyncAction(async () => {
            const value = password.value;
            const passwordBytes = new TextEncoder().encode(value).length;
            if (value && (passwordBytes < 8 || passwordBytes > 256)) {
              this.options.notify('Room password must be 8-256 bytes');
              password.focus();
              return;
            }
            await this.apply((owner) => owner.updateRoomSettings({ password: value || null }));
          }, 'Could not update the room password'),
        );
        this.controls['rs-password'] = password;
        const hint = note('');
        this.passwordState.hint = hint;
        const remove = button('Remove password', () => {}, 'sheet-button danger');
        remove.id = 'rs-password-remove';
        remove.addEventListener(
          'click',
          this.options.asyncAction(
            () => this.apply((owner) => owner.updateRoomSettings({ password: null })),
            'Could not remove the room password',
          ),
        );
        this.passwordState.remove = remove;
        const actions = el('div', undefined, 'sheet-actions');
        actions.append(remove);
        const body = el('div', undefined, 'sheet-screen');
        passwordBody = body;
        this.viewVersion++;
        this.screenBody = body;
        body.append(
          group(null, [fieldRow('New password', password)]),
          hint,
          actions,
          ...this.statusLine(),
        );
        this.sync();
        return body;
      },
      leave: () => {
        this.viewVersion++;
        if (field) field.value = '';
        delete this.controls['rs-password'];
        this.passwordState.hint = null;
        this.passwordState.remove = null;
        // The sheet caches its parent body across a push/pop. Restore the
        // parent's feedback nodes, rather than keeping the detached child owner.
        if (parent && this.screenBody === passwordBody) {
          this.screenBody = parent.body;
          this.status = parent.status;
          this.retry = parent.retry;
          this.retry.hidden = true;
          this.retry.onclick = null;
          this.sync();
        }
      },
    };
  }

  private async apply(change: (owner: RoomClient) => Promise<void>): Promise<void> {
    const owner = this.options.store.getRoom();
    if (!owner || this.settingsPending || this.disposed) return;
    const membership = owner.membershipVersion;
    const attempt = ++this.attempt;
    const current = (): boolean =>
      !this.disposed &&
      attempt === this.attempt &&
      this.options.store.getRoom() === owner &&
      owner.membershipVersion === membership;
    const view = this.viewVersion;
    const ownsView = (): boolean => view === this.viewVersion && this.screenOpen;
    const fields = Object.values(this.controls);
    const drafts = fields.map((field) => ({ field, value: field.value, checked: field.checked }));
    const status = this.status;
    const retry = this.retry;
    this.settingsPending = true;
    status.hidden = false;
    status.textContent = 'Saving change…';
    this.screenBody?.setAttribute('aria-busy', 'true');
    retry.hidden = true;
    retry.onclick = null;
    for (const field of fields) field.disabled = true;
    let confirmed = false;
    try {
      await change(owner);
      confirmed = true;
      if (current() && ownsView()) status.textContent = 'Change saved';
    } catch (error) {
      if (current() && ownsView()) {
        status.textContent =
          error instanceof Error ? error.message : 'The change was not confirmed';
        status.scrollIntoView({ block: 'nearest' });
      }
    } finally {
      if (current()) {
        this.settingsPending = false;
        this.screenBody?.setAttribute('aria-busy', 'false');
        for (const field of fields) field.disabled = false;
        this.sync();
        if (!confirmed && ownsView()) {
          for (const draft of drafts) {
            draft.field.value = draft.value;
            draft.field.checked = draft.checked;
          }
          retry.hidden = false;
          retry.onclick = this.options.asyncAction(
            () => (current() && ownsView() ? this.apply(change) : Promise.resolve()),
            'Could not save the room setting',
          );
        }
      }
    }
  }

  /** Blank clears a limit; malformed or fractional input must never silently clear it. */
  private limit(input: HTMLInputElement): number | null | undefined {
    if (input.validity.badInput) {
      input.reportValidity();
      return undefined;
    }
    if (input.value === '') return null;
    const value = input.valueAsNumber;
    if (
      !input.checkValidity() ||
      !Number.isSafeInteger(value) ||
      value < 0 ||
      value > 4_294_967_295
    ) {
      this.status.hidden = false;
      this.status.textContent =
        'Enter a whole number from 0 to 4294967295, or leave the limit blank.';
      input.focus();
      return undefined;
    }
    return value;
  }
}
