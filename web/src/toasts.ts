import { createScope } from './disposable';

/** Transient notices owned by the shared polite live region. */
export function createToasts(document: Document, toastContainer: HTMLElement) {
  const scope = createScope();
  const notices = new Map<HTMLElement, () => void>();
  function own(toast: HTMLElement, duration: number, animate = false): () => void {
    const timers: ReturnType<typeof setTimeout>[] = [];
    const remove = (): void => {
      for (const timer of timers) clearTimeout(timer);
      toast.remove();
      notices.delete(toast);
    };
    notices.set(toast, remove);
    if (animate)
      timers.push(
        setTimeout(() => toast.classList.add('toast-leaving'), Math.max(0, duration - 300)),
      );
    if (duration > 0 || animate) timers.push(setTimeout(remove, duration));
    return remove;
  }
  scope.onDispose(() => {
    for (const remove of notices.values()) remove();
  });
  type ToastKind = 'info' | 'error';

  /** The container is a polite live region; errors are additionally marked as alerts. */
  function showToast(message: string, duration = 3000, kind: ToastKind = 'info'): void {
    if (scope.signal.aborted) return;
    const toast = document.createElement('div');
    toast.className = kind === 'error' ? 'toast toast-error' : 'toast';
    if (kind === 'error') toast.setAttribute('role', 'alert');
    toast.textContent = message;
    toast.addEventListener('click', own(toast, duration, true), { signal: scope.signal });
    toastContainer.appendChild(toast);
  }

  function showActionToast(
    message: string,
    actions: { label: string; action: () => void }[],
    duration = 10000,
  ): HTMLElement {
    const toast = document.createElement('div');
    toast.className = 'toast toast-action';
    if (scope.signal.aborted) return toast;
    const remove = own(toast, duration);

    const msg = document.createElement('div');
    msg.textContent = message;
    toast.appendChild(msg);

    const btnRow = document.createElement('div');
    btnRow.className = 'toast-actions';
    for (const a of actions) {
      const btn = document.createElement('button');
      btn.textContent = a.label;
      btn.addEventListener(
        'click',
        () => {
          a.action();
          remove();
        },
        { signal: scope.signal },
      );
      btnRow.appendChild(btn);
    }
    toast.appendChild(btnRow);

    toastContainer.appendChild(toast);
    return toast;
  }

  return { showToast, showActionToast, dispose: scope.dispose };
}
