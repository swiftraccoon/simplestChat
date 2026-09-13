//! Peer-close ownership and real axum/tungstenite protocol regressions.

use super::*;
use axum::{Router, extract::ws::WebSocketUpgrade, routing::get};
use std::task::Poll;
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::{Message as PeerMessage, protocol::CloseFrame as PeerClose};

struct MarkDropped(Arc<AtomicBool>);

impl Drop for MarkDropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

async fn pending_writer() -> (OwnedTask, Arc<AtomicBool>) {
    let dropped = Arc::new(AtomicBool::new(false));
    let observed = dropped.clone();
    let (started, ready) = oneshot::channel();
    let writer = OwnedTask(tokio::spawn(async move {
        let _guard = MarkDropped(observed);
        started.send(()).unwrap();
        std::future::pending::<()>().await;
    }));
    ready.await.unwrap();
    (writer, dropped)
}

#[tokio::test]
async fn peer_close_stops_writer_before_polling_receiver() {
    let (mut writer, dropped) = pending_writer().await;
    let mut receiver = futures_util::stream::poll_fn(|_| {
        assert!(dropped.load(Ordering::Acquire));
        Poll::Ready(None::<Result<Message, ()>>)
    });
    assert!(matches!(
        complete_peer_close(&mut receiver, &mut writer).await,
        Outcome::Ok
    ));
    assert!(writer.0.is_finished());
}

#[tokio::test]
async fn peer_close_pending_receiver_has_a_bounded_deadline() {
    let (mut writer, dropped) = pending_writer().await;
    let mut receiver = futures_util::stream::pending::<Result<Message, ()>>();
    let result = tokio::time::timeout(
        PEER_CLOSE_TIMEOUT + Duration::from_secs(1),
        complete_peer_close(&mut receiver, &mut writer),
    )
    .await
    .unwrap();
    assert!(matches!(result, Outcome::Timeout));
    assert!(dropped.load(Ordering::Acquire));
    assert!(writer.0.is_finished());
}

#[tokio::test]
async fn peer_close_distinguishes_receive_errors_from_clean_eof() {
    let (mut writer, dropped) = pending_writer().await;
    let mut receiver = futures_util::stream::iter([Err::<Message, _>(())]);
    assert!(matches!(
        complete_peer_close(&mut receiver, &mut writer).await,
        Outcome::Error
    ));
    assert!(dropped.load(Ordering::Acquire));
}

#[tokio::test]
async fn peer_close_ready_frames_cannot_starve_its_deadline() {
    let (mut writer, dropped) = pending_writer().await;
    let mut receiver =
        futures_util::stream::repeat_with(|| Ok::<_, ()>(Message::Pong(bytes::Bytes::new())));
    let outcome = tokio::time::timeout(
        PEER_CLOSE_TIMEOUT + Duration::from_secs(1),
        complete_peer_close(&mut receiver, &mut writer),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, Outcome::Timeout));
    assert!(dropped.load(Ordering::Acquire));
}

#[tokio::test]
async fn peer_close_discards_buffered_frames_without_application_dispatch() {
    let (mut writer, dropped) = pending_writer().await;
    let mut receiver = futures_util::stream::iter([
        Ok::<_, ()>(Message::Text("not an application request".into())),
        Ok(Message::Pong(bytes::Bytes::new())),
    ]);
    assert!(matches!(
        complete_peer_close(&mut receiver, &mut writer).await,
        Outcome::Ok
    ));
    assert!(receiver.next().await.is_none());
    assert!(dropped.load(Ordering::Acquire));
}

/// Use axum's actual upgrade to exercise its pinned server transport, while
/// keeping media, database access and the public VPS outside this fixture.
async fn assert_close_echo(frame: Option<PeerClose>, queued_text: bool) {
    let (observed_tx, mut observed_rx) = mpsc::channel(1);
    let (ready_tx, mut ready_rx) = mpsc::channel(1);
    let router = Router::new().route(
        "/ws",
        get(move |upgrade: WebSocketUpgrade| {
            let observed_tx = observed_tx.clone();
            let ready_tx = ready_tx.clone();
            async move {
                upgrade.on_upgrade(move |socket| async move {
                    let outcome = tokio::time::timeout(Duration::from_secs(3), async {
                        let (mut sender, mut receiver) = socket.split();
                        let (started, ready) = oneshot::channel();
                        let mut writer = OwnedTask(tokio::spawn(async move {
                            if queued_text {
                                // feed retains a SplitSink slot without flushing it.
                                // It must be discarded, not sent after peer Close.
                                sender
                                    .feed(Message::Text("stale queued text".into()))
                                    .await
                                    .unwrap();
                            }
                            started.send(()).unwrap();
                            std::future::pending::<()>().await;
                            drop(sender);
                        }));
                        ready.await.unwrap();
                        ready_tx.send(()).await.unwrap();
                        assert!(matches!(receiver.next().await, Some(Ok(Message::Close(_)))));
                        complete_peer_close(&mut receiver, &mut writer).await
                    })
                    .await;
                    let _ = observed_tx.send(outcome).await;
                })
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel();
    let mut server = OwnedTask(tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    }));
    let check = tokio::time::timeout(Duration::from_secs(5), async {
        let (mut peer, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
            .await
            .unwrap();
        ready_rx.recv().await.unwrap();
        peer.send(PeerMessage::Close(frame.clone())).await.unwrap();
        assert_eq!(
            peer.next().await.unwrap().unwrap(),
            PeerMessage::Close(frame)
        );
        assert!(
            peer.next().await.is_none(),
            "Close reply must be followed by clean EOF"
        );
        assert!(matches!(
            observed_rx.recv().await.unwrap().unwrap(),
            Outcome::Ok
        ));
    })
    .await;
    let _ = stop.send(());
    let shutdown = tokio::time::timeout(Duration::from_secs(2), &mut server.0).await;
    check.unwrap();
    shutdown.unwrap().unwrap();
}

#[tokio::test]
async fn peer_close_echoes_normal_code_and_reason() {
    assert_close_echo(
        Some(PeerClose {
            code: 1000.into(),
            reason: "Finished testing".into(),
        }),
        false,
    )
    .await;
}

#[tokio::test]
async fn peer_close_echoes_empty_frame_without_inventing_a_code() {
    assert_close_echo(None, false).await;
}

#[tokio::test]
async fn peer_close_discards_split_sink_slot_before_echoing_normal_close() {
    assert_close_echo(
        Some(PeerClose {
            code: 1000.into(),
            reason: "Queued writer".into(),
        }),
        true,
    )
    .await;
}

#[tokio::test]
async fn peer_close_discards_split_sink_slot_before_echoing_empty_close() {
    assert_close_echo(None, true).await;
}
