//! Transports, producers, consumers and layers on the room's router, with the
//! per-sender media budgets and the broadcaster authorization they share.

use super::super::*;

impl RoomManager {
    pub(in crate::room) fn media_participant_id(
        room_id: &RoomId,
        participant_id: &ParticipantId,
        media_session_id: uuid::Uuid,
    ) -> String {
        format!("{room_id}\u{1f}{media_session_id}\u{1f}{participant_id}")
    }

    /// Gets router RTP capabilities for a room
    ///
    /// # Errors
    /// Returns an error if the room doesn't exist
    pub(crate) async fn get_router_rtp_capabilities(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
    ) -> Result<RtpCapabilitiesFinalized> {
        self.media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let router = self.get_router(room_id).await?;
        Ok(router.rtp_capabilities().clone())
    }

    /// Creates a send transport for a participant
    ///
    /// # Errors
    /// Returns an error if transport creation fails
    pub(crate) async fn create_send_transport(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
    ) -> Result<TransportInfo> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let router = self.get_router(room_id).await?;
        let webrtc_server = self
            .media_server
            .get_webrtc_server_for_room(room_id.as_str())
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let media_config = self.media_server.config();
        let _ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let transport = self
            .media_server
            .transport_manager()
            .create_send_transport(
                media_participant_id.clone(),
                &router,
                webrtc_server,
                &media_config.webrtc_transport_config,
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        if !self
            .is_current_media_session(room_id, participant_id, expected_sender, media_session_id)
            .await
        {
            let _ = self
                .media_server
                .transport_manager()
                .remove_participant(&media_participant_id)
                .await;
            anyhow::bail!("Participant is no longer in this room");
        }
        Ok(transport)
    }

    /// Creates a receive transport for a participant
    ///
    /// # Errors
    /// Returns an error if transport creation fails
    pub(crate) async fn create_recv_transport(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
    ) -> Result<TransportInfo> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let media_config = self.media_server.config();
        // A busy room spreads its viewers over the workers; the primary router
        // keeps every producer and the observers that watch them.
        let (router, worker_id, lease) = self
            .media_server
            .router_manager()
            .place_viewer(
                room_id.as_str(),
                &media_participant_id,
                &media_config.router_config,
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        let webrtc_server = self
            .media_server
            .get_webrtc_server_for_worker(worker_id)
            .await?;
        let _ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let transport = self
            .media_server
            .transport_manager()
            .create_recv_transport(
                media_participant_id.clone(),
                &router,
                webrtc_server,
                &media_config.webrtc_transport_config,
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        if let Some(lease) = lease
            && let Err(error) = self
                .media_server
                .transport_manager()
                .attach_viewer_lease(&media_participant_id, lease)
                .await
        {
            debug!(%room_id, %error, "Receive transport left before its placement was recorded");
        }
        if !self
            .is_current_media_session(room_id, participant_id, expected_sender, media_session_id)
            .await
        {
            let _ = self
                .media_server
                .transport_manager()
                .remove_participant(&media_participant_id)
                .await;
            anyhow::bail!("Participant is no longer in this room");
        }
        Ok(transport)
    }

    /// Connects a transport with DTLS parameters
    ///
    /// # Errors
    /// Returns an error if connection fails
    pub(crate) async fn connect_transport(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        transport_id: &str,
        dtls_parameters: DtlsParameters,
    ) -> Result<()> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let connected = self
            .media_server
            .transport_manager()
            .connect_transport(&media_participant_id, transport_id, dtls_parameters)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        if !connected {
            Self::refund_media_control_ipc(ipc_reservation).await;
        }

        if connected {
            debug!(
                "Connected transport {} for participant {}",
                transport_id, participant_id
            );
        }
        Ok(())
    }

