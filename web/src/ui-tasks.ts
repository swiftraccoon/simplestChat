interface UiOwner {
  room: object | null;
  accountId: string | null;
}
interface TaskOptions {
  owner: () => UiOwner;
  notify: (message: string, duration: number, kind: 'error') => void;
  report?: (message: string) => void;
  signal?: AbortSignal;
}

/** Observe detached work while keeping failures scoped to the initiating identity and room. */
export function createUiTasks(options: TaskOptions) {
  function observeUiTask(task: Promise<unknown>, failureMessage: string): void {
    const expected = options.owner();
    task.catch(() => {
      (options.report ?? console.error)(failureMessage);
      const current = options.owner();
      if (
        !options.signal?.aborted &&
        current.room === expected.room &&
        current.accountId === expected.accountId
      )
        options.notify(failureMessage, 8000, 'error');
    });
  }
  return {
    observeUiTask,
    asyncUiAction: (action: () => Promise<unknown>, message: string) =>
      asyncUiAction(action, message, observeUiTask),
  };
}

/** Run during the original device gesture while observing synchronous and asynchronous failures. */
export function asyncUiAction(
  action: () => Promise<unknown>,
  failureMessage: string,
  observe: (task: Promise<unknown>, failureMessage: string) => void,
): () => void {
  return () => {
    observe((async () => action())(), failureMessage);
  };
}
