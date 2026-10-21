import type { ServerCapabilities } from '../api-validation';
import { createFavoriteButton, mountSavedRooms, type DiscoveryMount } from '../discovery';
import type { Elements } from '../dom';
import type { RoomListItem } from '../protocol';
import { api, ApiError, el, safeRasterUrl } from '../ui';

type DirectoryElements = Pick<
  Elements,
  | 'roomBrowser'
  | 'roomSearchInput'
  | 'roomList'
  | 'roomLoadMore'
  | 'savedRoomPicker'
  | 'savedRoomContainer'
  | 'directoryRailToggle'
>;

/** Live account and selection state; callers supply getters rather than snapshots. */
export interface DirectoryStore {
  readonly capabilities: ServerCapabilities | null;
  readonly accountId: string | null;
  readonly token: string | null;
  readonly isLoggedIn: boolean;
  readonly selectedRoomId: string;
}

export interface DirectoryActions {
  enterRoom: (id: string) => void;
  observe: (task: Promise<unknown>, failureMessage: string) => void;
  notify: (message: string) => void;
  readPreference: (key: string) => string | null;
  writePreference: (key: string, value: string) => void;
  tone: (id: string) => string;
}

const MIN_ROOM_SEARCH_TRIGRAM_LEN = 3;

export function hasIndexableRoomSearchTrigram(query: string): boolean {
  let runLength = 0;
  for (const character of query) {
    if (/^[A-Za-z0-9]$/.test(character)) {
      runLength++;
      if (runLength >= MIN_ROOM_SEARCH_TRIGRAM_LEN) return true;
    } else {
      runLength = 0;
    }
  }
  return false;
}

/**
 * Owns directory requests, the search timer and the account's saved-room rail.
 * Disposing retires pending reads and removes listeners without changing room membership.
 */
export class DirectoryScreen {
  private readonly elements: DirectoryElements;
  private readonly store: DirectoryStore;
  private readonly actions: DirectoryActions;

  private page = 1;
  private query = '';
  private hasMore = false;
  private request = 0;
  private controller: AbortController | null = null;
  private savedRoomMount: DiscoveryMount | null = null;
  private savedRoomAccount: string | null = null;
  private savedRoomsEmpty = true;
  private searchDebounceTimer: ReturnType<typeof setTimeout> | null = null;
  private readonly listeners = new AbortController();
  private disposed = false;

  constructor(elements: DirectoryElements, store: DirectoryStore, actions: DirectoryActions) {
    this.elements = elements;
    this.store = store;
    this.actions = actions;

    this.setupRail();
    for (const host of [elements.roomBrowser, elements.savedRoomContainer]) {
      host.addEventListener(
        'click',
        (event) => {
          if (host === elements.roomBrowser && event.target instanceof Element) {
            const retry = event.target.closest('.directory-retry');
            if (retry && host.contains(retry)) {
              actions.observe(this.load(), 'Could not refresh the room directory');
              return;
            }
          }
          const card = this.cardForEvent(event, host);
          const id = card?.dataset['roomId'];
          if (id !== undefined) actions.enterRoom(id);
        },
        { signal: this.listeners.signal },
      );
      host.addEventListener(
        'keydown',
        (event) => {
          if (event.key !== 'Enter' && event.key !== ' ') return;
          const card = this.cardForEvent(event, host);
          if (!card) return;
          event.preventDefault();
          card.click();
        },
        { signal: this.listeners.signal },
      );
    }
    elements.roomSearchInput.addEventListener(
      'input',
      () => {
        if (this.searchDebounceTimer) clearTimeout(this.searchDebounceTimer);
        this.searchDebounceTimer = setTimeout(() => {
          this.searchDebounceTimer = null;
          if (this.disposed) return;
          const query = elements.roomSearchInput.value.trim();
          if (query && !hasIndexableRoomSearchTrigram(query)) {
            this.query = '';
            this.showSearchMinimum();
            return;
          }
          this.query = query;
          actions.observe(this.load(), 'Could not refresh the room directory');
        }, 300);
      },
      { signal: this.listeners.signal },
    );
    elements.roomLoadMore.addEventListener(
      'click',
      () => {
        this.page++;
        actions.observe(this.load(true), 'Could not load more rooms');
      },
      { signal: this.listeners.signal },
    );
  }

