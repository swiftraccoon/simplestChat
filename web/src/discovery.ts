import { api, asyncButton, button, el, field, input, modal } from './ui';
import { contactId, type Contact, type SavedRoom } from './discovery-validation';

interface AccountOptions {
  getToken: () => string | null;
  getAccountId: () => string | null;
  current: () => boolean;
}
export interface DiscoveryMount {
  refresh: () => Promise<void>;
  dispose: () => void;
}

export function contactLink(id: string): string {
  const url = new URL(window.location.origin);
  url.hash = `contact=${contactId(id)}`;
  return url.href;
}

/** Contact identifiers grant no authority: the recipient must accept separately. */
export function parseContactLink(value: string): string {
  try {
    const url = new URL(value.trim());
    if (
      url.origin !== window.location.origin ||
      url.username ||
      url.password ||
      url.pathname !== '/' ||
      url.search ||
      !url.hash.startsWith('#contact=')
    )
      throw new Error();
    return contactId(url.hash.slice('#contact='.length));
  } catch {
    throw new Error('Paste a contact link from this site');
  }
}

/** Scrub a contact offer before sign-in; never sends or accepts a request itself. */
export function takeContactOffer(): string | null {
  if (!window.location.hash.startsWith('#contact=')) return null;
  const value = window.location.href;
  const clean = new URL(value);
  clean.hash = '';
  window.history.replaceState(window.history.state, '', clean.href);
  try {
    return parseContactLink(value);
  } catch {
    return null;
  }
}

function accountContext(options: AccountOptions): {
  current: () => boolean;
  token: () => string;
  dispose: () => void;
  signal: AbortSignal;
} {
  const account = options.getAccountId();
  const controller = new AbortController();
  const current = (): boolean =>
    !controller.signal.aborted &&
    options.current() &&
    account !== null &&
    account === options.getAccountId() &&
    !!options.getToken();
  return {
    current,
    token: () => {
      const token = options.getToken();
      if (!current() || !token) throw new Error('Sign in to manage this account');
      return token;
    },
    signal: controller.signal,
    dispose: () => controller.abort(),
  };
}

export function offerContact(peer: string, options: Omit<AccountOptions, 'current'>): void {
  const account = options.getAccountId();
  if (!account || !options.getToken()) return;
  const view = modal('Add contact');
  view.dialog.setAttribute('data-account-dialog', 'true');
  const context = accountContext({ ...options, current: () => view.dialog.open });
  view.body.append(
    el('p', 'Send a contact request? You can message each other after they accept.'),
  );
  const send = asyncButton(
    'Send request',
    async () => {
      send.disabled = true;
      try {
        await api.requestContact(context.token(), contactId(peer), context.signal);
        if (context.current()) {
          view.body.replaceChildren(
            el('p', 'Request sent if this account is available. Manage requests in Messages.'),
          );
        }
      } finally {
        if (context.current()) send.disabled = false;
      }
    },
    (error) => {
      if (context.current()) {
        view.error.textContent = error instanceof Error ? error.message : 'Request unavailable';
        view.error.hidden = false;
      }
    },
    'btn-primary',
  );
  view.body.append(send);
  view.dialog.addEventListener('close', context.dispose, { once: true });
}

export function mountContacts(
  host: HTMLElement,
  options: AccountOptions & {
    onMessage: (peerId: string, accountName: string) => void;
  },
): DiscoveryMount {
  const context = accountContext(options);
  const section = el('details', undefined, 'discovery-contacts');
  const summary = el('summary', 'Contacts');
  const actions = el('div', undefined, 'discovery-actions');
  const status = el('p', '', 'setting-hint');
  status.setAttribute('role', 'status');
  const contacts = el('div', undefined, 'discovery-list');
  const error = el('p', '', 'community-error');
  error.hidden = true;
  const fail = (reason: unknown): void => {
    if (!context.current()) return;
    error.textContent = reason instanceof Error ? reason.message : 'Contacts unavailable';
    error.hidden = false;
  };
  const copy = asyncButton(
    'Copy my contact link',
    async () => {
      if (!context.current()) return;
      await navigator.clipboard.writeText(contactLink(options.getAccountId()!));
      if (context.current()) status.textContent = 'Contact link copied';
    },
    fail,
  );
  const link = input('', 'url', 2048);
  link.placeholder = 'Paste a contact link';
  link.autocomplete = 'off';
  const form = el('form', undefined, 'discovery-contact-form');
  const request = button('Send request', () => {});
  request.type = 'submit';
  form.append(field('Contact link', link), request);
  let pending = false;
  const refresh = async (): Promise<void> => {
    if (!context.current() || pending) return;
    pending = true;
    try {
      const page = await api.contacts(context.token(), context.signal);
      if (!context.current() || page.accountId !== options.getAccountId()) return;
      error.hidden = true;
      const incoming = page.contacts.filter((contact) => contact.status === 'incoming').length;
      summary.textContent = incoming
        ? `Contacts · ${incoming} request${incoming === 1 ? '' : 's'}`
        : 'Contacts';
      contacts.replaceChildren(...page.contacts.map(contactRow));
      if (!page.contacts.length)
        contacts.append(
          el(
            'p',
            'Share a contact link to start a conversation without joining a room.',
            'setting-hint',
          ),
        );
    } finally {
      pending = false;
    }
  };
  const action = (label: string, contact: Contact, accept: boolean): HTMLButtonElement => {
    const control = asyncButton(
      label,
      async () => {
        if (!context.current()) return;
        control.disabled = true;
        try {
          if (accept) await api.acceptContact(context.token(), contact.accountId, context.signal);
          else await api.removeContact(context.token(), contact.accountId, context.signal);
          if (context.current()) await refresh();
        } finally {
          if (context.current()) control.disabled = false;
        }
      },
      fail,
    );
    return control;
  };
  const contactRow = (contact: Contact): HTMLElement => {
    const row = el('div', undefined, 'discovery-row');
    const name = el('div', undefined, 'discovery-label');
    name.append(el('strong', contact.accountName));
    if (contact.status !== 'accepted')
      name.append(
        el(
          'span',
          contact.status === 'incoming' ? 'Wants to be a contact' : 'Waiting for acceptance',
          'setting-hint',
        ),
      );
    row.append(name);
    const controls = el('div', undefined, 'discovery-actions');
    if (contact.status === 'accepted')
      controls.append(
        button('Message', () => {
          if (context.current()) options.onMessage(contact.accountId, contact.accountName);
        }),
      );
    if (contact.status === 'incoming') controls.append(action('Accept', contact, true));
    controls.append(
      action(
        contact.status === 'outgoing'
          ? 'Cancel'
          : contact.status === 'incoming'
            ? 'Decline'
            : 'Remove',
        contact,
        false,
      ),
    );
    row.append(controls);
    return row;
  };
  form.addEventListener('submit', (event) => {
    event.preventDefault();
    if (request.disabled || !context.current()) return;
    request.disabled = true;
    (async () => {
      await api.requestContact(context.token(), parseContactLink(link.value), context.signal);
      if (!context.current()) return;
      link.value = '';
      status.textContent = 'Request sent if this account is available';
      await refresh();
    })()
      .catch(fail)
      .finally(() => {
        if (context.current()) request.disabled = false;
      });
  });
  const reload = asyncButton('Refresh contacts', refresh, fail);
  actions.append(copy, reload);
  section.append(summary, actions, form, status, error, contacts);
  host.append(section);
  refresh().catch(fail);
  return {
    refresh,
    dispose: () => {
      context.dispose();
      section.remove();
    },
  };
}

