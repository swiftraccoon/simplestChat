//! Replace one member's receiver without changing their sender or membership.

use super::RoomManager;
use crate::media::transport_manager::ReceiveReplacementPlacement;
use crate::media::types::{ReceiveBandwidthEstimate, TransportInfo};
use mediasoup::prelude::TransportId;
use tokio::sync::mpsc;

impl RoomManager {
    /// An admitted operation finishes in its owned task even when its reply is
    /// lost. The expected receiver ID is both a precondition and a bounded retry
    /// identity; it never authorizes replacing another receiver or participant.
    pub(crate) async fn replace_recv_transport(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        transport_id: &str,
        bwe_sender: Option<mpsc::Sender<ReceiveBandwidthEstimate>>,
    ) -> anyhow::Result<TransportInfo> {
        let expected = transport_id
            .parse::<TransportId>()
            .map_err(|_| anyhow::anyhow!("Invalid receive transport ID"))?;
        let participant_id = participant_id.to_owned();
        let expected_sender = expected_sender.clone();
        self.run_room_control(room_id, move |manager, room_id, _control| async move {
            let media_session = manager
                .media_session_for_sender(&room_id, &participant_id, &expected_sender)
                .await?;
            let media_id = Self::media_participant_id(&room_id, &participant_id, media_session);
            let transports = manager.media_server.transport_manager();
            if let Some(replay) = transports
                .recv_replacement_replay(&media_id, expected)
                .await?
            {
                return Ok(replay);
            }
            let _ipc = manager
                .reserve_media_control_ipc_for_sender(&room_id, &participant_id, &expected_sender)
                .await?;
            let config = manager.media_server.config();
            let routers = manager.media_server.router_manager();
            let (router, worker, lease) = routers
                .place_viewer(&room_id, &media_id, &config.router_config)
                .await?;
            let retain_existing_lease = routers.worker_for_participant(&media_id) == Some(worker);
            let webrtc_server = manager
                .media_server
                .get_webrtc_server_for_worker(worker)
                .await?;
            let replacement = transports
                .replace_recv_transport(
                    &media_id,
                    expected,
                    ReceiveReplacementPlacement {
                        router,
                        webrtc_server,
                        lease,
                        retain_existing_lease,
                    },
                    &config.webrtc_transport_config,
                    bwe_sender,
                )
                .await?;
            // Control excludes membership changes throughout native setup and
            // publication. The transport manager separately fences media eviction.
            Ok(replacement)
        })
        .await
    }