  refreshSavedRooms(): void {
    if (this.disposed) return;
    const account = this.store.capabilities?.roomDirectory ? this.store.accountId : null;
    if (account !== this.savedRoomAccount) {
      this.savedRoomMount?.dispose();
      this.savedRoomMount = null;
      this.savedRoomAccount = account;
      this.savedRoomsEmpty = true;
    }
    // The rail appears once the account's saved rooms are known to exist.
    this.elements.savedRoomPicker.hidden = account === null || this.savedRoomsEmpty;
    if (!account) return;
    if (this.savedRoomMount)
      this.actions.observe(this.savedRoomMount.refresh(), 'Could not refresh saved rooms');
    else
      this.savedRoomMount = mountSavedRooms(this.elements.savedRoomContainer, {
        getToken: () => this.store.token,
        getAccountId: () => this.store.accountId,
        current: () => !this.disposed && this.savedRoomAccount === account,
        renderRoom: (entry) => this.buildRoomCard(entry.room, true),
        onEmpty: (empty) => {
          this.savedRoomsEmpty = empty;
          this.elements.savedRoomPicker.hidden = empty;
        },
      });
  }

  /** Updates the directory selection after navigation has selected a room. */
  select(id: string): void {
    for (const card of this.elements.roomBrowser.querySelectorAll<HTMLElement>('.room-card'))
      card.classList.toggle('selected', card.dataset['roomId'] === id);
  }

  async load(append = false): Promise<void> {
    if (this.disposed || !this.store.capabilities?.roomDirectory) return;
    const request = ++this.request;
    this.controller?.abort();
    const controller = new AbortController();
    const account = this.store.accountId;
    this.controller = controller;
    const { roomList, roomLoadMore } = this.elements;
    if (!append) {
      this.page = 1;
      roomList.replaceChildren();
      roomLoadMore.hidden = true;
    }
    roomLoadMore.disabled = true;
    try {
      const params = new URLSearchParams({ page: String(this.page), limit: '20' });
      if (this.query) params.set('q', this.query);
      const token = this.store.token;
      const saved =
        token && account
          ? api.savedRooms(token, controller.signal).catch(() => null)
          : Promise.resolve(null);
      const rooms = await api.rooms(params, controller.signal).catch((error: unknown) => {
        if (!(error instanceof ApiError)) throw error;
        const explanation =
          error.status === 404 || error.status === 503
            ? 'The room directory is temporarily unavailable. You can still try joining a room by its ID below.'
            : error.status === 429
              ? 'Too many room searches. Wait a moment and try again.'
              : 'Could not load the room directory. You can still join directly by room name below.';
        throw new Error(explanation);
      });
      const favorites = new Set(
        (await saved)?.rooms.filter((entry) => entry.favorite).map((entry) => entry.room.id),
      );
      if (request !== this.request || this.store.accountId !== account || this.disposed) return;
      this.hasMore = rooms.length === 20;
      roomLoadMore.hidden = !this.hasMore;

      if (rooms.length === 0 && !append) {
        const empty = document.createElement('div');
        empty.className = 'room-list-empty';
        empty.textContent = this.query
          ? 'No rooms match your search'
          : this.store.isLoggedIn
            ? 'No public rooms yet — create one or join by name below.'
            : 'No public rooms are listed. Join by room name below, or sign in to create a saved room.';
        roomList.appendChild(empty);
        return;
      }

      for (const room of rooms) {
        const card = this.buildRoomCard(room);
        if (account) {
          const row = el('div', undefined, 'directory-room-row');
          const favorite = createFavoriteButton(room.id, favorites.has(room.id), {
            getToken: () => this.store.token,
            getAccountId: () => this.store.accountId,
            current: () => !this.disposed && this.store.accountId === account && card.isConnected,
            onError: (reason) =>
              this.actions.notify(
                reason instanceof Error ? reason.message : 'Favorite could not be changed',
              ),
            onChange: () => this.refreshSavedRooms(),
          });
          row.append(card, favorite);
          roomList.append(row);
        } else roomList.appendChild(card);
      }
    } catch (error) {
      if (request !== this.request || this.disposed) return;
      this.hasMore = false;
      roomLoadMore.hidden = true;
      roomList.querySelector('.directory-status')?.remove();
      const message = document.createElement('div');
      message.className = 'room-list-empty directory-status';
      message.setAttribute('role', 'status');
      message.textContent =
        error instanceof Error && !(error instanceof TypeError)
          ? error.message
          : 'The room directory could not be reached. You can still try joining directly below.';
      const retry = document.createElement('button');
      retry.type = 'button';
      retry.className = 'auth-link-btn directory-retry';
      retry.textContent = 'Retry directory';
      message.append(document.createElement('br'), retry);
      roomList.append(message);
    } finally {
      if (request === this.request && !this.disposed) roomLoadMore.disabled = false;
    }
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.request++;
    this.controller?.abort();
    this.controller = null;
    if (this.searchDebounceTimer) clearTimeout(this.searchDebounceTimer);
    this.searchDebounceTimer = null;
    this.savedRoomMount?.dispose();
    this.savedRoomMount = null;
    this.listeners.abort();
  }

