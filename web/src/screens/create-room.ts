import type { ServerCapabilities } from '../api-validation';
import type { Elements } from '../dom';
import type { CreateRoomRequest } from '../protocol';
import { api, ApiOutcomeUnknownError } from '../ui';

type CreateRoomElements = Pick<
  Elements,
  | 'createRoomBtn'
  | 'createRoomModal'
  | 'createRoomClose'
  | 'crId'
  | 'crName'
  | 'crTopic'
  | 'crPassword'
  | 'crModerated'
  | 'crLobby'
  | 'crSecret'
  | 'crGuests'
  | 'createRoomSubmit'
  | 'createRoomError'
>;

interface CreateRoomOptions {
  elements: CreateRoomElements;
  store: {
    readonly accountId: string | null;
    readonly token: string | null;
    readonly isLoggedIn: boolean;
    readonly capabilities: Pick<ServerCapabilities, 'roomCreation'> | null;
    readonly selectionVersion: number;
  };
  notify: (message: string, duration?: number, kind?: 'info' | 'error') => void;
  asyncAction: (action: () => Promise<void>, failureMessage: string) => () => void;
  /** Applies the existing explicit join action after the creating dialog closes. */
  created: (id: string) => void;
}

/** Room IDs appear in links: ASCII letters, digits and dashes derived from the name. */
export function suggestRoomId(displayName: string): string {
  return displayName
    .normalize('NFKD')
    .replace(/[\u0300-\u036f]/g, '')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 64)
    .replace(/-+$/g, '');
}

/** Owns the creating dialog and its mutations. Dismissal retires automatic joining,
 * but an unconfirmed mutation keeps retries disabled until identity reset or reload.
 * Dispose removes listeners, clears the password and ignores every pending result. */
export class CreateRoomScreen {
  private readonly options: CreateRoomOptions;

  private attempt = 0;
  private mutation = 0;
  private roomIdEdited = false;
  private disposed = false;
  private readonly cleanups: (() => void)[] = [];

  constructor(options: CreateRoomOptions) {
    this.options = options;

    const { elements } = options;
    this.listen(elements.createRoomBtn, 'click', () => this.open());
    this.listen(elements.createRoomClose, 'click', () => this.dismiss());
    this.listen(elements.createRoomModal, 'click', (event) => {
      if (event.target === elements.createRoomModal) this.dismiss();
    });
    this.listen(elements.createRoomModal, 'cancel', (event) => {
      event.preventDefault();
      this.dismiss();
    });
    this.listen(elements.crId, 'input', () => {
      this.roomIdEdited = elements.crId.value !== '';
    });
    this.listen(elements.crName, 'input', () => {
      if (!this.roomIdEdited) elements.crId.value = suggestRoomId(elements.crName.value);
    });
    this.listen(
      elements.createRoomSubmit,
      'click',
      options.asyncAction(() => this.submit(), 'Could not complete room creation'),
    );
  }

  /** Closes the current view without assuming an in-flight server change was cancelled. */
  dismiss(): void {
    const { elements } = this.options;
    this.attempt++;
    elements.createRoomModal.hidden = true;
    elements.createRoomModal.close();
    elements.crPassword.value = '';
    elements.createRoomError.hidden = true;
    elements.createRoomError.textContent = '';
  }

  /** Call on account changes before starting work for the replacement identity. */
  resetIdentity(): void {
    this.dismiss();
    this.mutation++;
    this.resetSubmit(false);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.mutation++;
    for (const cleanup of this.cleanups) cleanup();
    this.cleanups.length = 0;
    this.dismiss();
  }

  private listen(target: EventTarget, type: string, listener: EventListener): void {
    target.addEventListener(type, listener);
    this.cleanups.push(() => target.removeEventListener(type, listener));
  }

  private open(): void {
    const { elements, store } = this.options;
    if (this.disposed || !store.isLoggedIn || !store.capabilities?.roomCreation) return;
    this.attempt++;
    elements.createRoomModal.hidden = false;
    elements.createRoomModal.showModal();
    elements.crName.focus();
  }

  private resetSubmit(uncertain: boolean): void {
    const { createRoomSubmit } = this.options.elements;
    createRoomSubmit.disabled = uncertain;
    createRoomSubmit.textContent = uncertain ? 'Response not confirmed' : 'Create Room';
  }

  private async submit(): Promise<void> {
    if (this.disposed) return;
    const { elements, store } = this.options;
    elements.createRoomError.hidden = true;
    const id = elements.crId.value.trim();
    const displayName = elements.crName.value.trim();
    if (!id || !displayName) {
      elements.createRoomError.textContent = 'A room name and a room ID are required';
      elements.createRoomError.hidden = false;
      return;
    }
    const roomPasswordBytes = new TextEncoder().encode(elements.crPassword.value).length;
    if (elements.crPassword.value && (roomPasswordBytes < 8 || roomPasswordBytes > 256)) {
      elements.createRoomError.textContent = 'Room password must be 8-256 bytes';
      elements.createRoomError.hidden = false;
      return;
    }
    if (elements.createRoomSubmit.disabled) return;
    const attempt = ++this.attempt;
    const mutation = ++this.mutation;
    const account = store.accountId;
    const selection = store.selectionVersion;
    const current = (): boolean =>
      !this.disposed &&
      attempt === this.attempt &&
      elements.createRoomModal.open &&
      store.accountId === account &&
      selection === store.selectionVersion;
    let uncertain = false;
    elements.createRoomSubmit.disabled = true;
    elements.createRoomSubmit.textContent = 'Creating...';
    try {
      const body: CreateRoomRequest = { id, display_name: displayName };
      if (elements.crTopic.value.trim()) body.topic = elements.crTopic.value.trim();
      if (elements.crPassword.value) body.password = elements.crPassword.value;
      if (elements.crModerated.checked) body.moderated = true;
      if (elements.crLobby.checked) body.lobby_enabled = true;
      if (elements.crSecret.checked) body.secret = true;
      if (!elements.crGuests.checked) body.guests_allowed = false;

      await api.createRoom(store.token, body);
      if (!current()) {
        if (!this.disposed && store.accountId === account)
          this.options.notify('Room created. Open it from My rooms when ready.');
        return;
      }
      this.dismiss();
      this.options.created(id);
      elements.crId.value = '';
      elements.crName.value = '';
      elements.crTopic.value = '';
      elements.crPassword.value = '';
      elements.crModerated.checked = false;
      elements.crLobby.checked = false;
      elements.crSecret.checked = false;
      elements.crGuests.checked = true;
      this.roomIdEdited = false;
    } catch (error) {
      uncertain = error instanceof ApiOutcomeUnknownError;
      if (!current()) {
        if (
          !this.disposed &&
          error instanceof ApiOutcomeUnknownError &&
          store.accountId === account
        )
          this.options.notify(error.message, 8000, 'error');
        return;
      }
      elements.createRoomError.textContent =
        error instanceof Error ? error.message : 'Failed to create room';
      elements.createRoomError.hidden = false;
    } finally {
      if (!this.disposed && mutation === this.mutation) this.resetSubmit(uncertain);
    }
  }
}
