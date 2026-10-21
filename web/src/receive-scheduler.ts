type ReceiveJob = {
  kind: 'audio' | 'video';
  run: () => Promise<void>;
  finish: (error?: unknown) => void;
};

/** Browser negotiation is serial, but waiting voice precedes waiting video.
 * Retiring a receiver releases waiters without awaiting its poisoned native queue. */
export class ReceiveScheduler {
  private jobs: ReceiveJob[] = [];
  private active: ReceiveJob | null = null;
  private epoch = 0;
  private scheduled = false;
  private waiters: (() => void)[] = [];

  enqueue(kind: 'audio' | 'video', run: () => Promise<void>): Promise<void> {
    if (this.jobs.length >= 256)
      return Promise.reject(new Error('Too many incoming media requests'));
    const result = new Promise<void>((resolve, reject) => {
      this.jobs.push({
        kind,
        run,
        finish: (error) =>
          error === undefined
            ? resolve()
            : reject(error instanceof Error ? error : new Error('Incoming media setup failed')),
      });
    });
    if (!this.active && !this.scheduled) {
      this.scheduled = true;
      const epoch = this.epoch;
      queueMicrotask(() => {
        if (epoch !== this.epoch) return;
        this.scheduled = false;
        this.drain(epoch).catch(() => {});
      });
    }
    return result;
  }

  idle(): Promise<void> {
    if (!this.active && !this.jobs.length) return Promise.resolve();
    return new Promise((resolve) => this.waiters.push(resolve));
  }

  reset(): void {
    this.epoch++;
    this.scheduled = false;
    this.active?.finish();
    this.active = null;
    for (const job of this.jobs.splice(0)) job.finish();
    for (const resolve of this.waiters.splice(0)) resolve();
  }

  private async drain(epoch: number): Promise<void> {
    while (epoch === this.epoch && this.jobs.length) {
      const voice = this.jobs.findIndex((job) => job.kind === 'audio');
      const job = this.jobs.splice(voice < 0 ? 0 : voice, 1)[0]!;
      this.active = job;
      try {
        await job.run();
        job.finish();
      } catch (error) {
        job.finish(error);
      }
      if (epoch !== this.epoch) return;
      this.active = null;
    }
    if (epoch === this.epoch) for (const resolve of this.waiters.splice(0)) resolve();
  }
}
