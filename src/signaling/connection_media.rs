//! The media commands of one connection: router capabilities, transports,
//! producers, consumers, ICE restarts and layer preferences. Every arm reads
//! the room through the manager with the connection's own sender as the
//! membership proof.

use super::*;

/// Runs one media command for the connection that owns `connection`; the
/// dispatcher has already checked the lobby and membership preconditions.
pub(super) async fn handle(
    message: &ClientMessage,
    connection: Dispatch<'_, '_>,
) -> anyhow::Result<()> {
    let Dispatch {
        participant_id,
        current_room_id,
        reply,
        room_manager,
        turn_config,
        bwe_sender,
        ..
    } = connection;
    let sender = reply.sender;
    let metrics = reply.metrics;
    match message {
        ClientMessage::GetRouterRtpCapabilities => {
            if let Some(room_id) = current_room_id.as_ref() {
                let capabilities = room_manager
                    .get_router_rtp_capabilities(room_id, participant_id, sender)
                    .await?;
                reply.send(&ServerMessage::RouterRtpCapabilities {
                    rtp_capabilities: capabilities,
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::CreateSendTransport => {
            if let Some(room_id) = current_room_id.as_ref() {
                let transport_info = room_manager
                    .create_send_transport(room_id, participant_id, sender)
                    .await?;

                reply.send(&ServerMessage::TransportCreated {
                    transport_id: transport_info.id,
                    ice_parameters: transport_info.ice_parameters,
                    ice_candidates: transport_info.ice_candidates,
                    dtls_parameters: transport_info.dtls_parameters,
                    ice_servers: make_ice_servers(turn_config),
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::CreateRecvTransport => {
            if let Some(room_id) = current_room_id.as_ref() {
                let transport_info = room_manager
                    .create_recv_transport(room_id, participant_id, sender)
                    .await?;

                // Subscribe to BWE events now that recv transport exists
                if let Some(bwe_tx) = bwe_sender
                    && let Err(e) = room_manager
                        .subscribe_bwe_events(room_id, participant_id, sender, bwe_tx.clone())
                        .await
                {
                    debug!(
                        "Failed to subscribe BWE events for {}: {}",
                        participant_id, e
                    );
                }

                reply.send(&ServerMessage::TransportCreated {
                    transport_id: transport_info.id,
                    ice_parameters: transport_info.ice_parameters,
                    ice_candidates: transport_info.ice_candidates,
                    dtls_parameters: transport_info.dtls_parameters,
                    ice_servers: make_ice_servers(turn_config),
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::ReplaceRecvTransport { transport_id } => {
            let room_id = current_room_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not in a room"))?;
            let transport_info = room_manager
                .replace_recv_transport(
                    room_id,
                    participant_id,
                    sender,
                    *transport_id,
                    bwe_sender.clone(),
                )
                .await?;
            reply.send(&ServerMessage::TransportCreated {
                transport_id: transport_info.id,
                ice_parameters: transport_info.ice_parameters,
                ice_candidates: transport_info.ice_candidates,
                dtls_parameters: transport_info.dtls_parameters,
                ice_servers: make_ice_servers(turn_config),
            })?;
        }

        ClientMessage::ConnectTransport {
            transport_id,
            dtls_parameters,
        } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .connect_transport(
                        room_id,
                        participant_id,
                        sender,
                        *transport_id,
                        dtls_parameters.clone(),
                    )
                    .await?;

                reply.send(&ServerMessage::TransportConnected {
                    transport_id: *transport_id,
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::Produce {
            transport_id,
            kind,
            rtp_parameters,
            source,
        } => {
            if let Some(room_id) = current_room_id.as_ref() {
                let source = source.clone().unwrap_or_else(|| match kind {
                    mediasoup::prelude::MediaKind::Audio => "microphone".to_string(),
                    mediasoup::prelude::MediaKind::Video => "camera".to_string(),
                });

                // Newer Chromium omits the RTCP CNAME from its SDP, so browsers send an
                // empty cname. Consumers' mediasoup-client uses the cname as the msid
                // stream id, and Chromium rejects SDP with an empty msid stream id —
                // normalize to a per-participant cname so consuming works everywhere.
                let mut rtp_parameters = rtp_parameters.clone();
                if rtp_parameters
                    .rtcp
                    .cname
                    .as_deref()
                    .is_none_or(str::is_empty)
                {
                    rtp_parameters.rtcp.cname = Some(format!("ms-{participant_id}"));
                }

                let producer_id = room_manager
                    .create_producer(
                        room_id,
                        participant_id,
                        sender,
                        *transport_id,
                        *kind,
                        rtp_parameters,
                        Some(source),
                    )
                    .await?;

                metrics.inc_producers_created();
                reply.send(&ServerMessage::ProducerCreated { producer_id })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::Consume {
            producer_id,
            rtp_capabilities,
        } => {
            if let Some(room_id) = current_room_id.as_ref() {
                let consumer_info = room_manager
                    .create_consumer(
                        room_id,
                        participant_id,
                        sender,
                        *producer_id,
                        rtp_capabilities.clone(),
                        Some(sender.clone()),
                    )
                    .await?;

                metrics.inc_consumers_created();
                reply.send(&ServerMessage::ConsumerCreated {
                    consumer_id: consumer_info.id,
                    producer_id: consumer_info.producer_id,
                    kind: consumer_info.kind,
                    rtp_parameters: consumer_info.rtp_parameters,
                })?;

                // If the producer is already paused, immediately notify the consuming client
                // so it can hide the video tile instead of showing a black square.
                if consumer_info.producer_paused {
                    send_json(
                        metrics,
                        sender,
                        &ServerMessage::ProducerPaused {
                            producer_id: *producer_id,
                        },
                    )?;
                }
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::ResumeConsumer { consumer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .resume_consumer(room_id, participant_id, sender, *consumer_id)
                    .await?;

                reply.send(&ServerMessage::ConsumerResumed {
                    consumer_id: *consumer_id,
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::PauseConsumer { consumer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .pause_consumer(room_id, participant_id, sender, *consumer_id)
                    .await?;

                reply.send(&ServerMessage::ConsumerPaused {
                    consumer_id: *consumer_id,
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::CloseConsumer { consumer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .close_consumer(room_id, participant_id, sender, *consumer_id)
                    .await?;
                reply.acknowledge_control()?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::CloseProducer { producer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .close_producer(room_id, participant_id, sender, *producer_id)
                    .await?;
                reply.acknowledge_control()?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::PauseProducer { producer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .pause_producer(room_id, participant_id, sender, *producer_id)
                    .await?;
                reply.send(&ServerMessage::ProducerPaused {
                    producer_id: *producer_id,
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::ResumeProducer { producer_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .resume_producer(room_id, participant_id, sender, *producer_id)
                    .await?;
                reply.send(&ServerMessage::ProducerResumed {
                    producer_id: *producer_id,
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::RestartIce { transport_id } => {
            if let Some(room_id) = current_room_id.as_ref() {
                let ice_parameters = room_manager
                    .restart_ice(room_id, participant_id, sender, *transport_id)
                    .await?;

                reply.send(&ServerMessage::IceRestarted {
                    transport_id: *transport_id,
                    ice_parameters,
                    // Relay credentials minted at transport creation may
                    // have expired; candidate gathering needs fresh ones.
                    ice_servers: make_ice_servers(turn_config),
                })?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        ClientMessage::SetConsumerPreferredLayers {
            consumer_id,
            spatial_layer,
            temporal_layer,
        } => {
            if let Some(room_id) = current_room_id.as_ref() {
                room_manager
                    .set_preferred_layers(
                        room_id,
                        participant_id,
                        sender,
                        *consumer_id,
                        *spatial_layer,
                        *temporal_layer,
                    )
                    .await?;
                reply.acknowledge_control()?;
            } else {
                anyhow::bail!("Not in a room");
            }
        }

        _ => anyhow::bail!("Not a media command"),
    }
    Ok(())
}
