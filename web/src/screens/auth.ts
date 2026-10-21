import type { ServerCapabilities } from '../api-validation';
import { SessionOutcomeUnknownError, type AuthManager } from '../auth';
import { AuthDialogFlow, type AuthDialogAttempt } from '../auth-dialog';
import type { Elements } from '../dom';
import type { ClientTelemetry } from '../telemetry';
import { button, el, validatePassword } from '../ui';

export type AuthScreenElements = Pick<
  Elements,
  | 'signInBtn'
  | 'loginModal'
  | 'loginClose'
  | 'loginEmail'
  | 'loginPassword'
  | 'loginSubmit'
  | 'loginError'
  | 'loginPasskeyBtn'
  | 'loginToRegister'
  | 'registerModal'
  | 'registerClose'
  | 'registerEmail'
  | 'registerName'
  | 'registerPassword'
  | 'registerConfirm'
  | 'registerInvite'
  | 'registerSubmit'
  | 'registerError'
  | 'registerPasskeyBtn'
  | 'registerToLogin'
  | 'registrationHelp'
>;

type AuthCapabilities = Pick<
  ServerCapabilities,
  'accounts' | 'passwordLogin' | 'passkeyLogin' | 'passwordRegistration' | 'passkeyRegistration'
>;

/** Authentication actions and rendering dependencies; identity and routing stay with the host. */
export interface AuthScreenOptions {
  elements: AuthScreenElements;
  auth: Pick<
    AuthManager,
    | 'login'
    | 'register'
    | 'cancelPasskeyAttempt'
    | 'passkeyRegisterStart'
    | 'passkeyRegisterFinish'
    | 'passkeyLoginStart'
    | 'passkeyLoginFinish'
  >;
  capabilities: () => AuthCapabilities | null;
  telemetry: Pick<ClientTelemetry, 'measure'>;
  asyncAction: (action: () => Promise<void>, failureMessage: string) => () => void;
  notify: (message: string, duration?: number, kind?: 'info' | 'error') => void;
  /** Retire a matching pending registration link before its code is submitted. */
  spendRegistrationInvite: (code: string) => void;
}

/**
 * Owns the login/registration dialogs, their listeners and each authentication attempt.
 * Ceremony dismissal aborts pre-session work. Session creation stays visible until
 * its result is known; an unconfirmed cookie mutation requires reload. The host
 * retires attempts on external identity changes and disposes the screen on teardown.
 */
export class AuthScreen {
  private readonly options: AuthScreenOptions;

  private readonly flow: AuthDialogFlow;
  private readonly cleanups: (() => void)[] = [];
  private disposed = false;

  constructor(options: AuthScreenOptions) {
    this.options = options;

    const { elements, auth, asyncAction } = options;
    this.flow = new AuthDialogFlow(() => auth.cancelPasskeyAttempt());
    this.listen(elements.signInBtn, 'click', () => this.open('login'));
    this.listen(elements.loginClose, 'click', () => this.dismiss());
    this.listen(elements.registerClose, 'click', () => this.dismiss());
    this.listen(elements.loginToRegister, 'click', () => this.open('register'));
    this.listen(elements.registerToLogin, 'click', () => this.open('login'));
    for (const dialog of [elements.loginModal, elements.registerModal]) {
      this.listen(dialog, 'cancel', (event) => {
        event.preventDefault();
        this.dismiss();
      });
      this.listen(dialog, 'click', (event) => {
        if (event.target === dialog) this.dismiss();
      });
    }
    this.listen(
      elements.loginSubmit,
      'click',
      asyncAction(() => this.submitLogin(), 'Could not complete sign-in'),
    );
    this.listen(
      elements.registerSubmit,
      'click',
      asyncAction(() => this.submitRegistration(), 'Could not complete registration'),
    );
    this.listen(
      elements.loginPasskeyBtn,
      'click',
      asyncAction(() => this.submitPasskey(false), 'Could not complete passkey sign-in'),
    );
    this.listen(
      elements.registerPasskeyBtn,
      'click',
      asyncAction(() => this.submitPasskey(true), 'Could not complete passkey registration'),
    );
    this.listen(elements.loginEmail, 'keydown', (event) => {
      if (event.key === 'Enter') elements.loginPassword.focus();
    });
    this.listen(elements.loginPassword, 'keydown', (event) => {
      if (event.key === 'Enter') elements.loginSubmit.click();
    });
  }

