/** A mounted view or asynchronous owner that can be retired exactly once. */
export interface Disposable {
  dispose(): void;
}

/** Listeners use the signal; other resources register cleanup with their owner. */
export interface Scope extends Disposable {
  readonly signal: AbortSignal;
  onDispose(cleanup: () => void): void;
}

/** Abort listeners first, then release registered resources in reverse order. */
export function createScope(): Scope {
  const controller = new AbortController();
  const cleanups: (() => void)[] = [];
  return {
    signal: controller.signal,
    onDispose(cleanup) {
      if (controller.signal.aborted) cleanup();
      else cleanups.push(cleanup);
    },
    dispose() {
      if (controller.signal.aborted) return;
      controller.abort();
      let failed = false;
      let failure: unknown;
      for (const cleanup of cleanups.reverse()) {
        try {
          cleanup();
        } catch (error) {
          if (!failed) failure = error;
          failed = true;
        }
      }
      cleanups.length = 0;
      if (failed) throw failure;
    },
  };
}

/** Mount one owned element; disposal removes only its node and resources. */
export function mount(parent: HTMLElement, render: (scope: Scope) => HTMLElement): Scope {
  const scope = createScope();
  try {
    const element = render(scope);
    parent.append(element);
    scope.onDispose(() => element.remove());
    return scope;
  } catch (error) {
    scope.dispose();
    throw error;
  }
}
