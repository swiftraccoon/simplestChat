const configuredDialogs = new WeakSet<HTMLDialogElement>();

/** Add settings navigation and dismissal; callers own showModal and any cleanup. */
export function configureSettingsDialog(
  dialog: HTMLDialogElement,
  onDismiss: () => void = () => dialog.close(),
): void {
  if (configuredDialogs.has(dialog)) return;
  configuredDialogs.add(dialog);

  const panels = Array.from(dialog.querySelectorAll<HTMLElement>('[role="tabpanel"]'));
  const tabs = Array.from(dialog.querySelectorAll<HTMLButtonElement>('[data-settings-tab]')).filter(
    (tab) => panels.some((panel) => panel.id === tab.getAttribute('aria-controls')),
  );
  let activePanel: string | null = null;
  const select = (selected: HTMLButtonElement, focus = false): void => {
    for (const tab of tabs) {
      tab.setAttribute('aria-selected', String(tab === selected));
      tab.tabIndex = tab === selected ? 0 : -1;
    }
    for (const panel of panels) panel.hidden = panel.id !== selected.getAttribute('aria-controls');
    const body = dialog.querySelector<HTMLElement>('.settings-dialog-body');
    if (body) body.scrollTop = 0;
    if (focus) selected.focus();
    const panelId = selected.getAttribute('aria-controls');
    if (panelId !== activePanel) {
      activePanel = panelId;
      dialog.dispatchEvent(new CustomEvent('settings-tab-change', { detail: panelId }));
    }
  };
  const initial =
    tabs.find((tab) => tab.getAttribute('aria-selected') === 'true' && !tab.disabled) ??
    tabs.find((tab) => !tab.disabled);
  if (initial) select(initial);

  for (const tab of tabs) {
    tab.addEventListener('click', () => {
      if (!tab.disabled) select(tab, true);
    });
    tab.addEventListener('keydown', (event) => {
      if (event.altKey || event.ctrlKey || event.metaKey) return;
      const enabled = tabs.filter((item) => !item.disabled && !item.hidden);
      const current = enabled.indexOf(tab);
      if (current < 0) return;
      let index: number;
      switch (event.key) {
        case 'ArrowLeft':
          index = (current + enabled.length - 1) % enabled.length;
          break;
        case 'ArrowRight':
          index = (current + 1) % enabled.length;
          break;
        case 'Home':
          index = 0;
          break;
        case 'End':
          index = enabled.length - 1;
          break;
        default:
          return;
      }
      event.preventDefault();
      select(enabled[index]!, true);
    });
  }

  for (const button of dialog.querySelectorAll<HTMLButtonElement>('[data-dialog-close]')) {
    button.addEventListener('click', onDismiss);
  }
  dialog.addEventListener('cancel', (event) => {
    event.preventDefault();
    onDismiss();
  });
  dialog.addEventListener('click', (event) => {
    if (event.target !== dialog) return;
    const rect = dialog.getBoundingClientRect();
    if (
      event.clientX < rect.left ||
      event.clientX > rect.right ||
      event.clientY < rect.top ||
      event.clientY > rect.bottom
    ) {
      onDismiss();
    }
  });
}
