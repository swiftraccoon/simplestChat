export interface AuthDialogAttempt {
  readonly controller: AbortController;
}

/** A server may set a session cookie even if its fetch is aborted. */
export class AuthDialogFlow {
  private attempt: AuthDialogAttempt | null = null;
  private creatingSession = false;
  private uncertain = false;

  constructor(private readonly retireCeremony: () => void) {}

  begin(session: boolean): AuthDialogAttempt | null {
    if (this.attempt) return null;
    this.creatingSession = session;
    this.attempt = { controller: new AbortController() };
    return this.attempt;
  }

  current(attempt: AuthDialogAttempt): boolean {
    return this.attempt === attempt && !attempt.controller.signal.aborted;
  }

  establish(attempt: AuthDialogAttempt): boolean {
    if (!this.current(attempt)) return false;
    this.creatingSession = true;
    return true;
  }

  markUncertain(attempt: AuthDialogAttempt): void {
    if (this.current(attempt)) {
      this.uncertain = true;
      this.creatingSession = true;
    }
  }

  isUncertain(attempt: AuthDialogAttempt): boolean {
    return this.current(attempt) && this.uncertain;
  }

  finish(attempt: AuthDialogAttempt): boolean {
    if (!this.current(attempt) || this.uncertain) return false;
    this.attempt = null;
    this.creatingSession = false;
    return true;
  }

  /** An external account change supersedes even an unresolved cookie mutation. */
  retire(): void {
    const attempt = this.attempt;
    this.attempt = null;
    this.creatingSession = false;
    this.uncertain = false;
    if (attempt) {
      attempt.controller.abort();
      this.retireCeremony();
    }
  }

  dismiss(): boolean {
    // Remain visible until the session result is known. Cancelling an HTTP
    // request would not reliably prevent its HttpOnly cookie being installed.
    if (this.creatingSession) return false;
    if (this.attempt) {
      this.attempt.controller.abort();
      this.attempt = null;
      this.retireCeremony();
    }
    return true;
  }
}
