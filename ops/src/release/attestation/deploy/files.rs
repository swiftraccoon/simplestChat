//! Exact private deployment records and source-bound lower I/O fault fixtures.

#[cfg(test)]
thread_local! {
    static CLOSES: std::cell::Cell<(usize,usize)> = const { std::cell::Cell::new((0,0)) };
    static FAIL_CLOSE: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static DIGEST_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
pub(super) fn start_probe() {
    CLOSES.set((0, 0));
    FAIL_CLOSE.set(None);
    DIGEST_READS.set(0);
    super::super::stage::start_records_probe();
}
#[cfg(test)]
pub(super) fn take_probe() -> (usize, usize) {
    let own = CLOSES.replace((0, 0));
    let stage = super::super::stage::take_records_probe();
    (own.0 + stage.0, own.1 + stage.1)
}
#[cfg(test)]
pub(super) fn closed() -> bool {
    CLOSES.with(|value| {
        let (closed, fallback) = value.get();
        value.set((closed + 1, fallback));
        FAIL_CLOSE.get() == Some(closed + 1)
    })
}
#[cfg(test)]
pub(super) fn fail_close(at: usize) {
    FAIL_CLOSE.set(Some(at));
}
#[cfg(test)]
pub(super) fn checked_closes() -> usize {
    CLOSES.get().0
}
#[cfg(test)]
pub(super) fn digest_read() {
    DIGEST_READS.set(DIGEST_READS.get() + 1);
}
#[cfg(test)]
pub(super) fn digest_reads() -> u64 {
    DIGEST_READS.get()
}
#[cfg(test)]
pub(super) fn fallback() {
    CLOSES.with(|value| {
        let (closed, fallback) = value.get();
        value.set((closed, fallback + 1));
    });
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    BackupFileSync,
    BackupRename,
    BackupDirectorySync,
    BackupReceiptWrite,
    BeforeCopy,
    ReplaceJournalWrite,
    SelectComposeWrite,
    SelectEnvWrite,
    SelectImagesWrite,
    SelectAssets,
    RollbackComposeWrite,
    RollbackAssets,
    CompleteJournalWrite,
    OutcomeWrite,
    OutcomeFileSync,
    OutcomeDirectorySync,
    InterruptBeforeStop,
    InterruptAfterStop,
    ReplacementObservationTime,
}
