import type { AccountSession } from './api-validation';
import type { AccountSecurityFlow } from './account-security';
import { api, button, el } from './ui';

interface SessionOptions {
  container: HTMLElement;
  dialog: HTMLDialogElement;
  security: AccountSecurityFlow;
  token: () => string | null;
  current: () => boolean;
  signedOut: () => void;
  notify: (message: string) => void;
}

/** Session management shares the Account dialog's mutation ownership, so a
 * late response cannot sign out another identity or overlap a passkey change. */
export function mountAccountSessions(options: SessionOptions): { refreshControls: () => void } {
  const section = el('section', undefined, 'community-dialog-body');
  const heading = el('h3', 'Signed-in sessions');
  const status = el('p', 'Loading sessions…', 'setting-hint');
  status.setAttribute('role', 'status');
  const entries = el('div', undefined, 'community-dialog-body');
  const controller = new AbortController();
  let loading = false;
  let sessions: AccountSession[] = [];
  const controls: HTMLButtonElement[] = [];
  const current = (): boolean => options.current() && !controller.signal.aborted;
  const reportFailure = (): void => {
    if (current())
      status.textContent = 'Session controls could not be updated. Close Account and try again.';
  };
  const refreshControls = (): void => {
    for (const control of controls) control.disabled = loading || !options.security.canStart;
    others.disabled =
      loading || !options.security.canStart || !sessions.some((session) => !session.current);
    refresh.disabled = loading || !options.security.canStart;
  };
  const load = async (): Promise<void> => {
    if (!current() || loading) return;
    loading = true;
    refreshControls();
    status.textContent = 'Loading sessions…';
    try {
      const loaded = await api.accountSessions(options.token(), controller.signal);
      if (!current()) return;
      sessions = loaded;
      entries.replaceChildren();
      controls.length = 0;
      for (const session of sessions) {
        const row = el('div', undefined, 'management-entry');
        row.append(
          el('strong', session.current ? 'This session' : 'Other session'),
          el('p', `Signed in ${new Date(session.created_at).toLocaleString()}`, 'setting-hint'),
          el(
            'p',
            `Last refreshed ${new Date(session.refreshed_at).toLocaleString()} · Expires ${new Date(session.expires_at).toLocaleString()}`,
            'setting-hint',
          ),
        );
        const revoke = button(
          session.current ? 'Sign out this session' : 'Sign out session',
          () => {
            if (!current() || loading || !options.security.canStart) return;
            options.security
              .change(
                (token, signal) => api.revokeSession(token, session.id, signal),
                () => {
                  if (!current()) return;
                  if (session.current) options.signedOut();
                  else {
                    options.notify('Session signed out.');
                    load().catch(reportFailure);
                  }
                },
              )
              .catch(reportFailure);
          },
        );
        controls.push(revoke);
        row.append(revoke);
        entries.append(row);
      }
      status.textContent = `${sessions.length} signed-in ${sessions.length === 1 ? 'session' : 'sessions'}. Other browsers disconnect within a few seconds after sign-out.`;
    } catch {
      if (current())
        status.textContent = 'Sessions could not be loaded. Refresh the list to try again.';
    } finally {
      loading = false;
      if (current()) refreshControls();
    }
  };
  const others = button('Sign out other sessions', () => {
    if (
      !current() ||
      loading ||
      !options.security.canStart ||
      !sessions.some((session) => !session.current)
    )
      return;
    options.security
      .change(
        (token, signal) => api.revokeOtherSessions(token, signal),
        () => {
          if (!current()) return;
          options.notify('Other sessions signed out.');
          load().catch(reportFailure);
        },
      )
      .catch(reportFailure);
  });
  const refresh = button('Refresh sessions', () => {
    if (options.security.canStart) load().catch(reportFailure);
  });
  const actions = el('div', undefined, 'community-row');
  actions.append(others, refresh);
  section.append(heading, status, actions, entries);
  options.container.append(section);
  options.dialog.addEventListener('close', () => controller.abort(), { once: true });
  load().catch(reportFailure);
  return { refreshControls };
}
