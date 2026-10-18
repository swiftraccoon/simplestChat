export function accountUpdatesFixture() {
  const listeners = new Set();
  const live = new Set();
  return {
    live,
    emit: (update) => {
      for (const listener of listeners) listener(update);
    },
    module: {
      onAccountUpdate: (listener) => {
        listeners.add(listener);
        return () => listeners.delete(listener);
      },
      accountUpdatesLive: (account) => live.has(account),
    },
  };
}
