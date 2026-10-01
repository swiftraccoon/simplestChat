import { deserializeCreationOptions, deserializeRequestOptions, serializeCredential } from './auth';
import type { PasskeyActionResponse, PasskeyOperation, PasskeySettings } from './api-validation';
import { api, ApiError, button, el, field, input } from './ui';

type Phase =
  | 'idle'
  | 'loading'
  | 'requesting'
  | 'authenticate'
  | 'register'
  | 'ceremony'
  | 'recovery_key'
  | 'replacement_recovery'
  | 'uncertain'
  | 'removed'
  | 'replaced';
interface ChallengeDeadline {
  id: string;
  deadline: number;
  wallDeadline: number;
}
type Challenge = ChallengeDeadline &
  (
    | { kind: 'authenticate'; options: CredentialRequestOptions }
    | { kind: 'register'; options: CredentialCreationOptions }
  );
interface SecurityOptions {
  token: () => string | null;
  current: () => boolean;
  changed: () => void;
  completed: (kind: 'removed' | 'replaced') => void;
}
const REQUEST_MS = 20_000;
const CEREMONY_MS = 60_000;
const REPLACEMENT_MS = 300_000;

/** Owns a single account operation; neither a dismissed dialog nor a late
 * browser/network completion can create a new operation or adopt its result.
 */
export class AccountSecurityFlow {
  phase: Phase = 'idle';
  message = '';
  recoveryKey = '';
  settings: PasskeySettings | null = null;
  private challenge: Challenge | null = null;
  private operation: PasskeyOperation | null = null;
  private controller: AbortController | null = null;
  private challengeTimer: ReturnType<typeof setTimeout> | null = null;
  private generation = 0;
  private disposed = false;

  constructor(private readonly options: SecurityOptions) {}

  get canDismiss(): boolean {
    return this.phase !== 'requesting' && this.phase !== 'uncertain';
  }
  get canStart(): boolean {
    return this.phase === 'idle' && !this.disposed;
  }

  async load(): Promise<void> {
    if (!this.canStart) return;
    const token = this.options.token();
    if (!token || !this.options.current()) return;
    const generation = ++this.generation;
    const controller = new AbortController();
    this.controller = controller;
    this.phase = 'loading';
    this.changed();
    try {
      const settings = await this.bounded(
        () => api.passkeySettings(token, controller.signal),
        controller,
        REQUEST_MS,
      );
      if (!this.current(generation)) return;
      this.settings = settings;
      this.message = '';
    } catch {
      if (this.current(generation))
        this.message = 'Sign-in settings could not be loaded. Close Account and try again.';
    } finally {
      if (this.controller === controller) this.controller = null;
      controller.abort();
      if (this.current(generation)) {
        this.phase = 'idle';
        this.changed();
      }
    }
  }

  async start(operation: PasskeyOperation, password?: string): Promise<void> {
    if (!this.canStart || !this.settings) return;
    if (operation.action === 'add' && this.settings.passkeys.length >= this.settings.maximum)
      return;
    if (
      (operation.action === 'remove' || operation.action === 'replace') &&
      (!this.settings.passkeys.some((key) => key.id === operation.id) ||
        (operation.action === 'remove' &&
          this.settings.passkeys.length <= 1 &&
          !this.settings.password_enabled))
    )
      return;
    this.operation = operation;
    this.recoveryKey = '';
    await this.send(
      (token, signal) =>
        api.passkeyAction(
          token,
          {
            operation,
            ...(password === undefined ? {} : { current_password: password }),
          },
          signal,
        ),
      'start',
    );
  }

