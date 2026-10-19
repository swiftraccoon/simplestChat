import type {
  ModerationEventsPage,
  RoomBansPage,
  RoomMembersPage,
  RoomReportsPage,
  RoomSettings,
  RoomSettingsPatch,
} from './protocol';
import type { ScreenContext, SheetScreen, SheetSection } from './settings-sheet';
import {
  ApiError,
  ApiOutcomeUnknownError,
  asyncButton,
  button,
  el,
  field,
  input,
  modal,
} from './ui';
import { ROLES, asRole, rank } from './protocol';
import type { Role } from './protocol';

export interface RoomAdministrationView {
  role: Role;
  settings: RoomSettings;
}

export interface RoomAdministrationLobbyPage {
  lobby: { participantId: string; displayName: string; authenticated: boolean; role: Role }[];
  hasMore: boolean;
}

/** The HTTP implementation validates every response before these screens use it. */
export interface RoomAdministrationApi {
  lobby(
    token: string,
    id: string,
    offset: number,
    signal: AbortSignal,
  ): Promise<RoomAdministrationLobbyPage>;
  lobbyAction(
    token: string,
    id: string,
    target: string,
    action: 'admit' | 'deny',
    reason: string | undefined,
    signal: AbortSignal,
  ): Promise<void>;
  sanction(
    token: string,
    id: string,
    request: {
      targetParticipantId: string;
      kind: 'camera' | 'chat';
      enabled: boolean;
      reason?: string;
    },
    signal: AbortSignal,
  ): Promise<void>;
  overview(token: string, id: string, signal: AbortSignal): Promise<RoomAdministrationView>;
  settings(
    token: string,
    id: string,
    patch: RoomSettingsPatch,
    signal: AbortSignal,
  ): Promise<RoomAdministrationView>;
  members(token: string, id: string, offset: number, signal: AbortSignal): Promise<RoomMembersPage>;
  bans(token: string, id: string, offset: number, signal: AbortSignal): Promise<RoomBansPage>;
  reports(token: string, id: string, offset: number, signal: AbortSignal): Promise<RoomReportsPage>;
  history(
    token: string,
    id: string,
    offset: number,
    signal: AbortSignal,
  ): Promise<ModerationEventsPage>;
  setRole(
    token: string,
    id: string,
    target: string,
    role: Role,
    signal: AbortSignal,
  ): Promise<void>;
  removeBan(token: string, id: string, ban: string, signal: AbortSignal): Promise<void>;
  resolveReport(
    token: string,
    id: string,
    report: string,
    status: 'resolved' | 'dismissed',
    signal: AbortSignal,
  ): Promise<void>;
  moderate(
    token: string,
    id: string,
    request: {
      action: 'kick' | 'ban';
      targetParticipantId: string;
      reason?: string;
      durationSeconds?: number;
      reportId?: string;
    },
    signal: AbortSignal,
  ): Promise<void>;
}

export interface RoomAdministrationOptions {
  roomId: string;
  accountId: string;
  token: () => string;
  current: () => boolean;
  initial: RoomAdministrationView;
  api: RoomAdministrationApi;
  notify: (message: string) => void;
  changed?: (view: RoomAdministrationView) => void;
}

type Section = 'members' | 'bans' | 'reports' | 'history' | 'lobby';
const labels: Record<Section, string> = {
  members: 'People',
  lobby: 'Waiting room',
  bans: 'Bans',
  reports: 'Reports',
  history: 'History',
};
type BooleanSetting = {
  [K in keyof RoomSettings]-?: RoomSettings[K] extends boolean ? K : never;
}[keyof RoomSettings];

const access: [BooleanSetting, string][] = [
  ['guestsAllowed', 'Allow guests'],
  ['requireRegistration', 'Require an account'],
  ['inviteOnly', 'Invite only'],
  ['secret', 'Unlisted room'],
  ['lobbyEnabled', 'Waiting room'],
];
const participation: [BooleanSetting, string][] = [
  ['guestsCanBroadcast', 'Let guests broadcast'],
  ['moderated', 'Moderated participation'],
  ['allowVideo', 'Allow cameras'],
  ['allowChat', 'Allow chat'],
  ['allowScreenSharing', 'Allow screen sharing'],
  ['pushToTalk', 'Require push to talk'],
];

