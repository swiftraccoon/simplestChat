/** Account surfaces are destinations; room entry and capture remain explicit actions. */
export type AppDestination =
  | { kind: 'inbox' }
  | { kind: 'conversation'; peerId: string; peerName?: string }
  | { kind: 'settings'; section?: string; segment?: 'you' | 'room' };

export interface AppLocation {
  room: string;
  destination: AppDestination | null;
}
const roomPattern = /^[A-Za-z0-9_-]{1,128}$/;
const peerPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const sectionPattern = /^[a-z][a-z0-9-]{0,63}$/;

export function parseAppLocation(hash: string): AppLocation | null {
  const value = hash.replace(/^#/, '');
  if (value.length > 768) return null;
  if (!value.includes('=')) {
    try {
      const room = decodeURIComponent(value);
      return room === '' || roomPattern.test(room) ? { room, destination: null } : null;
    } catch {
      return null;
    }
  }
  const params = new URLSearchParams(value);
  if (
    [...params.keys()].some((key) => !['room', 'view', 'peer', 'section', 'segment'].includes(key))
  )
    return null;
  if ([...params.keys()].some((key) => params.getAll(key).length !== 1)) return null;
  const room = params.get('room') ?? '';
  if (room && !roomPattern.test(room)) return null;
  const kind = params.get('view');
  if (kind === 'inbox' && !params.has('peer') && !params.has('section') && !params.has('segment'))
    return { room, destination: { kind } };
  if (kind === 'conversation' && !params.has('section') && !params.has('segment')) {
    const peerId = params.get('peer');
    return peerId && peerPattern.test(peerId)
      ? { room, destination: { kind, peerId: peerId.toLowerCase() } }
      : null;
  }
  if (kind === 'settings' && !params.has('peer')) {
    const section = params.get('section');
    const segment = params.get('segment');
    if (
      (section !== null && !sectionPattern.test(section)) ||
      (segment !== null && segment !== 'you' && segment !== 'room')
    )
      return null;
    return {
      room,
      destination: { kind, ...(section && { section }), ...(segment && { segment }) },
    };
  }
  return null;
}

export function appHash(room: string, destination: AppDestination | null): string {
  if (!destination) return room ? `#${room}` : '';
  const params = new URLSearchParams();
  if (room) params.set('room', room);
  params.set('view', destination.kind);
  if (destination.kind === 'conversation') params.set('peer', destination.peerId);
  if (destination.kind === 'settings') {
    if (destination.section) params.set('section', destination.section);
    if (destination.segment) params.set('segment', destination.segment);
  }
  return `#${params}`;
}

export interface NavigationSurface {
  owner?: object;
  /** Absent for short-lived confirmations; these still close before leaving a call. */
  destination?: AppDestination;
  accountId?: string;
  canLeave: () => boolean;
  close: () => void;
  /** Same-page settings stacks can restore their exact screen without serializing state. */
  restore?: () => void;
  group?: 'settings';
}
interface Surface extends NavigationSurface {
  id: number;
  returnPosition: number;
}
interface Snapshot {
  room: string;
  account: string | null;
  surfaces: Surface[];
  position: number;
}
interface Options {
  getRoom: () => string;
  getAccountId: () => string | null;
  ready: () => boolean;
  restore: (destination: AppDestination) => void;
  error: (message: string) => void;
}

/** One page-owned history stack. Its state contains no credentials or drafts. */
export class AppNavigation {
  private surfaces: Surface[] = [];
  private readonly snapshots = new Map<number, Snapshot>();
  private sequence = 0;
  private readonly session = Array.from(crypto.getRandomValues(new Uint32Array(4)), (value) =>
    value.toString(16).padStart(8, '0'),
  ).join('');
  private entry = 0;
  private position = 0;
  private changing = false;
  private restoring: Surface | null = null;
  private account: string | null;
  private pending: AppLocation | null;

  constructor(private readonly options: Options) {
    this.account = options.getAccountId();
    this.pending = parseAppLocation(window.location.hash);
  }

  readRoom(): string | null {
    return parseAppLocation(window.location.hash)?.room ?? null;
  }

  /** Called by the central modal factory, including for nested confirmations. */
  opened(surface: NavigationSurface, replace = false): () => void {
    const previous = this.surfaces[this.surfaces.length - 1];
    const record = this.restoring ?? {
      ...surface,
      id: ++this.sequence,
      returnPosition:
        surface.group === 'settings' && previous?.group === 'settings'
          ? previous.returnPosition
          : this.position,
    };
    Object.assign(record, surface);
    if (this.restoring) {
      this.surfaces.push(record);
      this.restoring = null;
    } else {
      if (
        surface.group === 'settings' &&
        this.surfaces[this.surfaces.length - 1]?.group === 'settings'
      )
        this.surfaces.pop();
      this.surfaces.push(record);
      if (!this.changing) this.save(!replace);
    }
    return () => this.closed(record);
  }

  /** Retire a replaced offer before its queued native close event can traverse history. */
  retire(owner: object): void {
    const index = this.surfaces.findIndex((surface) => surface.owner === owner);
    if (index >= 0) this.surfaces.splice(index, 1);
  }

  /** Scrub an arriving offer while keeping the call and underlying account view. */
  preserveLocation(): void {
    this.save(false);
  }

  /** A settings Back button uses the same browser traversal as the system Back action. */
  back(): boolean {
    const top = this.surfaces[this.surfaces.length - 1];
    if (!top || this.position <= top.returnPosition) return false;
    window.history.go(-1);
    return true;
  }

  /** Auth restoration permits reading a route, never joining, sending, or capturing. */
  synchronize(): void {
    const account = this.options.getAccountId();
    if (this.account !== account) {
      const previous = this.account;
      this.account = account;
      if (previous !== null) {
        this.pending = null;
        this.dropSurfaces();
        this.snapshots.clear();
        this.write(this.options.getRoom(), null, false);
      }
    }
    if (!this.options.ready() || !this.pending?.destination) return;
    if (this.pending.destination.kind !== 'settings' && !account) return;
    const destination = this.pending.destination;
    this.pending = null;
    this.changing = true;
    try {
      this.options.restore(destination);
      this.save(false);
    } finally {
      this.changing = false;
    }
  }

  /** RoomNavigation delegates location events here before considering a room exit. */
  locationChanged(selectedRoom: string): boolean {
    const location = parseAppLocation(window.location.hash);
    if (!location) return false;
    if (!this.surfaces.every((surface) => surface.canLeave())) {
      this.save(false);
      this.options.error('Finish the current change before leaving this view.');
      return true;
    }
    if (location.room !== selectedRoom) {
      this.dropSurfaces();
      this.pending = location;
      return false;
    }
    const owner = this.stateAccount();
    if (owner !== undefined && owner !== null && owner !== this.options.getAccountId()) {
      this.dropSurfaces();
      this.pending = null;
      this.write(selectedRoom, null, false);
      return true;
    }
    const id = this.stateId();
    const snapshot = id === null ? undefined : this.snapshots.get(id);
    this.changing = true;
    try {
      if (
        snapshot &&
        snapshot.room === location.room &&
        snapshot.account === this.options.getAccountId()
      ) {
        this.restoreSnapshot(snapshot);
        this.entry = id!;
        this.position = snapshot.position;
        this.write(
          snapshot.room,
          [...this.surfaces].reverse().find((surface) => surface.destination)?.destination ?? null,
          false,
        );
      } else {
        this.dropSurfaces();
        this.pending = location;
        this.synchronize();
        if (!location.destination) this.save(false);
      }
    } finally {
      this.changing = false;
    }
    return true;
  }

  private closed(surface: Surface): void {
    if (this.changing) return;
    if (surface.accountId && surface.accountId !== this.options.getAccountId()) {
      this.synchronize();
      return;
    }
    const index = this.surfaces.indexOf(surface);
    if (index < 0) return;
    const ownsLocation = this.stateId() === this.entry;
    if (index !== this.surfaces.length - 1 || !ownsLocation) {
      // Native close events may follow an explicit room action or a newly opened
      // view. Retire only their own surface; that newer navigation owns history.
      this.surfaces.splice(index, 1);
      const successor = this.surfaces[index];
      if (successor)
        successor.returnPosition = Math.min(successor.returnPosition, surface.returnPosition);
      if (ownsLocation) this.save(false);
      return;
    }
    const removed = this.surfaces.splice(index);
    this.changing = true;
    try {
      for (const nested of removed.slice(1).reverse()) nested.close();
    } finally {
      this.changing = false;
    }
    // A cold settings URL occupies this document's baseline entry. Closing the
    // entire sheet must return to its underlying page, not reopen that entry.
    if (surface.group === 'settings' && surface.returnPosition === 0) {
      for (const snapshot of this.snapshots.values()) {
        if (
          snapshot.position === 0 &&
          snapshot.surfaces.some((entry) => entry.group === 'settings')
        )
          snapshot.surfaces = [];
      }
    }
    // The browser owns Forward. Close-button and Escape use the same Back entry.
    if (surface.returnPosition < this.position)
      window.history.go(surface.returnPosition - this.position);
    else this.save(false);
  }

  private restoreSnapshot(snapshot: Snapshot): void {
    let shared = 0;
    while (shared < this.surfaces.length && this.surfaces[shared] === snapshot.surfaces[shared])
      shared++;
    const keepsSettings = snapshot.surfaces.some((surface) => surface.group === 'settings');
    for (const surface of this.surfaces.splice(shared).reverse()) {
      if (keepsSettings && surface.group === 'settings') continue;
      surface.close();
    }
    for (const surface of snapshot.surfaces.slice(shared)) {
      if (surface.accountId && surface.accountId !== this.options.getAccountId()) continue;
      this.restoring = surface;
      if (surface.restore) surface.restore();
      else if (surface.destination) this.options.restore(surface.destination);
      // A dismissed transient confirmation has no action to replay on Forward.
      this.restoring = null;
    }
  }

  private dropSurfaces(): void {
    const changing = this.changing;
    this.changing = true;
    try {
      for (const surface of this.surfaces.splice(0).reverse()) surface.close();
    } finally {
      this.changing = changing;
    }
  }

  private save(push: boolean): void {
    const room = this.options.getRoom();
    this.entry = ++this.sequence;
    if (push) this.position++;
    this.snapshots.set(this.entry, {
      room,
      account: this.options.getAccountId(),
      surfaces: [...this.surfaces],
      position: this.position,
    });
    // Keep this page's navigation memory bounded; old URLs still restore typed destinations.
    if (this.snapshots.size > 150) this.snapshots.delete(this.snapshots.keys().next().value!);
    this.write(
      room,
      [...this.surfaces].reverse().find((surface) => surface.destination)?.destination ?? null,
      push,
    );
  }

  private write(room: string, destination: AppDestination | null, push: boolean): void {
    const url = `${window.location.pathname}${window.location.search}${appHash(room, destination)}`;
    const state = {
      simplestchatNavigation: this.entry,
      simplestchatNavigationSession: this.session,
      simplestchatAccount: this.options.getAccountId(),
    };
    try {
      if (push) window.history.pushState(state, '', url);
      else window.history.replaceState(state, '', url);
    } catch {
      this.options.error('The browser could not update this view link.');
    }
  }

  private stateAccount(): string | null | undefined {
    const state: unknown = window.history.state;
    if (!state || typeof state !== 'object' || !('simplestchatAccount' in state)) return undefined;
    return typeof state.simplestchatAccount === 'string' ? state.simplestchatAccount : null;
  }

  private stateId(): number | null {
    const state: unknown = window.history.state;
    if (
      !state ||
      typeof state !== 'object' ||
      !('simplestchatNavigation' in state) ||
      !('simplestchatNavigationSession' in state) ||
      state.simplestchatNavigationSession !== this.session
    )
      return null;
    const id = state.simplestchatNavigation;
    return typeof id === 'number' && Number.isSafeInteger(id) ? id : null;
  }
}
