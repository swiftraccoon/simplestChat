import type { AuthManager } from './auth';

/** Browser-local preferences remain usable when storage is blocked or full. */
export function createLocalPreferences(storage: () => Pick<Storage, 'getItem' | 'setItem'>) {
  const memory = new Map<string, string>();
  return {
    readLocalPreference(key: string): string | null {
      if (memory.has(key)) return memory.get(key) ?? null;
      try {
        return storage().getItem(key);
      } catch {
        return null;
      }
    },
    writeLocalPreference(key: string, value: string): void {
      memory.set(key, value);
      try {
        storage().setItem(key, value);
      } catch {
        /* Keep the value for this tab. */
      }
    },
    clear(): void {
      memory.clear();
    },
  };
}

/** Clears app preferences while preserving the credential-free cross-tab change marker. */
export function forgetThisDevice(storage: Pick<Storage, 'length' | 'key' | 'removeItem'>): void {
  const keys: string[] = [];
  for (let index = 0; index < storage.length; index++) {
    const key = storage.key(index);
    if (key !== null) keys.push(key);
  }
  for (const key of keys)
    if (
      key.startsWith('simplestchat.') ||
      ['displayName', 'micMode', 'layout', 'panelPreferences', 'reliabilityTelemetry'].includes(key)
    )
      storage.removeItem(key);
}

interface SignOutContext {
  readonly auth: Pick<AuthManager, 'logout' | 'isLoggedIn'>;
  readonly nameInput: Pick<HTMLInputElement, 'value'>;
  accountEpoch: () => number;
  clearPreferences: () => void;
  forgetDevice: () => void;
  reload: () => void;
}

/** Revoke sign-in before clearing this device; a replacement identity retires cleanup. */
export async function signOutAndForget(context: SignOutContext): Promise<void> {
  const epoch = context.accountEpoch();
  await context.auth.logout();
  if (context.auth.isLoggedIn || context.accountEpoch() !== epoch + 1) return;
  context.clearPreferences();
  context.nameInput.value = '';
  try {
    context.forgetDevice();
  } catch {
    throw new Error(
      'Signed out, but this browser prevented clearing saved app data. Clear site data in your browser settings.',
    );
  }
  context.reload();
}