  /** Called directly from a click handler after the challenge is fetched. */
  async continueWithPasskey(): Promise<void> {
    if (this.disposed || !this.options.current()) return;
    const challenge = this.challenge;
    if (!challenge || (this.phase !== 'authenticate' && this.phase !== 'register')) return;
    if (performance.now() >= challenge.deadline || Date.now() >= challenge.wallDeadline) {
      this.expireChallenge();
      return;
    }
    const generation = ++this.generation;
    const controller = new AbortController();
    this.controller = controller;
    this.clearChallengeTimer();
    this.phase = 'ceremony';
    this.message = '';
    this.changed();
    try {
      // Invoke the platform before the first await, preserving this gesture.
      const credential = await this.bounded(
        () =>
          challenge.kind === 'authenticate'
            ? navigator.credentials.get({ ...challenge.options, signal: controller.signal })
            : navigator.credentials.create({ ...challenge.options, signal: controller.signal }),
        controller,
        Math.min(challenge.deadline - performance.now(), challenge.wallDeadline - Date.now()),
      );
      if (!this.current(generation)) return;
      if (performance.now() >= challenge.deadline || Date.now() >= challenge.wallDeadline)
        throw new DOMException('Expired ceremony', 'TimeoutError');
      if (!credential) throw new DOMException('No credential returned', 'NotAllowedError');
      const serialized = serializeCredential(credential);
      this.challenge = null;
      await this.send(
        (token, signal) =>
          challenge.kind === 'authenticate'
            ? api.passkeyAuthorize(
                token,
                { ceremony_id: challenge.id, credential: serialized },
                signal,
              )
            : api.passkeyEnroll(
                token,
                { ceremony_id: challenge.id, credential: serialized },
                signal,
              ),
        challenge.kind,
      );
    } catch {
      if (this.current(generation)) {
        this.challenge = null;
        this.phase = 'idle';
        this.message =
          challenge.kind === 'register' && this.operation?.action === 'replace'
            ? 'Replacement was not submitted. Your saved recovery key can reset your password if your previous passkey is no longer available. Start again when ready.'
            : 'The passkey step was cancelled, timed out or unavailable. Start again when ready.';
        this.changed();
      }
    } finally {
      if (this.controller === controller) this.controller = null;
      controller.abort();
    }
  }

  private async send(
    request: (token: string, signal: AbortSignal) => Promise<PasskeyActionResponse>,
    step: 'start' | 'authenticate' | 'register',
  ): Promise<void> {
    await this.request(
      request,
      (result, started, wallStarted) => this.accept(result, step, started, wallStarted),
      (result) => {
        if (result.kind === 'recovery_key' || result.kind === 'replace_registration')
          result.recovery_key = '';
      },
      step === 'register' && this.operation?.action === 'replace'
        ? 'Keep your saved recovery key. After reloading, try the new passkey; if sign-in is unavailable, use the saved recovery key to reset your password.'
        : '',
    );
  }

  /** Profile, password and passkey mutations share one owner and uncertainty guard. */
  async change<T>(
    request: (token: string, signal: AbortSignal) => Promise<T>,
    confirmed: (result: T) => void,
  ): Promise<void> {
    if (!this.canStart) return;
    await this.request(request, (result) => {
      this.phase = 'idle';
      this.message = '';
      confirmed(result);
    });
  }

  private async request<T>(
    request: (token: string, signal: AbortSignal) => Promise<T>,
    accept: (result: T, started: number, wallStarted: number) => void,
    retire: (result: T) => void = () => {},
    failureHint = '',
  ): Promise<void> {
    const token = this.options.token();
    if (!token || !this.options.current() || this.disposed) return;
    const generation = ++this.generation;
    this.controller?.abort();
    const controller = new AbortController();
    this.controller = controller;
    const started = performance.now();
    const wallStarted = Date.now();
    this.phase = 'requesting';
    this.message = 'Confirming this account change…';
    this.changed();
    try {
      const result = await this.bounded(
        () =>
          request(token, controller.signal).then((value) => {
            if (
              !this.current(generation) ||
              controller.signal.aborted ||
              performance.now() - started >= REQUEST_MS ||
              Date.now() - wallStarted >= REQUEST_MS
            )
              retire(value);
            return value;
          }),
        controller,
        REQUEST_MS,
      );
      if (!this.current(generation)) {
        retire(result);
        return;
      }
      accept(result, started, wallStarted);
    } catch (error) {
      if (!this.current(generation)) return;
      this.challenge = null;
      if (
        error instanceof ApiError &&
        error.status >= 400 &&
        error.status < 500 &&
        error.status !== 408
      ) {
        this.phase = 'idle';
        this.message = error.message;
      } else {
        this.phase = 'uncertain';
        this.message =
          'This change may have completed, but its response could not be confirmed. Reload before making another account change.';
        if (!failureHint)
          this.message += ' If a recovery key was replaced, generate a new one after reloading.';
      }
      if (failureHint) this.message += ` ${failureHint}`;
    } finally {
      if (this.controller === controller) this.controller = null;
      controller.abort();
      if (this.current(generation)) this.changed();
    }
  }