/** Saved-room screens operate on the selected record and never join its call. */
export function administrationSections(options: RoomAdministrationOptions): SheetSection[] {
  let view = options.initial;
  const adopt = (updated: RoomAdministrationView): void => {
    view = updated;
    options.changed?.(updated);
  };
  const available =
    (minimum: number): (() => boolean) =>
    () =>
      options.current() && rank(view.role) >= minimum;

  function screen(
    title: string,
    render: (
      body: HTMLElement,
      current: () => boolean,
      signal: AbortSignal,
      run: (work: () => Promise<void>, mutating?: boolean) => Promise<void>,
    ) => Promise<void>,
    minimum = 3,
  ): SheetScreen {
    const controller = new AbortController();
    let pending = false;
    let blocksLeave = false;
    let uncertain = false;
    return {
      title,
      subtitle: `${view.settings.displayName} · ${options.roomId}`,
      canLeave: () => !blocksLeave,
      leave: () => controller.abort(),
      body: async (context: ScreenContext) => {
        const body = el('div', undefined, 'sheet-screen');
        const content = el('div', undefined, 'management-list');
        const error = el('p', '', 'auth-error');
        error.hidden = true;
        error.setAttribute('role', 'alert');
        const current = (): boolean =>
          !controller.signal.aborted && context.current() && options.current();
        const run = async (work: () => Promise<void>, mutating = true): Promise<void> => {
          if (pending || !current()) return;
          pending = true;
          blocksLeave = mutating;
          error.hidden = true;
          body.setAttribute('aria-busy', 'true');
          const controls = [
            ...body.querySelectorAll<HTMLInputElement | HTMLButtonElement | HTMLSelectElement>(
              'input,button,select',
            ),
          ];
          const disabled = controls.map((control) => control.disabled);
          for (const control of controls) control.disabled = true;
          try {
            await work();
            uncertain = false;
          } catch (failure) {
            if (current()) {
              if (failure instanceof ApiOutcomeUnknownError) uncertain = true;
              if (
                failure instanceof ApiError &&
                (failure.status === 401 || failure.status === 403)
              ) {
                adopt({ ...view, role: 'user' });
                content.replaceChildren(
                  el('p', 'Your room access changed. Return to My rooms.', 'sheet-note'),
                );
              }
              error.textContent =
                failure instanceof Error ? failure.message : 'Room administration failed';
              error.hidden = false;
            }
          } finally {
            pending = false;
            blocksLeave = false;
            if (current()) {
              body.setAttribute('aria-busy', 'false');
              controls.forEach((control, index) => {
                const reload =
                  control.tagName === 'BUTTON' &&
                  ['Reload room settings', 'Refresh page', 'Retry loading room'].includes(
                    control.textContent ?? '',
                  );
                control.disabled = (disabled[index] ?? false) || (uncertain && !reload);
              });
            }
          }
        };
        const retry = asyncButton(
          'Retry loading room',
          () => run(load, false),
          () => {},
          'sheet-button',
        );
        retry.hidden = true;
        body.append(content, error, retry);
        async function load(): Promise<void> {
          const updated = await options.api.overview(
            options.token(),
            options.roomId,
            controller.signal,
          );
          if (current()) {
            adopt(updated);
            if (rank(view.role) < minimum)
              throw new Error('Your room permissions changed. Return to My rooms.');
            await render(content, current, controller.signal, run);
            retry.hidden = true;
          }
        }
        await run(async () => {
          retry.hidden = false;
          await load();
        }, false);
        return body;
      },
    };
  }

  function settingsScreen(kind: 'access' | 'participation'): SheetScreen {
    const title = kind === 'access' ? 'Who can enter' : 'Chat & broadcasting';
    return screen(
      title,
      async (body, current, signal, run) => {
        const draw = (): void => {
          body.replaceChildren();
          const baseline = view.settings;
          const controls = new Map<BooleanSetting, HTMLInputElement>();
          for (const [key, label] of kind === 'access' ? access : participation) {
            const control = input('', 'checkbox');
            control.checked = baseline[key];
            controls.set(key, control);
            body.append(field(label, control));
          }
          let participants: HTMLInputElement | null = null;
          let broadcasters: HTMLInputElement | null = null;
          let password: HTMLInputElement | null = null;
          if (kind === 'participation') {
            broadcasters = input(baseline.maxBroadcasters?.toString() ?? '', 'number');
            participants = input(baseline.maxParticipants?.toString() ?? '', 'number');
            broadcasters.min = participants.min = '1';
            broadcasters.max = '1000';
            participants.max = '10000';
            body.append(
              field('Broadcaster limit', broadcasters),
              field('Participant limit', participants),
            );
            body.append(
              el(
                'p',
                'A blank limit uses the server limit. Limits must be at least 1.',
                'sheet-note',
              ),
            );
          } else {
            password = input('', 'password', 256);
            password.autocomplete = 'new-password';
            body.append(
              el(
                'p',
                baseline.passwordProtected ? 'A room password is set.' : 'No room password is set.',
                'sheet-note',
              ),
            );
            body.append(field('New password', password));
            if (baseline.passwordProtected) {
              body.append(
                asyncButton(
                  'Remove password',
                  () =>
                    run(async () => {
                      const updated = await options.api.settings(
                        options.token(),
                        options.roomId,
                        { password: null },
                        signal,
                      );
                      if (current()) {
                        adopt(updated);
                        draw();
                        options.notify('Room password removed');
                      }
                    }),
                  () => {},
                  'sheet-button danger',
                ),
              );
            }
          }
          const limits = (control: HTMLInputElement | null, maximum: number): number | null => {
            if (!control?.value.trim()) return null;
            const value = Number(control.value);
            if (!Number.isInteger(value) || value < 1 || value > maximum)
              throw new Error(`Use a whole-number limit from 1 to ${maximum}, or leave it blank.`);
            return value;
          };
          body.append(
            asyncButton(
              'Save room settings',
              () =>
                run(async () => {
                  const patch: RoomSettingsPatch = {};
                  for (const [key, control] of controls) {
                    if (control.checked !== baseline[key])
                      Object.assign(patch, { [key]: control.checked });
                  }
                  if (kind === 'participation') {
                    const participantLimit = limits(participants, 10000);
                    const broadcasterLimit = limits(broadcasters, 1000);
                    if (participantLimit !== (baseline.maxParticipants ?? null))
                      patch.maxParticipants = participantLimit;
                    if (broadcasterLimit !== (baseline.maxBroadcasters ?? null))
                      patch.maxBroadcasters = broadcasterLimit;
                  }
                  if (password?.value) {
                    patch.password = password.value;
                    password.value = '';
                  }
                  if (!Object.keys(patch).length) return;
                  const updated = await options.api.settings(
                    options.token(),
                    options.roomId,
                    patch,
                    signal,
                  );
                  if (current()) {
                    adopt(updated);
                    draw();
                    options.notify('Room settings saved');
                  }
                }),
              () => {},
              'sheet-button btn-primary',
            ),
          );
          body.append(
            asyncButton(
              'Reload room settings',
              () =>
                run(async () => {
                  const updated = await options.api.overview(
                    options.token(),
                    options.roomId,
                    signal,
                  );
                  if (current()) {
                    adopt(updated);
                    draw();
                  }
                }, false),
              () => {},
              'sheet-button',
            ),
          );
        };
        draw();
      },
      4,
    );
  }

  function managementScreen(section: Section, title: string): SheetScreen {
    return screen(
      title,
      async (body, current, signal, run) => {
        let offset = 0;
        const load = async (): Promise<void> => {
          const token = options.token();
          const page =
            section === 'lobby'
              ? {
                  kind: 'lobby' as const,
                  data: await options.api.lobby(token, options.roomId, offset, signal),
                }
              : section === 'members'
                ? {
                    kind: 'members' as const,
                    data: await options.api.members(token, options.roomId, offset, signal),
                  }
                : section === 'bans'
                  ? {
                      kind: 'bans' as const,
                      data: await options.api.bans(token, options.roomId, offset, signal),
                    }
                  : section === 'reports'
                    ? {
                        kind: 'reports' as const,
                        data: await options.api.reports(token, options.roomId, offset, signal),
                      }
                    : {
                        kind: 'history' as const,
                        data: await options.api.history(token, options.roomId, offset, signal),
                      };
          if (!current()) return;
          body.replaceChildren();
          const count =
            page.kind === 'lobby'
              ? page.data.lobby.length
              : page.kind === 'members'
                ? page.data.members.length
                : page.kind === 'bans'
                  ? page.data.bans.length
                  : page.kind === 'reports'
                    ? page.data.reports.length
                    : page.data.events.length;
          if (!count) body.append(el('p', `No ${section} on this page.`, 'sheet-note'));
          const mutate = (work: () => Promise<void>): Promise<void> =>
            run(async () => {
              await work();
              if (current()) {
                const updated = await options.api.overview(options.token(), options.roomId, signal);
                if (current()) {
                  adopt(updated);
                  await load();
                }
              }
            });
          const moderate = (
            action: 'kick' | 'ban',
            target: string,
            name: string,
            reportId?: string,
          ): void => {
            if (!current()) return;
            const confirmation = modal(`${action === 'ban' ? 'Ban' : 'Kick'} ${name}?`);
            confirmation.dialog.setAttribute('data-account-dialog', 'true');
            const abort = (): void => confirmation.close();
            signal.addEventListener('abort', abort, { once: true });
            confirmation.dialog.addEventListener(
              'close',
              () => signal.removeEventListener('abort', abort),
              { once: true },
            );
            const reason = input('', 'text', 256);
            confirmation.body.append(field('Reason', reason));
            const duration = el('select');
            if (action === 'ban') {
              for (const [value, label] of [
                ['', 'Until removed'],
                ['3600', '1 hour'],
                ['86400', '1 day'],
                ['604800', '1 week'],
              ]) {
                const option = el('option', label);
                option.value = value ?? '';
                duration.append(option);
              }
              confirmation.body.append(field('Duration', duration));
            }
            const confirm = asyncButton(
              action === 'ban' ? 'Confirm ban' : 'Confirm kick',
              async () => {
                if (!current() || confirm.disabled) return;
                confirm.disabled = true;
                let completed = false;
                await mutate(async () => {
                  await options.api.moderate(
                    options.token(),
                    options.roomId,
                    {
                      action,
                      targetParticipantId: target,
                      ...(reason.value.trim() && { reason: reason.value.trim() }),
                      ...(action === 'ban' &&
                        duration.value && { durationSeconds: Number(duration.value) }),
                      ...(reportId && { reportId }),
                    },
                    signal,
                  );
                  completed = true;
                });
                if (completed || !current()) confirmation.close();
                else {
                  confirmation.error.textContent =
                    'The change was not confirmed. Close this dialog and refresh the room before taking another action.';
                  confirmation.error.hidden = false;
                }
              },
              () => {},
              'sheet-button danger',
            );
            confirmation.body.append(confirm);
          };
          if (page.kind === 'lobby') {
            for (const person of page.data.lobby) {
              const row = el('div', undefined, 'management-entry');
              row.append(el('span', `${person.displayName} · ${person.role} · waiting`));
              for (const action of ['admit', 'deny'] as const) {
                row.append(
                  asyncButton(
                    action === 'admit' ? 'Admit' : 'Deny',
                    () =>
                      mutate(() =>
                        options.api.lobbyAction(
                          options.token(),
                          options.roomId,
                          person.participantId,
                          action,
                          undefined,
                          signal,
                        ),
                      ),
                    () => {},
                    'sheet-button',
                  ),
                );
              }
              body.append(row);
            }
          } else if (page.kind === 'members') {
            for (const person of page.data.members) {
              const row = el('div', undefined, 'management-entry');
              row.append(
                el(
                  'span',
                  `${person.displayName} · ${person.role} · ${person.online ? 'online' : 'offline'}`,
                ),
              );
              if (
                person.authenticated &&
                person.userId !== options.accountId &&
                rank(view.role) > rank(person.role)
              ) {
                const role = el('select');
                role.setAttribute('aria-label', `Role for ${person.displayName}`);
                for (const candidate of ROLES.slice(1, rank(view.role))) {
                  const choice = el('option', candidate);
                  choice.value = candidate;
                  role.append(choice);
                }
                role.value = person.role;
                row.append(
                  role,
                  asyncButton(
                    'Update role',
                    () =>
                      mutate(() =>
                        options.api.setRole(
                          options.token(),
                          options.roomId,
                          person.userId,
                          asRole(role.value) ?? person.role,
                          signal,
                        ),
                      ),
                    () => {},
                    'sheet-button',
                  ),
                );
              }
              if (
                person.online &&
                rank(view.role) > rank(person.role) &&
                person.userId !== options.accountId
              ) {
                row.append(
                  button(
                    'Kick…',
                    () => moderate('kick', person.userId, person.displayName),
                    'sheet-button danger',
                  ),
                );
                row.append(
                  button(
                    'Ban…',
                    () => moderate('ban', person.userId, person.displayName),
                    'sheet-button danger',
                  ),
                );
                for (const [label, kind, enabled] of [
                  ['Restrict camera', 'camera', true],
                  ['Restore camera', 'camera', false],
                  ['Mute chat', 'chat', true],
                  ['Restore chat', 'chat', false],
                ] as const) {
                  row.append(
                    asyncButton(
                      label,
                      () =>
                        mutate(() =>
                          options.api.sanction(
                            options.token(),
                            options.roomId,
                            { targetParticipantId: person.userId, kind, enabled },
                            signal,
                          ),
                        ),
                      () => {},
                      'sheet-button',
                    ),
                  );
                }
              }
              body.append(row);
            }
          } else if (page.kind === 'bans') {
            for (const ban of page.data.bans) {
              const row = el('div', undefined, 'management-entry');
              row.append(
                el('strong', ban.displayName),
                el('p', ban.reason || 'No reason recorded'),
              );
              row.append(
                el(
                  'p',
                  ban.expiresAt
                    ? `Expires ${new Date(ban.expiresAt).toLocaleString()}`
                    : 'Until removed',
                ),
              );
              row.append(
                asyncButton(
                  'Unban',
                  () =>
                    mutate(() =>
                      options.api.removeBan(options.token(), options.roomId, ban.banId, signal),
                    ),
                  () => {},
                  'sheet-button',
                ),
              );
              body.append(row);
            }
          } else if (page.kind === 'reports') {
            for (const report of page.data.reports) {
              const row = el('div', undefined, 'management-entry');
              row.append(
                el('strong', `${report.targetName} · ${report.status}`),
                el('p', report.reason, 'profile-bio'),
              );
              row.append(
                el('p', `${report.reporterName} · ${new Date(report.createdAt).toLocaleString()}`),
              );
              if (report.status === 'open') {
                for (const status of ['resolved', 'dismissed'] as const) {
                  row.append(
                    asyncButton(
                      status === 'resolved' ? 'Mark resolved' : 'Dismiss report',
                      () =>
                        mutate(() =>
                          options.api.resolveReport(
                            options.token(),
                            options.roomId,
                            report.reportId,
                            status,
                            signal,
                          ),
                        ),
                      () => {},
                      'sheet-button',
                    ),
                  );
                }
                row.append(
                  button(
                    'Kick…',
                    () =>
                      moderate(
                        'kick',
                        report.targetParticipantId,
                        report.targetName,
                        report.reportId,
                      ),
                    'sheet-button danger',
                  ),
                );
                row.append(
                  button(
                    'Ban…',
                    () =>
                      moderate(
                        'ban',
                        report.targetParticipantId,
                        report.targetName,
                        report.reportId,
                      ),
                    'sheet-button danger',
                  ),
                );
              } else if (report.outcome) {
                row.append(
                  el(
                    'p',
                    `Led to: ${report.outcome.action.split('_').join(' ')} · ${new Date(report.outcome.createdAt).toLocaleString()}`,
                  ),
                );
              }
              body.append(row);
            }
          } else {
            for (const event of page.data.events) {
              const row = el('div', undefined, 'management-entry');
              row.append(
                el('strong', `${event.targetName} · ${event.action.split('_').join(' ')}`),
              );
              row.append(
                el(
                  'p',
                  `${event.actorName} · ${new Date(event.createdAt).toLocaleString()}${event.reportId ? ' · from a report' : ''}`,
                ),
              );
              if (event.reason) row.append(el('p', event.reason, 'profile-bio'));
              if (event.expiresAt)
                row.append(el('p', `Until ${new Date(event.expiresAt).toLocaleString()}`));
              if (view.role === 'owner' && event.targetIp)
                row.append(el('p', `Address ${event.targetIp}`, 'sheet-note'));
              body.append(row);
            }
          }
          if (offset)
            body.append(
              asyncButton(
                'Previous page',
                async () => {
                  offset = Math.max(0, offset - 100);
                  await run(load, false);
                },
                () => {},
                'sheet-button',
              ),
            );
          if (page.data.hasMore && offset < 10000)
            body.append(
              asyncButton(
                'Next page',
                async () => {
                  offset += 100;
                  await run(load, false);
                },
                () => {},
                'sheet-button',
              ),
            );
          body.append(
            asyncButton(
              'Refresh page',
              () =>
                run(async () => {
                  const updated = await options.api.overview(
                    options.token(),
                    options.roomId,
                    signal,
                  );
                  if (current()) {
                    adopt(updated);
                    await load();
                  }
                }, false),
              () => {},
              'sheet-button',
            ),
          );
        };
        await load();
      },
      section === 'bans' ? 4 : 3,
    );
  }

  return [
    {
      id: 'room-access',
      segment: 'room',
      label: 'Who can enter',
      available: available(4),
      screen: () => settingsScreen('access'),
    },
    {
      id: 'room-participation',
      segment: 'room',
      label: 'Chat & broadcasting',
      available: available(4),
      screen: () => settingsScreen('participation'),
    },
    ...(['members', 'lobby', 'bans', 'reports', 'history'] as const).map((kind) => ({
      id: `room-${kind}`,
      segment: 'room' as const,
      label: labels[kind],
      available: available(kind === 'bans' ? 4 : 3),
      screen: () => managementScreen(kind, labels[kind]),
    })),
  ];
}
