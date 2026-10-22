//! Native adapters construct no service until the explicit smoke command runs.

use super::{
    config::{self, Config, Mode, Ports},
    controller::{self, PortSource, Services},
    wire::NativeWire,
};
use crate::bench::{
    clock::{Clock, RealClock},
    process::NativeHost,
};
use anyhow::Result;
use std::{ffi::OsString, sync::Arc};

struct NativePorts;
#[async_trait::async_trait]
impl PortSource for NativePorts {
    fn reserve(&self) -> Result<Ports> {
        let http = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let media = match std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0)) {
            Ok(value) => value,
            Err(error) => {
                let _ = nix::unistd::close(http);
                return Err(error.into());
            }
        };
        let ports = (|| {
            Ok::<_, anyhow::Error>(Ports {
                http: http.local_addr()?.port(),
                media: media.local_addr()?.port(),
            })
        })();
        // Admission observations only; final binding still belongs to the child.
        let a = nix::unistd::close(http);
        let b = nix::unistd::close(media);
        let ports = ports?;
        a?;
        b?;
        ports.validate()
    }
}
pub(super) fn run(mode: Mode, arguments: &[OsString]) -> u8 {
    let environment = std::env::vars_os().collect();
    let result = (|| -> Result<u8> {
        let directory = std::env::current_dir()?;
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
                let config = Config::parse(mode, arguments, directory, environment)
                    .map_err(|_| controller::Failure(2))?;
                let wire = NativeWire::new()?;
                let services = Services {
                    host: host.as_ref(),
                    clock: &clock,
                    wire: &wire,
                    ports: &NativePorts,
                };
                let outcome = controller::execute(&config, &services).await?;
                Ok::<_, anyhow::Error>(outcome.code)
            };
            tokio::pin!(operation);
            let result = loop {
                tokio::select! {biased;
                    _=interrupt.recv()=>host.cancel(130),
                    _=terminate.recv()=>host.cancel(143),
                    result=&mut operation=>break result,
                }
            };
            tokio::select! {biased;_=interrupt.recv()=>host.cancel(130),
            _=terminate.recv()=>host.cancel(143),()=std::future::ready(())=>{}}
            let mut code = result?;
            if code == 0 && host.code() != 0 {
                code = host.code()
            }
            if code == 0 && clock.elapsed_ms() >= config::FINAL_MS {
                code = 124
            }
            Ok(code)
        })
    })();
    match result {
        Ok(code) => code,
        Err(error) => controller::status(&error),
    }
}