  private accept(
    result: PasskeyActionResponse,
    step: 'start' | 'authenticate' | 'register',
    started: number,
    wallStarted: number,
  ): void {
    const action = this.operation?.action;
    const valid =
      (result.kind === 'authenticate' && step === 'start') ||
      (result.kind === 'register' && action === 'add' && step !== 'register') ||
      (result.kind === 'added' && action === 'add' && step === 'register') ||
      (result.kind === 'replace_registration' && action === 'replace' && step !== 'register') ||
      (result.kind === 'replaced' && action === 'replace' && step === 'register') ||
      (result.kind === 'removed' && action === 'remove' && step !== 'register') ||
      (result.kind === 'recovery_key' && action === 'recovery_key' && step !== 'register');
    if (!valid) {
      if (result.kind === 'recovery_key' || result.kind === 'replace_registration')
        result.recovery_key = '';
      throw new Error('Unexpected account action response');
    }
    if (result.kind === 'replace_registration') {
      // The backup is already persisted. Preserve it even when the creation
      // options are unusable, until it is saved or the dialog is dismissed.
      this.recoveryKey = result.recovery_key;
      result.recovery_key = '';
      if (this.settings) this.settings.recovery_enabled = true;
      this.phase = 'replacement_recovery';
      this.message = '';
      try {
        this.challenge = {
          kind: 'register',
          id: result.ceremony_id,
          deadline: started + REPLACEMENT_MS,
          wallDeadline: wallStarted + REPLACEMENT_MS,
          options: deserializeCreationOptions(result.options),
        };
        this.challengeTimer = setTimeout(
          () => this.expireChallenge(),
          Math.max(0, this.challenge.deadline - performance.now()),
        );
      } catch {
        this.challenge = null;
        this.message =
          'Replacement could not be prepared. Save this recovery key before trying again.';
      }
      return;
    }
    if (result.kind === 'authenticate' || result.kind === 'register') {
      const deadline = {
        id: result.ceremony_id,
        deadline: started + CEREMONY_MS,
        wallDeadline: wallStarted + CEREMONY_MS,
      };
      if (result.kind === 'authenticate') {
        const options = deserializeRequestOptions(result.options);
        if (options.mediation !== 'required') throw new Error('Expected explicit verification');
        this.challenge = { ...deadline, kind: 'authenticate', options };
      } else
        this.challenge = {
          ...deadline,
          kind: 'register',
          options: deserializeCreationOptions(result.options),
        };
      this.phase = result.kind;
      this.message =
        result.kind === 'authenticate'
          ? action === 'remove'
            ? 'The passkey has not been removed. Verify with an existing passkey for this account to remove it and sign out all sessions.'
            : action === 'replace'
              ? 'Verify with an existing passkey for this account before preparing its replacement. A new recovery key will replace any previous recovery key.'
              : 'Verify with an existing passkey for this account.'
          : 'Choose where to save your new passkey.';
      this.challengeTimer = setTimeout(
        () => this.expireChallenge(),
        Math.max(0, this.challenge.deadline - performance.now()),
      );
    } else if (result.kind === 'recovery_key') {
      this.recoveryKey = result.recovery_key;
      result.recovery_key = '';
      if (this.settings) this.settings.recovery_enabled = true;
      this.phase = 'recovery_key';
      this.message = '';
    } else if (result.kind === 'removed' || result.kind === 'replaced') {
      this.phase = result.kind;
      this.message =
        result.kind === 'replaced'
          ? 'Passkey replaced. Sign in again with your new passkey.'
          : 'Passkey removed. Sign in again with a remaining sign-in method.';
      try {
        this.options.completed(result.kind);
      } catch {
        /* The confirmed account change still owns this terminal outcome. */
      }
    } else {
      this.phase = 'idle';
      this.message = 'Passkey added.';
      this.load().catch(() => {
        /* Load reports its own bounded failure. */
      });
    }
  }