  /** Open an advertised account flow, preserving an unresolved session mutation. */
  open(kind: 'login' | 'register'): void {
    if (this.disposed) return;
    const capabilities = this.options.capabilities();
    if (!capabilities?.accounts) return;
    if (
      kind === 'register' &&
      capabilities.passwordRegistration === 'disabled' &&
      capabilities.passkeyRegistration === 'disabled'
    ) {
      this.options.notify('Account registration is unavailable on this server.', 4000, 'error');
      return;
    }
    if (!this.dismiss()) return;
    const { elements } = this.options;
    const dialog = kind === 'login' ? elements.loginModal : elements.registerModal;
    dialog.hidden = false;
    dialog.showModal();
    (kind === 'login' ? elements.loginEmail : elements.registerEmail).focus();
  }

  /** Returns false while a request can still create an unconfirmed session cookie. */
  dismiss(): boolean {
    const { elements } = this.options;
    if (elements.loginModal.hidden && elements.registerModal.hidden) return true;
    if (!this.flow.dismiss()) return false;
    elements.loginModal.hidden = true;
    elements.registerModal.hidden = true;
    elements.loginModal.close();
    elements.registerModal.close();
    this.clearSecrets();
    elements.loginError.hidden = true;
    elements.loginError.textContent = '';
    elements.registerError.hidden = true;
    elements.registerError.textContent = '';
    this.sessionPending(false);
    elements.loginSubmit.disabled = false;
    elements.loginPasskeyBtn.disabled = false;
    elements.registerSubmit.disabled = false;
    elements.registerPasskeyBtn.disabled = false;
    elements.loginSubmit.textContent = 'Sign In';
    elements.registerSubmit.textContent = 'Create Account';
    elements.loginPasskeyBtn.textContent = 'Use a passkey';
    elements.registerPasskeyBtn.textContent = 'Register with passkey';
    return true;
  }

  /** External identity changes supersede even an unresolved cookie mutation. */
  retire(): void {
    this.flow.retire();
    this.dismiss();
  }

  /** Render only controls advertised by the current verified server contract. */
  applyCapabilities(value: AuthCapabilities, hasPendingInvite: boolean): void {
    if (this.disposed) return;
    const { elements } = this.options;
    elements.loginSubmit.hidden = !value.passwordLogin;
    this.settingGroup(elements.loginEmail).toggleAttribute('hidden', !value.passwordLogin);
    this.settingGroup(elements.loginPassword).toggleAttribute('hidden', !value.passwordLogin);
    elements.loginPasskeyBtn.hidden = !value.passkeyLogin;
    elements.loginToRegister.hidden =
      value.passwordRegistration === 'disabled' && value.passkeyRegistration === 'disabled';
    elements.registerSubmit.hidden = value.passwordRegistration === 'disabled';
    elements.registerPasskeyBtn.hidden = value.passkeyRegistration !== 'open';
    this.settingGroup(elements.registerInvite).toggleAttribute(
      'hidden',
      value.passwordRegistration !== 'invite' && !hasPendingInvite,
    );
    elements.registrationHelp.textContent =
      value.passwordRegistration === 'invite'
        ? 'An invite code lets you create a password account. You can add a passkey later when this server supports it.'
        : 'Choose a sign-in method supported by this server.';
  }

  /** Populate a link's code without redeeming it or starting registration. */
  setRegistrationInvite(code: string): void {
    if (!this.disposed) this.options.elements.registerInvite.value = code;
  }

  /** Remove listeners, clear secrets and retire pending results; safe to call twice. */
  dispose(): void {
    if (this.disposed) return;
    this.retire();
    this.disposed = true;
    for (const cleanup of this.cleanups) cleanup();
    this.cleanups.length = 0;
    this.clearSecrets();
  }

  private listen<K extends keyof HTMLElementEventMap>(
    target: HTMLElement,
    type: K,
    listener: (event: HTMLElementEventMap[K]) => void,
  ): void {
    target.addEventListener(type, listener);
    this.cleanups.push(() => target.removeEventListener(type, listener));
  }

