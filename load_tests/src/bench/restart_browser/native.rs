//! One signal/cancellation owner remains live through all finalizers and closes.
use super::{
    config::{Config, InvalidArguments},
    controller::{self, PortSource, Readiness, Services},
};
use crate::bench::{
    clock::{Clock, RealClock},
    process::NativeHost,
    smoke::{config::Ports, native::reserve_guest_ports, wire::NativeWire},
};
use anyhow::Result;
use std::{ffi::OsString, sync::Arc};

struct PortsOwner;
impl PortSource for PortsOwner {
    fn reserve(&self) -> Result<Ports> {
        reserve_guest_ports()
    }
}
#[async_trait::async_trait]
impl Readiness for NativeWire {
    async fn ready(&self, ports: Ports) -> Result<bool> {
        self.request_ready(ports).await
    }
}
pub(super) fn run(arguments: &[OsString]) -> u8 {
    let environment = std::env::vars_os().collect();
    let result: Result<u8> = (|| {
        let root = std::env::current_dir()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let host = Arc::new(NativeHost::default());
        runtime.block_on(async {
            use tokio::signal::unix::{SignalKind, signal};
            let mut interrupt = signal(SignalKind::interrupt())?;
            let mut terminate = signal(SignalKind::terminate())?;
            let clock = RealClock::default();
            let operation = async {
                let config = Config::parse(arguments, root, environment)?;
                let wire = match NativeWire::new() {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = config.finish();
                        return Err(error);
                    }
                };
                let services = Services {
                    host: host.as_ref(),
                    clock: &clock,
                    ports: &PortsOwner,
                    readiness: &wire,
                };
                controller::execute(config, &services).await.map(|v| {
                    drop(v.report);
                    v.code
                })
            };
            tokio::pin!(operation);
            let result = loop {
                tokio::select! {biased;_=interrupt.recv()=>host.cancel(130),
                _=terminate.recv()=>host.cancel(143),result=&mut operation=>break result}
            };
            tokio::select! {biased;_=interrupt.recv()=>host.cancel(130),
            _=terminate.recv()=>host.cancel(143),()=std::future::ready(())=>{}}
            let mut code = result?;
            if code == 0 && host.code() != 0 {
                code = host.code();
            }
            if code == 0 && clock.elapsed_ms() >= super::config::FINAL {
                code = 124;
            }
            Ok(code)
        })
    })();
    result.unwrap_or_else(|error| {
        if error.is::<InvalidArguments>() {
            2
        } else {
            controller::status(&error)
        }
    })
}