  private async bounded<T>(
    work: () => Promise<T>,
    controller: AbortController,
    milliseconds: number,
  ): Promise<T> {
    const deadline = performance.now() + milliseconds;
    const wallDeadline = Date.now() + milliseconds;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let aborted: (() => void) | undefined;
    try {
      const abortedWork = new Promise<never>((_resolve, reject) => {
        aborted = () => reject(new DOMException('Account operation retired', 'AbortError'));
        controller.signal.addEventListener('abort', aborted, { once: true });
        timer = setTimeout(() => controller.abort(), Math.max(0, milliseconds));
      });
      if (controller.signal.aborted)
        throw new DOMException('Retired account operation', 'AbortError');
      const result = await Promise.race([work(), abortedWork]);
      if (controller.signal.aborted || performance.now() >= deadline || Date.now() >= wallDeadline)
        throw new DOMException('Account operation timed out', 'TimeoutError');
      return result;
    } finally {
      clearTimeout(timer);
      if (aborted) controller.signal.removeEventListener('abort', aborted);
    }
  }

  private current(generation: number): boolean {
    return !this.disposed && generation === this.generation && this.options.current();
  }
  private changed(): void {
    try {
      this.options.changed();
    } catch {
      /* A rendering failure cannot bypass lifecycle guards. */
    }
  }
  private clearChallengeTimer(): void {
    if (this.challengeTimer !== null) clearTimeout(this.challengeTimer);
    this.challengeTimer = null;
  }
  private expireChallenge(): void {
    if (this.disposed || !this.challenge) return;
    this.clearChallengeTimer();
    this.challenge = null;
    if (this.phase === 'replacement_recovery') {
      this.message =
        'The replacement request expired. Save this recovery key before starting again.';
      this.changed();
      return;
    }
    this.phase = 'idle';
    this.message = 'The passkey request expired. Start again when ready.';
    this.changed();
  }
  dismissRecoveryKey(): void {
    if (this.phase !== 'recovery_key') return;
    this.recoveryKey = '';
    this.phase = 'idle';
    this.changed();
  }
  acknowledgeReplacementRecovery(): void {
    if (this.phase !== 'replacement_recovery' || this.disposed || !this.options.current()) return;
    const challenge = this.challenge;
    if (
      challenge &&
      (performance.now() >= challenge.deadline || Date.now() >= challenge.wallDeadline)
    )
      this.expireChallenge();
    this.recoveryKey = '';
    this.phase = this.challenge ? 'register' : 'idle';
    this.message = this.challenge
      ? 'Create your replacement passkey now. The previous passkey is removed from this account only after the new one is confirmed. All sessions will then be signed out.'
      : 'Your saved recovery key can reset your password. Start replacement again when ready.';
    this.changed();
  }
  /** Retire an unused proof challenge; no account mutation has been submitted. */
  cancelVerification(): void {
    if (this.phase !== 'authenticate') return;
    this.generation++;
    this.clearChallengeTimer();
    this.challenge = null;
    this.operation = null;
    this.phase = 'idle';
    this.message = '';
    this.changed();
  }
  dispose(): void {
    this.disposed = true;
    this.generation++;
    this.controller?.abort();
    this.controller = null;
    this.clearChallengeTimer();
    this.challenge = null;
    this.operation = null;
    this.recoveryKey = '';
  }
}