  private settingGroup(control: HTMLElement): Element {
    const group = control.closest('.setting-group');
    if (!group)
      throw new Error(`Could not start simplestChat: #${control.id} needs a setting group.`);
    return group;
  }

  private clearSecrets(): void {
    const { elements } = this.options;
    elements.loginPassword.value = '';
    elements.registerPassword.value = '';
    elements.registerConfirm.value = '';
  }

  private sessionPending(pending: boolean): void {
    const { elements } = this.options;
    for (const control of [
      elements.loginClose,
      elements.registerClose,
      elements.loginToRegister,
      elements.registerToLogin,
    ]) {
      control.disabled = pending;
      control.setAttribute('aria-disabled', String(pending));
    }
    elements.loginModal.setAttribute('aria-busy', String(pending));
    elements.registerModal.setAttribute('aria-busy', String(pending));
  }

  private showFailure(
    attempt: AuthDialogAttempt,
    node: HTMLElement,
    error: unknown,
    message: string,
  ): void {
    if (!this.flow.current(attempt)) return;
    if (error instanceof SessionOutcomeUnknownError) {
      const { elements } = this.options;
      this.flow.markUncertain(attempt);
      this.clearSecrets();
      this.sessionPending(true);
      elements.loginModal.setAttribute('aria-busy', 'false');
      elements.registerModal.setAttribute('aria-busy', 'false');
      elements.loginSubmit.disabled = true;
      elements.loginPasskeyBtn.disabled = true;
      elements.registerSubmit.disabled = true;
      elements.registerPasskeyBtn.disabled = true;
      elements.loginSubmit.textContent = 'Response not confirmed';
      elements.registerSubmit.textContent = 'Response not confirmed';
      node.replaceChildren(
        el('p', error.message),
        button('Reload and check session', () => window.location.reload(), 'btn-primary'),
      );
    } else node.textContent = message;
    node.hidden = false;
    node.scrollIntoView({ block: 'nearest' });
  }

  private async submitLogin(): Promise<void> {
    if (this.disposed) return;
    const { elements, auth, telemetry } = this.options;
    const attempt = this.flow.begin(true);
    if (!attempt) return;
    this.sessionPending(true);
    elements.loginError.hidden = true;
    elements.loginSubmit.disabled = true;
    elements.loginPasskeyBtn.disabled = true;
    elements.loginSubmit.textContent = 'Signing in...';
    try {
      await telemetry.measure('password_login', () =>
        auth.login(elements.loginEmail.value.trim(), elements.loginPassword.value),
      );
      if (this.flow.finish(attempt)) {
        this.dismiss();
        elements.loginEmail.value = '';
      }
    } catch (error) {
      this.showFailure(
        attempt,
        elements.loginError,
        error,
        error instanceof Error ? error.message : 'Login failed',
      );
    } finally {
      if (this.flow.current(attempt) && !this.flow.isUncertain(attempt)) {
        this.flow.finish(attempt);
        this.sessionPending(false);
        this.clearSecrets();
        elements.loginSubmit.disabled = false;
        elements.loginPasskeyBtn.disabled = false;
        elements.loginSubmit.textContent = 'Sign In';
      }
    }
  }

  private async submitRegistration(): Promise<void> {
    if (this.disposed) return;
    const { elements, auth, telemetry } = this.options;
    elements.registerError.hidden = true;
    try {
      validatePassword(elements.registerPassword.value, elements.registerConfirm.value);
    } catch (error) {
      elements.registerError.textContent =
        error instanceof Error ? error.message : 'Choose a valid password';
      elements.registerError.hidden = false;
      elements.registerError.scrollIntoView({ block: 'nearest' });
      return;
    }
    const attempt = this.flow.begin(true);
    if (!attempt) return;
    this.sessionPending(true);
    elements.registerSubmit.disabled = true;
    elements.registerPasskeyBtn.disabled = true;
    elements.registerSubmit.textContent = 'Creating account...';
    try {
      this.options.spendRegistrationInvite(elements.registerInvite.value.trim());
      await telemetry.measure('password_register', () =>
        auth.register(
          elements.registerEmail.value.trim(),
          elements.registerName.value.trim(),
          elements.registerPassword.value,
          elements.registerInvite.value.trim() || undefined,
        ),
      );
      if (this.flow.finish(attempt)) {
        this.dismiss();
        elements.registerEmail.value = '';
        elements.registerName.value = '';
        elements.registerInvite.value = '';
      }
    } catch (error) {
      this.showFailure(
        attempt,
        elements.registerError,
        error,
        error instanceof Error ? error.message : 'Registration failed',
      );
    } finally {
      if (this.flow.current(attempt) && !this.flow.isUncertain(attempt)) {
        this.flow.finish(attempt);
        this.sessionPending(false);
        this.clearSecrets();
        elements.registerSubmit.disabled = false;
        elements.registerPasskeyBtn.disabled = false;
        elements.registerSubmit.textContent = 'Create Account';
      }
    }
  }