    /// Creates a producer for a participant
    ///
    /// # Errors
    /// Returns an error if producer creation fails
    pub(crate) async fn create_producer(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        kind: MediaKind,
        rtp_parameters: RtpParameters,
        source: Option<String>,
    ) -> Result<String> {
        let source = source
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Media source is required"))?;
        if !Self::valid_media_source(kind, source) {
            anyhow::bail!("Media kind does not match source");
        }
        if !self
            .can_participant_produce(room_id, participant_id, expected_sender, kind, source)
            .await?
        {
            anyhow::bail!("You are not allowed to produce this media type");
        }
        let _reservation = self
            .reserve_media_mutation_for_sender(room_id, participant_id, expected_sender, None)
            .await?;

        // Create producer WITHOUT room lock
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let producer = self
            .media_server
            .transport_manager()
            .create_producer(
                &media_participant_id,
                kind,
                rtp_parameters,
                AppData::default(),
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        let producer_id = producer.id().to_string();
        let producer_id_typed = producer.id();

        // Store producer info, broadcast, and clone observers (per-room write lock)
        let (active_obs, audio_obs) = {
            let room_lock = match self.get_room(room_id) {
                Ok(room) => room,
                Err(error) => {
                    let _ = self
                        .media_server
                        .transport_manager()
                        .close_producer(&media_participant_id, &producer_id)
                        .await;
                    return Err(error);
                }
            };
            let mut room = room_lock.write().await;
            if let Err(error) = room.ensure_live() {
                drop(room);
                let _ = self
                    .media_server
                    .transport_manager()
                    .close_producer(&media_participant_id, &producer_id)
                    .await;
                return Err(error);
            }
            let allowed = room
                .participants
                .get(participant_id)
                .is_some_and(|participant| {
                    participant.media_session_id == media_session_id
                        && participant.sender.same_channel(expected_sender)
                        && Self::participant_can_produce(&room, participant, kind, source)
                });
            if !allowed {
                drop(room);
                let _ = self
                    .media_server
                    .transport_manager()
                    .close_producer(&media_participant_id, &producer_id)
                    .await;
                anyhow::bail!("You are no longer allowed to produce this media type");
            }

            let participant = room.participants.get_mut(participant_id).unwrap();
            participant.producers.insert(
                producer_id.clone(),
                ProducerRecord {
                    kind,
                    source: Some(source.to_string()),
                },
            );
            room.producer_to_participant
                .insert(producer_id.clone(), *participant_id);

            room.broadcast_except(
                participant_id,
                &ServerMessage::NewProducer {
                    participant_id: *participant_id,
                    producer_id: producer_id.clone(),
                    kind,
                    source: Some(source.to_string()),
                },
            );

            // Clone observers before releasing lock
            (
                room.active_speaker_observer.clone(),
                room.audio_level_observer.clone(),
            )
        }; // room lock released

        // Add to observers OUTSIDE lock (async IPC)
        if kind == MediaKind::Audio {
            if let Some(obs) = &active_obs
                && let Err(error) = obs
                    .add_producer(RtpObserverAddProducerOptions::new(producer_id_typed))
                    .await
            {
                warn!(%room_id, producer_id, %error, "Active speaker observer rejected a producer");
            }
            if let Some(obs) = &audio_obs
                && let Err(error) = obs
                    .add_producer(RtpObserverAddProducerOptions::new(producer_id_typed))
                    .await
            {
                warn!(%room_id, producer_id, %error, "Audio level observer rejected a producer");
            }
        }

        info!(
            "Created {:?} producer {} for participant {} in room {}",
            kind, producer_id, participant_id, room_id
        );

        Ok(producer_id)
    }

    /// Creates a consumer for a participant to receive media
    ///
    /// # Errors
    /// Returns an error if consumer creation fails
    pub(crate) async fn create_consumer(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        producer_id: ProducerId,
        rtp_capabilities: RtpCapabilities,
        notification_sender: Option<mpsc::Sender<crate::OutboundJson>>,
    ) -> MediaResult<crate::media::types::ConsumerInfo> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await
            .map_err(|error| crate::media::types::MediaError::ConsumerError(error.to_string()))?;
        let consumer_source = {
            let room_lock = self.get_room(room_id).map_err(|error| {
                crate::media::types::MediaError::ConsumerError(error.to_string())
            })?;
            let room = room_lock.read().await;
            let metadata = room
                .producer_to_participant
                .get(&producer_id.to_string())
                .and_then(|owner| room.participants.get(owner))
                .and_then(|owner| owner.producers.get(&producer_id.to_string()))
                .ok_or_else(|| {
                    crate::media::types::MediaError::ConsumerError(
                        "Producer is not in this room".to_string(),
                    )
                })?;
            crate::media::transport_manager::ConsumerMediaSource::from_room_source(
                metadata.kind,
                metadata.source.as_deref(),
            )
        };
        let _ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await
            .map_err(|error| crate::media::types::MediaError::ConsumerError(error.to_string()))?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        // A viewer placed on another worker consumes the producer's pipe there.
        self.media_server
            .router_manager()
            .ensure_piped(room_id.as_str(), &media_participant_id, producer_id)
            .await?;
        // Look up the consumer counter for the participant's worker (for load-aware tracking)
        let consumer_counter = self
            .media_server
            .get_consumer_counter_for_participant(room_id.as_str(), &media_participant_id)
            .await
            .ok()
            .flatten();

        // No room lock needed — purely transport_manager operation
        let consumer = self
            .media_server
            .transport_manager()
            .create_consumer(
                &media_participant_id,
                producer_id,
                rtp_capabilities,
                AppData::new(consumer_source),
                notification_sender,
                consumer_counter,
            )
            .await?;

        if !self
            .is_current_media_session(room_id, participant_id, expected_sender, media_session_id)
            .await
        {
            let _ = self
                .media_server
                .transport_manager()
                .remove_participant(&media_participant_id)
                .await;
            return Err(crate::media::types::MediaError::ConsumerError(
                "Participant is no longer in this room".to_string(),
            ));
        }

        let consumer_info = crate::media::types::ConsumerInfo::from_consumer(&consumer);

        debug!(
            "Created consumer {} for participant {} in room {}",
            consumer_info.id, participant_id, room_id
        );

        Ok(consumer_info)
    }

