//! Actual owned pipe/group checks with inert executables, never SSH or Ansible.

use super::*;
use crate::{ci::ops_artifact::capacity_vps::tests::PreparationFixture, owned_process::Signals};

const LEASE: &str = r#"
def exact(n):
    data=b''
    while len(data)<n:
        chunk=os.read(0,n-len(data))
        if not chunk: raise RuntimeError('short frame')
        data+=chunk
    return data
line=b''
while not line.endswith(b'\n'): line+=exact(1)
header=json.loads(line)
request=exact(header['request']['bytes'])
with open(ROOT+'/frame-count','w') as out: out.write(str(len(line)+len(request)))
os.write(1,READY)
release=exact(8)
assert release==b'release\n' and os.read(0,1)==b''
assert os.path.exists(ROOT+'/ansible-finished')
with open(ROOT+'/released','w') as out: out.write('released')
"#;

#[tokio::test]
async fn completed_preparation_closes_both_groups_and_streams_before_release() {
    let mut fixture = PreparationFixture::new(
        LEASE,
        r#"
os.write(1,b'private ansible output\n')
os.write(2,b'private ansible error\n')
with open(ROOT+'/ansible-finished','w') as out: out.write('done')
"#,
        240,
    )
    .await;
    let invocation = fixture.take();
    let mut native = Native::new(
        &fixture.caller,
        &fixture.environment,
        Signals::new().unwrap(),
    );
    native.vps_prepare(invocation, fixture.end).await.unwrap();
    assert!(native.cleanup_passed());
    assert_eq!(
        std::fs::read_to_string(fixture.marker("frame-count")).unwrap(),
        fixture.frame_bytes.to_string()
    );
    assert!(fixture.marker("released").exists());
    let captures = fixture.captures();
    assert_eq!(
        captures[0].as_ref().unwrap().stdout.bytes,
        fixture.ready.len() as u64
    );
    assert!(captures[1].as_ref().unwrap().stdout.bytes > 0);
    assert!(captures[1].as_ref().unwrap().stderr.bytes > 0);
    assert!(fixture.exits().iter().all(|value| {
        value
            .as_ref()
            .is_some_and(|v| v.status == Some(0) && v.settled)
    }));
    drop(native);
    fixture.finish().unwrap();
}

#[tokio::test]
async fn late_lease_stdout_after_release_cannot_pass_the_completed_capture_fence() {
    let lease = format!("{LEASE}\nos.write(1,b'late extra record\\n')\n");
    let mut fixture = PreparationFixture::new(
        &lease,
        "with open(ROOT+'/ansible-finished','w') as out: out.write('done')\n",
        240,
    )
    .await;
    let invocation = fixture.take();
    let mut native = Native::new(
        &fixture.caller,
        &fixture.environment,
        Signals::new().unwrap(),
    );
    let error = native
        .vps_prepare(invocation, fixture.end)
        .await
        .unwrap_err();
    assert_eq!(error.code, 1);
    assert!(fixture.marker("released").exists());
    assert!(native.cleanup_passed());
    assert!(fixture.captures()[0].as_ref().unwrap().stdout.bytes > fixture.ready.len() as u64);
    drop(native);
    fixture.finish().unwrap();
}

#[tokio::test]
async fn stderr_eof_cancels_quiet_ansible_while_the_lease_leader_and_stdout_remain() {
    let lease = LEASE.replace(
        "release=exact(8)",
        r#"
while not os.path.exists(ROOT+'/ansible-started'): time.sleep(0.005)
os.close(2)
while True: time.sleep(1)
release=exact(8)"#,
    );
    let mut fixture = PreparationFixture::new(
        &lease,
        r#"
with open(ROOT+'/ansible-started','w') as out: out.write('started')
time.sleep(30)
with open(ROOT+'/ansible-finished','w') as out: out.write('unexpected')
"#,
        240,
    )
    .await;
    let invocation = fixture.take();
    let mut native = Native::new(
        &fixture.caller,
        &fixture.environment,
        Signals::new().unwrap(),
    );
    let error = native
        .vps_prepare(invocation, fixture.end)
        .await
        .unwrap_err();
    assert_eq!(error.code, 1);
    assert!(fixture.marker("ansible-started").exists());
    assert!(!fixture.marker("ansible-finished").exists());
    assert!(native.cleanup_passed());
    assert!(fixture.exits().iter().all(|value| {
        value
            .as_ref()
            .is_some_and(|v| v.status.is_some() && v.settled)
    }));
    drop(native);
    fixture.finish().unwrap();
}

#[tokio::test]
async fn first_interrupt_survives_a_later_signal_during_joined_settlement() {
    let mut fixture = PreparationFixture::new(
        LEASE,
        r#"
def interrupted(signum,frame):
    with open(ROOT+'/ansible-interrupted','w') as out: out.write('seen')
signal.signal(signal.SIGINT,interrupted)
signal.signal(signal.SIGTERM,interrupted)
with open(ROOT+'/ansible-started','w') as out: out.write('started')
while True: time.sleep(1)
"#,
        240,
    )
    .await;
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let started = fixture.marker("ansible-started");
    let interrupted = fixture.marker("ansible-interrupted");
    let injected = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            sender.send(130).unwrap();
            while !interrupted.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            sender.send(143).unwrap();
        })
        .await
        .unwrap();
    });
    let invocation = fixture.take();
    let mut native = Native::new(
        &fixture.caller,
        &fixture.environment,
        Signals::injected(receiver),
    );
    let error = native
        .vps_prepare(invocation, fixture.end)
        .await
        .unwrap_err();
    injected.await.unwrap();
    assert_eq!(error.code, 130);
    assert_eq!(native.ready().await.unwrap_err().code, 130);
    assert!(native.cleanup_passed());
    assert!(fixture.exits().iter().all(|value| {
        value
            .as_ref()
            .is_some_and(|v| v.status.is_some() && v.settled)
    }));
    drop(native);
    fixture.finish().unwrap();
}
