//! Helpers shared by the repository's command-line tools: child commands with
//! an explicit environment and the pinned native build settings (`tooling`),
//! owned process groups and signals over the Ops implementation
//! (`owned_process`), retained executable identities (`executable`), the
//! reserved output-replay child that every runner binary routes first
//! (`replay`), the owned disposable PostgreSQL and loopback server test
//! environment (`test_env`), and the default route's IPv4 address for test
//! media announcements (`media_ip`).

pub mod executable;
pub mod media_ip;
pub mod owned_process;
pub mod replay;
pub mod test_env;
pub mod tooling;

#[cfg(test)]
mod tests {
    #[test]
    fn public_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<crate::tooling::Failure>();
        assert_send_sync::<crate::tooling::NativeInputs>();
        assert_send_sync::<crate::owned_process::Group>();
        assert_send_sync::<crate::owned_process::Signals>();
        assert_send_sync::<crate::executable::Executable>();
        assert_send_sync::<crate::replay::Finalize>();
        assert_send_sync::<crate::replay::ReplayOutcome>();
        assert_send_sync::<crate::test_env::Stop>();
        assert_send_sync::<crate::media_ip::NativeDetector<'static>>();
    }
}