  /** The rail's collapsed state belongs to this device, independently of the account. */
  private setupRail(): void {
    const { savedRoomPicker: rail, directoryRailToggle: toggle } = this.elements;
    let collapsed = this.actions.readPreference('simplestchat.directoryRail') === 'collapsed';
    const apply = (): void => {
      rail.classList.toggle('collapsed', collapsed);
      toggle.setAttribute('aria-expanded', String(!collapsed));
      toggle.setAttribute(
        'aria-label',
        collapsed ? 'Show favorites and recent rooms' : 'Hide favorites and recent rooms',
      );
    };
    apply();
    toggle.addEventListener(
      'click',
      () => {
        collapsed = !collapsed;
        apply();
        this.actions.writePreference(
          'simplestchat.directoryRail',
          collapsed ? 'collapsed' : 'open',
        );
      },
      { signal: this.listeners.signal },
    );
  }

  private showSearchMinimum(): void {
    this.request++;
    this.controller?.abort();
    this.page = 1;
    this.hasMore = false;
    this.elements.roomLoadMore.hidden = true;
    this.elements.roomList.replaceChildren();
    const message = document.createElement('div');
    message.className = 'room-list-empty';
    message.textContent = 'Enter at least 3 consecutive letters or numbers';
    this.elements.roomList.appendChild(message);
  }

  private cardForEvent(event: Event, host: HTMLElement): HTMLElement | null {
    if (!(event.target instanceof Element)) return null;
    const card = event.target.closest<HTMLElement>('.room-card');
    return card && host.contains(card) ? card : null;
  }

  /** A compact saved-room card keeps only the room's picture and name. */
  private buildRoomCard(room: RoomListItem, compact = false): HTMLElement {
    const card = document.createElement('div');
    card.className = compact ? 'room-card room-card-small' : 'room-card';
    card.tabIndex = 0;
    card.setAttribute('role', 'button');
    card.setAttribute('aria-label', `Enter ${room.display_name}`);
    card.dataset['roomId'] = room.id;
    card.dataset['initial'] = [...room.display_name.trim()][0]?.toLocaleUpperCase() ?? '';
    card.style.setProperty('--room-tone', this.actions.tone(room.id));
    card.classList.toggle('selected', room.id === this.store.selectedRoomId);

    const info = document.createElement('div');
    info.className = 'room-card-info';
    if (safeRasterUrl(room.image_url)) {
      const image = document.createElement('img');
      image.src = room.image_url;
      image.alt = '';
      image.className = 'room-card-image';
      card.append(image);
    }

    const nameRow = document.createElement('div');
    nameRow.className = 'room-card-name';
    const nameText = document.createElement('span');
    nameText.textContent = room.display_name;
    nameRow.appendChild(nameText);
    if (room.password_protected) {
      const lockSpan = document.createElement('span');
      lockSpan.insertAdjacentHTML(
        'afterbegin',
        '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="11" width="18" height="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>',
      );
      nameRow.appendChild(lockSpan);
    }
    info.appendChild(nameRow);

    if (compact) {
      card.appendChild(info);
      return card;
    }
    if (room.topic || room.description) {
      const topic = document.createElement('div');
      topic.className = 'room-card-topic';
      topic.textContent = room.topic || room.description || '';
      info.appendChild(topic);
    }
    if (room.topic && room.description) {
      const description = document.createElement('p');
      description.className = 'room-card-topic';
      description.textContent = room.description;
      description.title = room.description;
      info.appendChild(description);
    }

    const meta = document.createElement('div');
    meta.className = 'room-card-meta';
    const count = document.createElement('span');
    count.textContent = `${room.participant_count ?? '?'} online · ${room.broadcaster_count ?? '?'} broadcasting`;
    meta.appendChild(count);
    if (room.moderated) {
      const badge = document.createElement('span');
      badge.className = 'room-card-badge';
      badge.textContent = 'Moderated';
      meta.appendChild(badge);
    }

    card.appendChild(info);
    card.appendChild(meta);
    return card;
  }
}
