import { button, el } from './ui';

/** Explicit update control, with the current call owner rechecked on every click. */
export function createAppUpdateNotice(toastContainer: HTMLElement, reloadWhenIdle: () => boolean) {
  let appUpdateNotice: HTMLElement | null = null;
  function renderAppUpdateAvailable(available: boolean): void {
    if (!available) {
      appUpdateNotice?.remove();
      appUpdateNotice = null;
      return;
    }
    if (appUpdateNotice?.isConnected) return;
    const notice = el('div', undefined, 'toast toast-action');
    notice.id = 'app-update-notice';
    const message = el('p', 'An app update is ready. Update when you are outside a call.');
    message.setAttribute('role', 'status');
    const actions = el('div', undefined, 'toast-actions');
    actions.append(
      button('Update app', () => {
        if (!reloadWhenIdle())
          message.textContent =
            'Leave the call when you are ready, then choose Update app. Your call will continue until you leave it.';
      }),
    );
    notice.append(message, actions);
    toastContainer.append(notice);
    appUpdateNotice = notice;
  }
  return { renderAppUpdateAvailable, dispose: () => renderAppUpdateAvailable(false) };
}