export function mountAccountSecurity(options: {
  container: HTMLElement;
  dialog: HTMLDialogElement;
  token: () => string | null;
  current: () => boolean;
  completed: (kind: 'removed' | 'replaced') => void;
  interactionChanged?: (canStart: boolean) => void;
}): AccountSecurityFlow {
  const section = el('section');
  section.setAttribute('aria-label', 'Sign-in and recovery');
  options.container.append(section);
  let passwordInput: HTMLInputElement | null = null;
  let recoveryInput: HTMLTextAreaElement | null = null;
  let keyAction: { action: 'remove' | 'replace'; id: string; description: string } | null = null;
  const run = (work: Promise<void>): void => {
    work.catch(() => {
      /* Flow owns safe errors. */
    });
  };
  const render = (): void => {
    if (!options.current()) return;
    options.interactionChanged?.(flow.canStart);
    if (passwordInput) passwordInput.value = '';
    if (recoveryInput) recoveryInput.value = '';
    section.replaceChildren(
      el(
        'h3',
        keyAction
          ? keyAction.action === 'replace'
            ? 'Replace passkey'
            : 'Remove passkey'
          : 'Sign-in and recovery',
      ),
    );
    if (flow.message) section.append(el('p', flow.message, 'setting-hint'));
    if (flow.phase === 'uncertain') {
      section.append(
        button('Reload to check account', () => window.location.reload(), 'btn-primary'),
      );
      return;
    }
    if (flow.phase === 'authenticate' || flow.phase === 'register') {
      section.append(
        button(
          flow.phase === 'authenticate'
            ? keyAction
              ? keyAction.action === 'replace'
                ? 'Verify replacement with passkey'
                : 'Verify and remove passkey'
              : 'Verify with passkey'
            : keyAction?.action === 'replace'
              ? 'Create replacement passkey'
              : 'Create passkey',
          () => run(flow.continueWithPasskey()),
          'btn-primary',
        ),
      );
      if (keyAction && flow.phase === 'authenticate')
        section.append(
          button(
            keyAction.action === 'replace'
              ? 'Back to replacement options'
              : 'Back to removal options',
            () => {
              flow.cancelVerification();
              section.querySelector('select')?.focus();
            },
          ),
        );
      return;
    }
    if (
      flow.phase === 'requesting' ||
      flow.phase === 'ceremony' ||
      flow.phase === 'removed' ||
      flow.phase === 'replaced' ||
      flow.phase === 'loading'
    ) {
      section.append(
        el(
          'p',
          flow.phase === 'ceremony'
            ? 'Complete the browser passkey prompt, or close Account to cancel this step.'
            : 'Please wait…',
        ),
      );
      return;
    }
    if (flow.phase === 'recovery_key' || flow.phase === 'replacement_recovery') {
      section.append(
        el(
          'p',
          flow.phase === 'replacement_recovery'
            ? 'Save this one-time recovery key before creating the replacement passkey. Your password manager may overwrite the previous passkey before the site confirms the new one. This key can reset your password if replacement fails. Any previous recovery key no longer works. No email is sent.'
            : 'Save this one-time recovery key in your password manager. It is shown only now and can reset your password once. Any previous recovery key no longer works. No email is sent.',
        ),
      );
      const key = el('textarea');
      recoveryInput = key;
      key.value = flow.recoveryKey;
      key.readOnly = true;
      key.rows = 3;
      const copied = el('p', '', 'setting-hint');
      const copy = async (): Promise<void> => {
        try {
          await navigator.clipboard.writeText(key.value);
          if (
            options.current() &&
            (flow.phase === 'recovery_key' || flow.phase === 'replacement_recovery')
          )
            copied.textContent = 'Copied';
        } catch {
          if (options.current())
            copied.textContent = 'Copy failed. Select and save the key manually.';
        }
      };
      section.append(
        field('Recovery key', key),
        button('Copy recovery key', () => run(copy())),
        copied,
        button('I saved my recovery key', () =>
          flow.phase === 'replacement_recovery'
            ? flow.acknowledgeReplacementRecovery()
            : flow.dismissRecoveryKey(),
        ),
      );
      return;
    }
    const settings = flow.settings;
    if (!settings) {
      section.append(el('p', 'Sign-in settings are unavailable.'));
      return;
    }
    section.append(
      el(
        'p',
        keyAction
          ? keyAction.action === 'replace'
            ? `Replace the key added ${keyAction.description}? Verify your identity before replacement. Save the new recovery key before creating a replacement; it will replace any previous recovery key. The previous passkey stays on this account until the new one is confirmed, then all sessions are signed out.`
            : `Remove the key added ${keyAction.description}? Verify your identity before removal. After removal, all sessions will be signed out. Make sure you can use another sign-in method.`
          : `${settings.passkeys.length} of ${settings.maximum} passkeys saved. ${settings.password_enabled ? 'Password sign-in is also available.' : 'This account signs in with passkeys.'}`,
        'setting-hint',
      ),
    );
    const proof = el('select');
    if (settings.passkeys.length > 0) {
      const option = el('option', 'Existing passkey');
      option.value = 'passkey';
      proof.append(option);
    }
    if (settings.password_enabled) {
      const option = el('option', 'Current password');
      option.value = 'password';
      proof.append(option);
    }
    proof.value =
      keyAction && settings.password_enabled
        ? 'password'
        : settings.passkeys.length > 0
          ? 'passkey'
          : 'password';
    const password = input('', 'password', 128);
    passwordInput = password;
    password.autocomplete = 'current-password';
    const passwordField = field('Current password for verification', password);
    passwordField.hidden = proof.value !== 'password';
    proof.addEventListener('change', () => {
      password.value = '';
      passwordField.hidden = proof.value !== 'password';
    });
    section.append(field('Verify account changes with', proof), passwordField);
    const choose = (operation: PasskeyOperation): void => {
      if (proof.value === 'password' && !password.value) {
        flow.message = 'Enter your current password to verify this change.';
        render();
        if (keyAction) passwordInput?.focus();
        return;
      }
      const secret = proof.value === 'password' ? password.value : undefined;
      password.value = '';
      run(flow.start(operation, secret));
    };
    if (keyAction) {
      const { action, id } = keyAction;
      section.append(
        button(
          action === 'replace' ? 'Verify before replacement' : 'Verify and remove passkey',
          () => choose({ action, id }),
          action === 'replace' ? 'btn-primary' : 'btn-secondary danger',
        ),
        button('Keep passkey', () => {
          keyAction = null;
          flow.message = '';
          render();
          section.querySelector('select')?.focus();
        }),
      );
      return;
    }
    for (const key of settings.passkeys) {
      const description = `${new Date(key.created_at).toLocaleString()} · ${key.id.slice(0, 8)}`;
      const row = el('div', undefined, 'community-field');
      const replace = button(`Replace passkey ${key.id.slice(0, 8)}`, () => {
        keyAction = { action: 'replace', id: key.id, description };
        flow.message = '';
        render();
        section.querySelector('select')?.focus();
      });
      const remove = button(`Remove passkey ${key.id.slice(0, 8)}`, () => {
        keyAction = { action: 'remove', id: key.id, description };
        flow.message = '';
        render();
        section.querySelector('select')?.focus();
      });
      remove.disabled = settings.passkeys.length <= 1 && !settings.password_enabled;
      row.append(el('span', description), replace, remove);
      section.append(row);
    }
    if (settings.passkeys.length <= 1 && !settings.password_enabled)
      section.append(
        el(
          'p',
          'Add another passkey before removing your last one. A recovery key alone does not enable removal.',
          'setting-hint',
        ),
      );
    const add = button('Add passkey', () => choose({ action: 'add' }), 'btn-primary');
    add.disabled = settings.passkeys.length >= settings.maximum;
    const recovery = button(
      settings.recovery_enabled ? 'Replace recovery key' : 'Generate recovery key',
      () => {
        if (settings.recovery_enabled)
          section.replaceChildren(
            el('h3', 'Replace recovery key'),
            el(
              'p',
              'Your saved recovery key will stop working. Save the new key immediately after verification.',
            ),
            button('Confirm replacement', () => choose({ action: 'recovery_key' }), 'btn-primary'),
            button('Keep current key', render),
          );
        else choose({ action: 'recovery_key' });
      },
    );
    section.append(add, recovery);
  };
  const flow = new AccountSecurityFlow({
    token: options.token,
    current: options.current,
    changed: render,
    completed: options.completed,
  });
  options.dialog.addEventListener(
    'close',
    () => {
      if (passwordInput) passwordInput.value = '';
      if (recoveryInput) recoveryInput.value = '';
      flow.dispose();
    },
    { once: true },
  );
  render();
  return flow;
}