    /// A cached estimate is usable only while its receiver and sender still own
    /// this membership. This performs no native statistics request.
    pub(crate) async fn is_current_recv_transport(
        &self,
        room_id: &str,
        participant_id: &str,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        expected: TransportId,
    ) -> bool {
        let Ok(session) = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await
        else {
            return false;
        };
        self.media_server
            .transport_manager()
            .is_current_recv_transport(
                &Self::media_participant_id(room_id, participant_id, session),
                expected,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::ServerMetrics;
    use crate::room::JoinResult;
    use mediasoup::prelude::Transport;
    use std::sync::{Arc, atomic::AtomicBool};
    use uuid::Uuid;

    struct Fixture {
        manager: RoomManager,
        room: String,
        member: String,
        sender: mpsc::Sender<crate::OutboundJson>,
        _messages: mpsc::Receiver<crate::OutboundJson>,
        sent: TransportInfo,
        received: TransportInfo,
    }

    impl Fixture {
        async fn new() -> Self {
            let manager = RoomManager::new_for_connection_tests(ServerMetrics::new()).await;
            let room = format!("receiver-{}", Uuid::new_v4());
            let member = Uuid::new_v4().to_string();
            let (sender, messages) = mpsc::channel(64);
            let joined = manager
                .add_participant(
                    &room,
                    member.clone(),
                    "Receiver fixture".into(),
                    sender.clone(),
                    false,
                    Arc::new(AtomicBool::new(false)),
                    None,
                    "receiver-fixture-token",
                    None,
                    None,
                )
                .await
                .unwrap();
            assert!(matches!(joined, JoinResult::Joined { .. }));
            let sent = manager
                .create_send_transport(&room, &member, &sender)
                .await
                .unwrap();
            let received = manager
                .create_recv_transport(&room, &member, &sender)
                .await
                .unwrap();
            Self {
                manager,
                room,
                member,
                sender,
                _messages: messages,
                sent,
                received,
            }
        }

        async fn finish(self) {
            self.manager
                .remove_participant_for_sender(&self.room, &self.member, &self.sender)
                .await
                .unwrap();
            self.manager.media_server.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn receive_replacement_rejects_stale_sender_and_preserves_membership() {
        let f = Fixture::new().await;
        let room = f.manager.get_room(&f.room).unwrap();
        let session = room.read().await.participants[&f.member].media_session_id;
        let (stale_sender, _messages) = mpsc::channel(8);
        assert!(
            f.manager
                .replace_recv_transport(&f.room, &f.member, &stale_sender, &f.received.id, None)
                .await
                .is_err()
        );
        assert!(
            f.manager
                .replace_recv_transport(&f.room, &f.member, &f.sender, &f.sent.id, None)
                .await
                .is_err(),
            "send transport IDs never authorize receiver replacement"
        );
        let replacement = f
            .manager
            .replace_recv_transport(&f.room, &f.member, &f.sender, &f.received.id, None)
            .await
            .unwrap();
        assert_ne!(replacement.id, f.received.id);
        let state = room.read().await;
        assert_eq!(state.participants.len(), 1);
        let member = &state.participants[&f.member];
        assert_eq!(member.media_session_id, session);
        assert!(member.sender.same_channel(&f.sender));
        let media_id = RoomManager::media_participant_id(&f.room, &f.member, session);
        drop(state);
        let media = f
            .manager
            .media_server
            .transport_manager()
            .get_participant(&media_id)
            .await
            .unwrap();
        assert_eq!(
            media.send_transport.as_ref().unwrap().id().to_string(),
            f.sent.id
        );
        assert!(!media.send_transport.as_ref().unwrap().closed());
        drop(media);
        f.finish().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn receive_replacement_owned_completion_survives_lost_caller_and_replays_same_receiver() {
        let f = Fixture::new().await;
        let room = f.manager.get_room(&f.room).unwrap();
        let control = room.read().await.control.clone();
        // All admission locks are free. Disable cooperative-budget yields so
        // Pending means the admitted operation is waiting on its owned task,
        // rather than yielding before it has acquired room control.
        let mut request = Box::pin(tokio::task::unconstrained(
            f.manager
                .replace_recv_transport(&f.room, &f.member, &f.sender, &f.received.id, None),
        ));
        assert!(futures_util::poll!(&mut request).is_pending());
        // spawn does not poll its task inline. On this current-thread runtime,
        // try_write installs the action barrier before any executor yield.
        let blocked = room.try_write().expect("admission released room state");
        let media_id = RoomManager::media_participant_id(
            &f.room,
            &f.member,
            blocked.participants[&f.member].media_session_id,
        );
        assert!(
            control.try_lock().is_err(),
            "the operation has entered owned admission"
        );
        // The admitted task is blocked on room state. Drop only its waiter,
        // then release it and wait for control as the completion barrier.
        drop(request);
        drop(blocked);
        drop(
            tokio::time::timeout(std::time::Duration::from_secs(5), control.lock())
                .await
                .expect("cancelled caller's owned replacement completes"),
        );
        let completed = f
            .manager
            .media_server
            .transport_manager()
            .get_participant(&media_id)
            .await
            .unwrap()
            .recv_transport
            .as_ref()
            .unwrap()
            .id()
            .to_string();
        assert_ne!(
            completed, f.received.id,
            "the cancelled caller completes its swap before any replay request"
        );
        let replacement = f
            .manager
            .replace_recv_transport(&f.room, &f.member, &f.sender, &f.received.id, None)
            .await
            .unwrap();
        let replay = f
            .manager
            .replace_recv_transport(&f.room, &f.member, &f.sender, &f.received.id, None)
            .await
            .unwrap();
        assert_eq!(replacement.id, replay.id);
        assert_eq!(replacement.id, completed);
        assert_ne!(replacement.id, f.received.id);
        assert!(
            f.manager
                .is_current_recv_transport(
                    &f.room,
                    &f.member,
                    &f.sender,
                    replacement.id.parse().unwrap()
                )
                .await
        );
        f.finish().await;
    }

    #[tokio::test]
    async fn receive_replacement_unavailable_worker_does_not_install_partial_receiver_or_change_membership()
     {
        let f = Fixture::new().await;
        let room = f.manager.get_room(&f.room).unwrap();
        let session = room.read().await.participants[&f.member].media_session_id;
        f.manager
            .media_server
            .worker_manager()
            .shutdown()
            .await
            .unwrap();
        assert!(
            f.manager
                .replace_recv_transport(&f.room, &f.member, &f.sender, &f.received.id, None)
                .await
                .is_err()
        );
        assert_eq!(
            room.read().await.participants[&f.member].media_session_id,
            session
        );
        let media = f
            .manager
            .media_server
            .transport_manager()
            .get_participant(&RoomManager::media_participant_id(
                &f.room, &f.member, session,
            ))
            .await
            .unwrap();
        assert_eq!(
            media.recv_transport.as_ref().unwrap().id().to_string(),
            f.received.id
        );
        assert_eq!(
            media.send_transport.as_ref().unwrap().id().to_string(),
            f.sent.id
        );
        drop(media);
        f.finish().await;
    }
}