  private async submitPasskey(registration: boolean): Promise<void> {
    if (this.disposed) return;
    const { elements, auth, telemetry } = this.options;
    const email = (registration ? elements.registerEmail : elements.loginEmail).value.trim();
    const displayName = elements.registerName.value.trim();
    const errorNode = registration ? elements.registerError : elements.loginError;
    const passkeyButton = registration ? elements.registerPasskeyBtn : elements.loginPasskeyBtn;
    const passwordButton = registration ? elements.registerSubmit : elements.loginSubmit;
    if (registration && (!email || !displayName)) {
      errorNode.textContent = 'Fill in email and display name first';
      errorNode.hidden = false;
      return;
    }
    const attempt = this.flow.begin(false);
    if (!attempt) return;
    errorNode.hidden = true;
    passkeyButton.disabled = true;
    passwordButton.disabled = true;
    passkeyButton.textContent = 'Waiting for passkey...';
    let completed = false;
    try {
      let credential: Credential | null;
      if (registration) {
        const options = await telemetry.measure('passkey_register_start', () =>
          auth.passkeyRegisterStart(email, displayName, attempt.controller.signal),
        );
        if (!this.flow.current(attempt)) return;
        credential = await telemetry.measure('passkey_register_ceremony', async () => {
          const result = await navigator.credentials.create({
            ...options,
            signal: attempt.controller.signal,
          });
          if (!result) throw new DOMException('Passkey cancelled or timed out', 'NotAllowedError');
          return result;
        });
      } else {
        const options = await telemetry.measure('passkey_login_start', () =>
          auth.passkeyLoginStart(attempt.controller.signal),
        );
        if (!this.flow.current(attempt)) return;
        credential = await telemetry.measure('passkey_login_ceremony', async () => {
          const result = await navigator.credentials.get({
            ...options,
            signal: attempt.controller.signal,
          });
          if (!result) throw new DOMException('Passkey cancelled or timed out', 'NotAllowedError');
          return result;
        });
      }
      if (!this.flow.current(attempt)) return;
      if (!credential) throw new DOMException('Passkey cancelled or timed out', 'NotAllowedError');
      if (!this.flow.establish(attempt)) return;
      this.sessionPending(true);
      passkeyButton.textContent = registration ? 'Creating account...' : 'Signing in...';
      if (registration)
        await telemetry.measure('passkey_register_finish', () =>
          auth.passkeyRegisterFinish(credential),
        );
      else
        await telemetry.measure('passkey_login_finish', () => auth.passkeyLoginFinish(credential));
      if (this.flow.finish(attempt)) {
        completed = true;
        this.dismiss();
        elements.loginEmail.value = '';
        elements.registerEmail.value = '';
        elements.registerName.value = '';
      }
    } catch (error) {
      this.showFailure(
        attempt,
        errorNode,
        error,
        passkeyErrorMessage(
          error,
          registration ? 'Passkey registration failed' : 'Passkey sign-in failed',
        ),
      );
    } finally {
      if (!this.flow.isUncertain(attempt) && (this.flow.current(attempt) || completed)) {
        this.flow.finish(attempt);
        this.sessionPending(false);
        this.clearSecrets();
        passkeyButton.disabled = false;
        passwordButton.disabled = false;
        passkeyButton.textContent = registration ? 'Register with passkey' : 'Use a passkey';
      }
    }
  }
}

function passkeyErrorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error && (error.name === 'NotAllowedError' || error.name === 'AbortError'))
    return 'Passkey request cancelled or timed out. You can try again. See Passkey help if your password manager did not appear.';
  return error instanceof Error ? error.message : fallback;
}
