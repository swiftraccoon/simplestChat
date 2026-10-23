//! Native observations and forwarding through the existing single signal owner.
use super::{
    failure,
    io::{Endpoint, Observations, Process},
    namespace::{Lease, Roots},
    plan, sampling,
    tools::Tools,
};
use crate::{
    ci::{
        command::{Command, Program},
        process::{self, Executor},
    },
    tooling::Result,
};
use std::{
    collections::BTreeMap,
    fs::File,
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) struct Host<'a> {
    tools: &'a Tools,
    roots: Roots,
}
impl<'a> Host<'a> {
    pub fn new(tools: &'a Tools) -> Self {
        Self {
            tools,
            roots: Roots::native(),
        }
    }
}
impl Observations for Host<'_> {
    type Namespace = Lease;
    type Groups = sampling::Groups;
    fn platform(&self) -> Result<()> {
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(failure(2));
        }
        #[cfg(not(target_os = "linux"))]
        return Err(failure(2));
        #[cfg(target_os = "linux")]
        {
            use super::namespace;
            // This checks the controller's host profile, not that an arbitrary
            // Unix-socket daemon necessarily shares this host kernel.
            let budget = self.tools.budget();
            let _permit = budget.reserve(namespace::count(&self.roots.cgroup)? + 1)?;
            let root = crate::ci::release_controller::files::Directory::open(&self.roots.cgroup)
                .map_err(|_| failure(2))?;
            let observed = namespace::required(root.file(), "cgroup.controllers", 64 * 1024);
            let closed = root.finish().map_err(|_| failure(1));
            let bytes = observed?;
            closed?;
            let text = std::str::from_utf8(&bytes).map_err(|_| failure(2))?;
            let fields = text.split_ascii_whitespace().collect::<Vec<_>>();
            if !fields.contains(&"cpu") || !fields.contains(&"memory") {
                return Err(failure(2));
            }
            Ok(())
        }
    }
    fn snapshot(&self) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut rows = sampling::snapshot(&self.roots, &self.tools.budget())?;
        let uname = rustix::system::uname();
        let text = format!(
            "{} {} {} {} {} GNU/Linux\n",
            uname.sysname().to_string_lossy(),
            uname.nodename().to_string_lossy(),
            uname.release().to_string_lossy(),
            uname.version().to_string_lossy(),
            uname.machine().to_string_lossy()
        );
        if text.len() > 4096 {
            return Err(failure(1));
        }
        rows.insert("kernel.txt".into(), text.into_bytes());
        Ok(rows)
    }
    fn namespace(&self, process: &Process) -> Result<Arc<Lease>> {
        Ok(Arc::new(Lease::open(
            &self.roots,
            process,
            &self.tools.budget(),
        )?))
    }
    fn groups(&self, processes: &[Process; 2]) -> Result<sampling::Groups> {
        sampling::Groups::new(&self.roots, processes, &self.tools.budget())
    }
    fn curl(
        &self,
        namespace: &Arc<Lease>,
        endpoint: Endpoint,
        input: File,
        timeout: Duration,
        cleanup: bool,
    ) -> Result<Command> {
        let result = (|| {
            if timeout.is_zero() || timeout > Duration::from_secs(4) {
                return Err(failure(1));
            }
            let mut command = Command::capture(Program::BenchmarkCurl, &[]);
            command.arguments = self.tools.curl_arguments(namespace, endpoint)?;
            command.capture = Some(plan::CAPTURE);
            command.timeout = timeout;
            command.grace = Duration::from_secs(1);
            command.cleanup = cleanup;
            let proof = self.tools.curl_invocation(&command, namespace.clone())?;
            command.benchmark = Some(proof);
            Ok(command)
        })();
        match result {
            Ok(mut command) => {
                command.stdin = Some(Arc::new(input));
                Ok(command)
            }
            Err(error) => {
                let _ = nix::unistd::close(input);
                Err(error)
            }
        }
    }
}
pub(super) struct Adapter<'a, E> {
    inner: &'a mut E,
    tools: &'a Tools,
}
impl<'a, E> Adapter<'a, E> {
    pub fn new(inner: &'a mut E, tools: &'a Tools) -> Self {
        Self { inner, tools }
    }
}
fn conflicting(command: &Command) -> bool {
    command.executable.is_some()
        || command.node.is_some()
        || command.dtls.is_some()
        || command.ops.is_some()
        || command.source_git.is_some()
        || command.comparison.is_some()
        || command.zstd.is_some()
        || command.load_image.is_some()
        || command.mdns.is_some()
        || command.retained_ops.is_some()
}
impl<E: Executor> Executor for Adapter<'_, E> {
    async fn run(&mut self, mut request: Command) -> Result<Vec<u8>> {
        let admitted = (|| {
            if conflicting(&request) {
                return Err(failure(1));
            }
            match request.program {
                Program::Docker => {
                    if request.benchmark.is_some() {
                        return Err(failure(1));
                    }
                    request.benchmark = Some(self.tools.invocation(&request)?);
                }
                Program::BenchmarkCurl => {
                    if !request
                        .benchmark
                        .as_ref()
                        .is_some_and(|p| p.belongs(self.tools))
                    {
                        return Err(failure(1));
                    }
                }
                _ => return Err(failure(1)),
            }
            Ok(())
        })();
        if let Err(error) = admitted {
            let _ = process::settle_unforwarded(request).await;
            return Err(error);
        }
        let proof = request.benchmark.as_ref().unwrap().clone();
        let program = request.program;
        let args = request.arguments.clone();
        let result = self.inner.run(request).await;
        // Native has already retired the reserved CLI group and finished both
        // owned log peers and stdin before this post-call namespace/tool check.
        let checked = proof.validate_settled(program, &args);
        match result {
            Ok(bytes) => {
                checked?;
                Ok(bytes)
            }
            Err(error) => Err(error),
        }
    }
    async fn ready(&mut self) -> Result<()> {
        self.inner.ready().await
    }
    async fn replay(&mut self, bytes: &[u8], stderr: bool, deadline: Instant) -> Result<()> {
        self.inner.replay(bytes, stderr, deadline).await
    }
    fn cleanup_passed(&self) -> bool {
        self.inner.cleanup_passed()
    }
}
