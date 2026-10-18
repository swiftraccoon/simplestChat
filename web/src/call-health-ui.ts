import { button, el } from './ui';
import type { CallHealthSnapshot } from './call-health';

export function mountCallHealth(
  host: HTMLElement,
  actions: {
    retryPlayback: () => void;
    speaker: () => void;
    capture: () => void;
    refreshIncoming: () => void;
    canRefresh: () => boolean;
  },
): (snapshot: CallHealthSnapshot) => void {
  const rows = (['sending', 'receiving', 'playback'] as const).map((name) => {
    const section = el('section', undefined, 'call-health-section');
    section.setAttribute('data-call-health', name);
    const heading = el(
      'h3',
      { sending: 'Sending', receiving: 'Receiving', playback: 'Playback' }[name],
    );
    const status = el('p', 'Checking', 'call-health-status');
    const detail = el('p', '', 'setting-hint');
    section.append(heading, status, detail);
    host.append(section);
    return { name, section, status, detail };
  });
  const controls = el('div', undefined, 'call-health-actions');
  const refresh = button('Refresh incoming media', actions.refreshIncoming, 'btn-secondary');
  controls.append(
    button('Retry playback', actions.retryPlayback, 'btn-secondary'),
    button('Speaker settings', actions.speaker, 'btn-secondary'),
    button('Camera & mic', actions.capture, 'btn-secondary'),
    refresh,
  );
  host.append(
    controls,
    el(
      'p',
      'These checks stay in this browser. They work with reliability sharing off and never start a microphone, camera or screen share.',
      'setting-hint',
    ),
  );
  return (snapshot) => {
    for (const { name, section, status, detail } of rows) {
      const value = snapshot[name];
      section.setAttribute('data-health-level', value.level);
      status.textContent = value.label;
      detail.textContent = value.detail;
    }
    refresh.disabled = !actions.canRefresh();
  };
}