    /// Resumes a consumer for a participant
    pub(crate) async fn resume_consumer(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        consumer_id: &str,
    ) -> Result<()> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let resumed = self
            .media_server
            .transport_manager()
            .resume_consumer(&media_participant_id, consumer_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        if !resumed {
            Self::refund_media_control_ipc(ipc_reservation).await;
        }

        if resumed {
            debug!(
                "Resumed consumer {} for participant {}",
                consumer_id, participant_id
            );
        }
        Ok(())
    }

    /// Pauses a consumer for a participant
    pub(crate) async fn pause_consumer(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        consumer_id: &str,
    ) -> Result<()> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let paused = self
            .media_server
            .transport_manager()
            .pause_consumer(&media_participant_id, consumer_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        if !paused {
            Self::refund_media_control_ipc(ipc_reservation).await;
        }

        if paused {
            debug!(
                "Paused consumer {} for participant {}",
                consumer_id, participant_id
            );
        }
        Ok(())
    }

    /// Releases only the current sender's consumer, idempotently within its
    /// session. Teardown may race producer closure or repeat after a lost reply.
    pub(crate) async fn close_consumer(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        consumer_id: &str,
    ) -> Result<()> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let closed = self
            .media_server
            .transport_manager()
            .close_consumer(&media_participant_id, consumer_id)
            .await?;
        if !closed {
            Self::refund_media_control_ipc(ipc_reservation).await;
        }
        Ok(())
    }

    /// Closes a producer for a participant
    pub(crate) async fn close_producer(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        producer_id: &str,
    ) -> Result<()> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let _reservation = self
            .reserve_media_mutation_for_sender(
                room_id,
                participant_id,
                expected_sender,
                Some(producer_id),
            )
            .await?;
        // Close producer WITHOUT room lock
        self.media_server
            .transport_manager()
            .close_producer(&media_participant_id, producer_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        // Remove from room's participant tracking, observer maps, and notify others (per-room write lock)
        let (active_obs, audio_obs, was_audio) = {
            let room_lock = self.get_room(room_id)?;
            let mut room = room_lock.write().await;
            if !Self::participant_session_is_current(
                &room,
                participant_id,
                expected_sender,
                media_session_id,
            ) {
                anyhow::bail!("Participant is no longer in this room");
            }
            let was_audio = room
                .participants
                .get_mut(participant_id)
                .and_then(|participant| participant.producers.remove(producer_id))
                .is_some_and(|record| record.kind == MediaKind::Audio);
            room.producer_to_participant.remove(producer_id);

            room.broadcast_except(
                participant_id,
                &ServerMessage::ProducerClosed {
                    producer_id: producer_id.to_string(),
                },
            );

            // Clone observers before releasing lock
            (
                room.active_speaker_observer.clone(),
                room.audio_level_observer.clone(),
                was_audio,
            )
        }; // room lock released

        // Remove from observers OUTSIDE lock
        if was_audio && let Ok(pid) = producer_id.parse::<ProducerId>() {
            if let Some(obs) = &active_obs {
                let _ = obs.remove_producer(pid).await;
            }
            if let Some(obs) = &audio_obs {
                let _ = obs.remove_producer(pid).await;
            }
        }

        info!(
            "Closed producer {} for participant {} in room {}",
            producer_id, participant_id, room_id
        );
        Ok(())
    }

    /// Pauses a producer. Mediasoup propagates producer pause state to its
    /// consumers, so no server-wide consumer scan is needed.
    pub(crate) async fn pause_producer(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        producer_id: &str,
    ) -> Result<()> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let Some(reservation) = self
            .reserve_producer_pause_mutation_for_sender(
                room_id,
                participant_id,
                expected_sender,
                producer_id,
                true,
            )
            .await?
        else {
            return Ok(());
        };
        // Pause the producer itself
        let changed = self
            .media_server
            .transport_manager()
            .pause_producer(&media_participant_id, producer_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        if !changed {
            Self::refund_media_mutation(reservation).await;
            return Ok(());
        }

        // Broadcast ProducerPaused to other participants in the room
        let room_lock = self.get_room(room_id)?;
        let room = room_lock.read().await;
        if !Self::participant_session_is_current(
            &room,
            participant_id,
            expected_sender,
            media_session_id,
        ) {
            anyhow::bail!("Participant is no longer in this room");
        }
        room.broadcast_except(
            participant_id,
            &ServerMessage::ProducerPaused {
                producer_id: producer_id.to_string(),
            },
        );

        info!(
            "Paused producer {} for participant {} in room {}",
            producer_id, participant_id, room_id
        );
        Ok(())
    }

    /// Resumes a producer. Mediasoup propagates producer resume state to its
    /// consumers, so no server-wide consumer scan is needed.
    pub(crate) async fn resume_producer(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        producer_id: &str,
    ) -> Result<()> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let Some(reservation) = self
            .reserve_producer_pause_mutation_for_sender(
                room_id,
                participant_id,
                expected_sender,
                producer_id,
                false,
            )
            .await?
        else {
            return Ok(());
        };
        // Resume the producer itself
        let changed = self
            .media_server
            .transport_manager()
            .resume_producer(&media_participant_id, producer_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        if !changed {
            Self::refund_media_mutation(reservation).await;
            return Ok(());
        }

        // Broadcast ProducerResumed to other participants in the room
        let room_lock = self.get_room(room_id)?;
        let room = room_lock.read().await;
        if !Self::participant_session_is_current(
            &room,
            participant_id,
            expected_sender,
            media_session_id,
        ) {
            anyhow::bail!("Participant is no longer in this room");
        }
        room.broadcast_except(
            participant_id,
            &ServerMessage::ProducerResumed {
                producer_id: producer_id.to_string(),
            },
        );

        info!(
            "Resumed producer {} for participant {} in room {}",
            producer_id, participant_id, room_id
        );
        Ok(())
    }

    /// Sets preferred simulcast layers for a consumer
    pub(crate) async fn set_preferred_layers(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        consumer_id: &str,
        spatial_layer: u8,
        temporal_layer: Option<u8>,
    ) -> Result<()> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let changed = self
            .media_server
            .transport_manager()
            .set_preferred_layers(
                &media_participant_id,
                consumer_id,
                ConsumerLayers {
                    spatial_layer,
                    temporal_layer,
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        if changed {
            self.metrics.inc_consumer_layer_request();
        } else {
            Self::refund_media_control_ipc(ipc_reservation).await;
        }
        Ok(())
    }

    /// Subscribes to BWE events for a participant's recv transport
    pub(crate) async fn subscribe_bwe_events(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        bwe_sender: mpsc::Sender<crate::media::types::ReceiveBandwidthEstimate>,
    ) -> Result<()> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let _ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await?;
        self.media_server
            .transport_manager()
            .subscribe_bwe_events(&media_participant_id, bwe_sender)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Applies the receive transport's bandwidth tier as a layer ceiling to
    /// the participant's layered consumers: one media-control request per
    /// consumer whose applied layers change, none for the rest. Returns the
    /// number of worker requests made.
    pub(crate) async fn set_bandwidth_ceiling(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        expected_transport: TransportId,
        spatial_layer: u8,
    ) -> Result<usize> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let transports = self.media_server.transport_manager();
        let pending = transports
            .set_bandwidth_ceiling_for_transport(
                &media_participant_id,
                Some(expected_transport),
                spatial_layer,
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        let mut written = 0;
        for consumer_id in pending {
            let reservation = self
                .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
                .await?;
            let changed = transports
                .apply_layer_ceilings(&media_participant_id, &consumer_id)
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
            if changed {
                written += 1;
                self.metrics.inc_consumer_layer_request();
            } else {
                Self::refund_media_control_ipc(reservation).await;
            }
        }
        Ok(written)
    }

    /// Restarts ICE on a transport, returning new ICE parameters
    pub(crate) async fn restart_ice(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        transport_id: &str,
    ) -> Result<IceParameters> {
        let media_session_id = self
            .media_session_for_sender(room_id, participant_id, expected_sender)
            .await?;
        let media_participant_id =
            Self::media_participant_id(room_id, participant_id, media_session_id);
        let _ipc_reservation = self
            .reserve_media_control_ipc_for_sender(room_id, participant_id, expected_sender)
            .await?;
        self.media_server
            .transport_manager()
            .restart_ice(&media_participant_id, transport_id)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    pub(in crate::room) async fn reserve_media_mutation_for_sender(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        producer_id: Option<&str>,
    ) -> Result<MediaMutationReservation> {
        let room_lock = self.get_room(room_id)?;
        let mut room = measure(Stage::RoomLockWait, room_lock.write()).await;
        let participant = Self::participant_for_sender(&room, participant_id, expected_sender)?;
        if producer_id.is_some_and(|producer_id| !participant.producers.contains_key(producer_id)) {
            anyhow::bail!("Producer does not belong to this participant");
        }
        let reserved_at = std::time::Instant::now();
        if !room.media_mutations.reserve(reserved_at) {
            anyhow::bail!("Room media changes are rate limited");
        }
        drop(room);
        Ok(MediaMutationReservation {
            room: room_lock,
            reserved_at,
        })
    }

    pub(in crate::room) async fn reserve_producer_pause_mutation_for_sender(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        producer_id: &str,
        desired_paused: bool,
    ) -> Result<Option<MediaMutationReservation>> {
        let room_lock = self.get_room(room_id)?;
        let mut room = measure(Stage::RoomLockWait, room_lock.write()).await;
        let participant = Self::participant_for_sender(&room, participant_id, expected_sender)?;
        if !participant.producers.contains_key(producer_id) {
            anyhow::bail!("Producer does not belong to this participant");
        }
        if self
            .media_server
            .transport_manager()
            .find_producer_paused(producer_id)
            == Some(desired_paused)
        {
            return Ok(None);
        }
        let reserved_at = std::time::Instant::now();
        if !room.media_mutations.reserve(reserved_at) {
            anyhow::bail!("Room media changes are rate limited");
        }
        drop(room);
        Ok(Some(MediaMutationReservation {
            room: room_lock,
            reserved_at,
        }))
    }

    pub(in crate::room) async fn refund_media_mutation(reservation: MediaMutationReservation) {
        let mut room = reservation.room.write().await;
        room.media_mutations.refund(reservation.reserved_at);
    }

    pub(in crate::room) async fn reserve_media_control_ipc_for_sender(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
    ) -> Result<MediaControlIpcReservation> {
        let room_lock = self.get_room(room_id)?;
        let mut room = measure(Stage::RoomLockWait, room_lock.write()).await;
        Self::participant_for_sender(&room, participant_id, expected_sender)?;
        let reserved_at = std::time::Instant::now();
        if !room.media_control_ipc.reserve(reserved_at) {
            anyhow::bail!("Room media control operations are rate limited");
        }
        drop(room);
        Ok(MediaControlIpcReservation {
            room: room_lock,
            reserved_at,
        })
    }

    pub(in crate::room) async fn refund_media_control_ipc(reservation: MediaControlIpcReservation) {
        let mut room = reservation.room.write().await;
        room.media_control_ipc.refund(reservation.reserved_at);
    }

    pub(in crate::room) async fn media_session_for_sender(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
    ) -> Result<uuid::Uuid> {
        let room_lock = self.get_room(room_id)?;
        let room = measure(Stage::RoomLockWait, room_lock.read()).await;
        Ok(Self::participant_for_sender(&room, participant_id, expected_sender)?.media_session_id)
    }

    pub(in crate::room) async fn is_current_media_session(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        media_session_id: uuid::Uuid,
    ) -> bool {
        let Ok(room_lock) = self.get_room(room_id) else {
            return false;
        };
        let room = room_lock.read().await;
        Self::participant_session_is_current(
            &room,
            participant_id,
            expected_sender,
            media_session_id,
        )
    }

    pub(in crate::room) fn valid_media_source(kind: MediaKind, source: &str) -> bool {
        matches!(
            (kind, source),
            (MediaKind::Audio, "microphone")
                | (MediaKind::Audio, "screen-audio")
                | (MediaKind::Video, "camera")
                | (MediaKind::Video, "screen")
        )
    }

    pub(in crate::room) fn producer_allowed_without_capacity(
        room: &Room,
        participant: &Participant,
        kind: MediaKind,
        source: &str,
    ) -> bool {
        if !Self::valid_media_source(kind, source) {
            return false;
        }

        if let Some(settings) = &room.settings {
            if participant.role == roles::Role::Guest && !settings.guests_can_broadcast {
                return false;
            }
            if !settings.allow_screen_sharing && matches!(source, "screen" | "screen-audio") {
                return false;
            }
            if !settings.allow_video && kind == MediaKind::Video && source == "camera" {
                return false;
            }
        }

        let moderated = room.settings.as_ref().is_some_and(|s| s.moderated);
        moderation::can_produce(&participant.punitive, participant.role, moderated, kind)
    }

    pub(in crate::room) fn participant_can_produce(
        room: &Room,
        participant: &Participant,
        kind: MediaKind,
        source: &str,
    ) -> bool {
        if !Self::producer_allowed_without_capacity(room, participant, kind, source) {
            return false;
        }

        if let Some(max) = Self::broadcaster_limit(room) {
            let broadcaster_count = room
                .participants
                .values()
                .filter(|participant| !participant.producers.is_empty())
                .count();
            if participant.producers.is_empty() && broadcaster_count >= max {
                return false;
            }
        }

        true
    }

    /// The room's own broadcaster limit beneath the deployment's ceiling; a
    /// negative stored limit (which validation refuses) admits nobody.
    pub(in crate::room) fn broadcaster_limit(room: &Room) -> Option<usize> {
        let own = room
            .settings
            .as_ref()
            .and_then(|settings| settings.max_broadcasters)
            .map(|maximum| usize::try_from(maximum).unwrap_or(0));
        match (own, room.broadcaster_ceiling) {
            (Some(own), Some(ceiling)) => Some(own.min(ceiling)),
            (own, ceiling) => own.or(ceiling),
        }
    }

    /// Remove producer bookkeeping for streams that no longer satisfy current
    /// room policy. Physical mediasoup closure is deliberately performed by
    /// `close_revoked_producers` after the room lock is released.
    pub(in crate::room) fn revoke_unauthorized_producers(
        room: &mut Room,
        participant_filter: Option<&ParticipantId>,
        reason: &str,
    ) -> Vec<RevokedProducer> {
        let max_broadcasters = Self::broadcaster_limit(room);

        // Determine which otherwise-authorized broadcasters survive a newly
        // reduced cap. Prefer more privileged roles, then stable participant ID,
        // so enforcement is deterministic and does not depend on HashMap order.
        let mut broadcaster_candidates: Vec<(ParticipantId, roles::Role)> = room
            .participants
            .values()
            .filter(|participant| {
                participant.producers.values().any(|record| {
                    record.source.as_deref().is_some_and(|source| {
                        Self::producer_allowed_without_capacity(
                            room,
                            participant,
                            record.kind,
                            source,
                        )
                    })
                })
            })
            .map(|participant| (participant.id, participant.role))
            .collect();
        broadcaster_candidates
            .sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        let over_capacity: HashSet<ParticipantId> = max_broadcasters
            .map(|maximum| {
                broadcaster_candidates
                    .into_iter()
                    .skip(maximum)
                    .map(|(participant_id, _)| participant_id)
                    .collect()
            })
            .unwrap_or_default();

        let revoked: Vec<RevokedProducer> = room
            .participants
            .values()
            .filter(|participant| participant_filter.is_none_or(|filter| participant.id == *filter))
            .flat_map(|participant| {
                participant.producers.iter().filter_map(
                    |(producer_id, ProducerRecord { kind, source })| {
                        let allowed = source.as_deref().is_some_and(|source| {
                            Self::producer_allowed_without_capacity(
                                room,
                                participant,
                                *kind,
                                source,
                            )
                        });
                        (!allowed || over_capacity.contains(&participant.id)).then(|| {
                            RevokedProducer {
                                participant_id: participant.id,
                                media_session_id: participant.media_session_id,
                                producer_id: producer_id.clone(),
                                kind: *kind,
                            }
                        })
                    },
                )
            })
            .collect();

        for producer in &revoked {
            if let Some(participant) = room.participants.get_mut(&producer.participant_id) {
                participant.producers.remove(&producer.producer_id);
            }
            room.producer_to_participant.remove(&producer.producer_id);
            room.broadcast_all(&ServerMessage::ForceClosedProducer {
                producer_id: producer.producer_id.clone(),
                reason: reason.to_string(),
            });
            room.broadcast_all(&ServerMessage::ProducerClosed {
                producer_id: producer.producer_id.clone(),
            });
        }

        revoked
    }

    pub(in crate::room) async fn close_revoked_producers(
        &self,
        room_id: &RoomId,
        revoked: Vec<RevokedProducer>,
        active_observer: Option<ActiveSpeakerObserver>,
        audio_observer: Option<AudioLevelObserver>,
    ) {
        let mut failed_participants = HashSet::new();
        for producer in revoked {
            let media_participant_id = Self::media_participant_id(
                room_id,
                &producer.participant_id,
                producer.media_session_id,
            );
            if !failed_participants.contains(&producer.participant_id)
                && let Err(error) = self
                    .media_server
                    .transport_manager()
                    .close_producer(&media_participant_id, &producer.producer_id)
                    .await
            {
                // A failed individual close must not leave an unauthorized
                // stream alive. Fall back to dropping all media state for that
                // participant; they may establish fresh authorized transports.
                warn!(
                    %room_id,
                    participant_id = %producer.participant_id,
                    producer_id = %producer.producer_id,
                    %error,
                    "Failed to close revoked producer; dropping participant media state"
                );
                failed_participants.insert(producer.participant_id);
                if let Err(cleanup_error) = self
                    .media_server
                    .transport_manager()
                    .remove_participant(&media_participant_id)
                    .await
                {
                    warn!(
                        %room_id,
                        participant_id = %producer.participant_id,
                        %cleanup_error,
                        "Failed to drop media state after producer revocation"
                    );
                }
            }

            if producer.kind == MediaKind::Audio
                && let Ok(producer_id) = producer.producer_id.parse::<ProducerId>()
            {
                if let Some(observer) = &active_observer {
                    let _ = observer.remove_producer(producer_id).await;
                }
                if let Some(observer) = &audio_observer {
                    let _ = observer.remove_producer(producer_id).await;
                }
            }
        }
    }

    /// Check if a participant can produce a given source type
    pub(crate) async fn can_participant_produce(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        kind: MediaKind,
        source: &str,
    ) -> Result<bool> {
        let room_lock = self.get_room(room_id)?;
        let room = measure(Stage::RoomLockWait, room_lock.read()).await;
        let participant = Self::participant_for_sender(&room, participant_id, expected_sender)?;

        Ok(Self::participant_can_produce(
            &room,
            participant,
            kind,
            source,
        ))
    }

    /// Gets the router for a room (from media server)
    pub(in crate::room) async fn get_router(&self, room_id: &RoomId) -> Result<Router> {
        self.media_server
            .router_manager()
            .get_router(room_id.as_str())
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }
}