export function createFavoriteButton(
  roomId: string,
  initialFavorite: boolean,
  options: AccountOptions & {
    onChange?: (favorite: boolean) => void;
    onError: (reason: unknown) => void;
  },
): HTMLButtonElement {
  const account = options.getAccountId();
  let favorite = initialFavorite;
  const current = (): boolean =>
    options.current() && !!account && account === options.getAccountId() && !!options.getToken();
  const control = asyncButton(
    '',
    async () => {
      if (!current()) return;
      control.disabled = true;
      try {
        await api.saveRoom(options.getToken()!, roomId, !favorite, new AbortController().signal);
        if (!current()) return;
        favorite = !favorite;
        render();
        options.onChange?.(favorite);
      } finally {
        if (current()) control.disabled = false;
      }
    },
    (error) => {
      if (current()) options.onError(error);
    },
    'btn-secondary discovery-favorite',
  );
  const render = (): void => {
    control.textContent = favorite ? '★' : '☆';
    control.setAttribute(
      'aria-label',
      favorite ? 'Remove from favorite rooms' : 'Add to favorite rooms',
    );
    control.setAttribute('aria-pressed', String(favorite));
    control.title = favorite ? 'Remove favorite' : 'Favorite room';
  };
  render();
  return control;
}

export function mountSavedRooms(
  host: HTMLElement,
  options: AccountOptions & {
    onJoin: (roomId: string) => void;
  },
): DiscoveryMount {
  const context = accountContext(options);
  const section = el('section', undefined, 'discovery-saved-rooms');
  section.setAttribute('aria-label', 'Favorite and recent rooms');
  const list = el('div', undefined, 'discovery-list');
  const status = el('p', '', 'setting-hint');
  status.setAttribute('role', 'status');
  const fail = (reason: unknown): void => {
    if (context.current())
      status.textContent = reason instanceof Error ? reason.message : 'Saved rooms unavailable';
  };
  let pending = false;
  const refresh = async (): Promise<void> => {
    if (!context.current() || pending) return;
    pending = true;
    try {
      const page = await api.savedRooms(context.token(), context.signal);
      if (!context.current()) return;
      status.textContent = '';
      list.replaceChildren();
      for (const [heading, entries] of [
        ['Favorites', page.rooms.filter((room) => room.favorite)],
        ['Recent rooms', page.rooms.filter((room) => !room.favorite)],
      ] as const) {
        if (!entries.length) continue;
        list.append(el('h3', heading));
        for (const entry of entries) list.append(roomRow(entry));
      }
      if (!page.rooms.length)
        status.textContent = 'Favorite rooms and rooms you join appear here across your devices.';
    } finally {
      pending = false;
    }
  };
  const roomRow = (entry: SavedRoom): HTMLElement => {
    const row = el('div', undefined, 'discovery-row');
    const join = button(
      '',
      () => {
        if (context.current()) options.onJoin(entry.room.id);
      },
      'discovery-room-link',
    );
    join.append(el('strong', entry.room.display_name));
    if (entry.room.topic) join.append(el('span', entry.room.topic, 'setting-hint'));
    row.append(
      join,
      createFavoriteButton(entry.room.id, entry.favorite, {
        ...options,
        current: context.current,
        onError: fail,
        onChange: () => {
          refresh().catch(fail);
        },
      }),
    );
    return row;
  };
  section.append(list, status);
  host.append(section);
  refresh().catch(fail);
  return {
    refresh,
    dispose: () => {
      context.dispose();
      section.remove();
    },
  };
}
