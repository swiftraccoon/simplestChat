use super::*;
use futures_util::FutureExt;
use mediasoup_types::data_structures::DtlsState;
use simplestChat::media::config::{RouterConfig, WebRtcTransportConfig};
use simplestChat::media::transport_manager::TransportManager;
use simplestChat::media::types::MediaError;
use std::net::{IpAddr, Ipv4Addr};
use std::panic::AssertUnwindSafe;
use tokio::task::JoinSet;

const PUBLISHER: &str = "owned-keyframe-publisher";
const RECEIVER: &str = "owned-keyframe-receiver";
const OBSERVATION: Duration = Duration::from_secs(8);
const RESUME_PHASE: Duration = Duration::from_millis(1500);

/// Own every fixture task and native handle outside the timed body. A failed
/// assertion, IPC error, or canceled body must still close both local peers and
/// remove both native participants. Dropping the JoinSet also aborts its task.
struct Fixture {
    manager: TransportManager,
    publisher: Arc<Mutex<WebRtcSession>>,
    receiver: Arc<Mutex<WebRtcSession>>,
    publisher_metrics: Arc<MetricsCollector>,
    receiver_metrics: Arc<MetricsCollector>,
    publisher_origin: Instant,
    receiver_origin: Instant,
    tasks: JoinSet<()>,
    worker: Option<Worker>,
    router: Option<Router>,
    server: Option<WebRtcServer>,
}

impl Fixture {
    fn new() -> Self {
        let publisher_origin = Instant::now();
        let publisher_metrics = Arc::new(MetricsCollector::new(PUBLISHER.into()));
        let receiver_origin = Instant::now();
        let receiver_metrics = Arc::new(MetricsCollector::new(RECEIVER.into()));
        publisher_metrics.enable_diagnostics();
        receiver_metrics.enable_diagnostics();
        publisher_metrics.begin_connection_attempt();
        receiver_metrics.begin_connection_attempt();
        Self {
            manager: TransportManager::new(),
            publisher: Arc::new(Mutex::new(WebRtcSession::new(
                PUBLISHER.into(),
                publisher_metrics.clone(),
            ))),
            receiver: Arc::new(Mutex::new(WebRtcSession::new(
                RECEIVER.into(),
                receiver_metrics.clone(),
            ))),
            publisher_metrics,
            receiver_metrics,
            publisher_origin,
            receiver_origin,
            tasks: JoinSet::new(),
            worker: None,
            router: None,
            server: None,
        }
    }

    async fn run(&mut self, feedback_enabled: bool) -> Result<CaseEvidence> {
        let workers = WorkerManager::new();
        let worker = workers.create_worker(WorkerSettings::default()).await?;
        self.worker = Some(worker.clone());
        let router = worker
            .create_router(RouterConfig::default().to_router_options())
            .await?;
        self.router = Some(router.clone());
        // An unrelated process winning this bind is a fixture failure. Do not
        // retry against a different port or connect to an existing service.
        let reservation = std::net::UdpSocket::bind("127.0.0.1:0")?;
        let port = reservation.local_addr()?.port();
        drop(reservation);
        let server = worker
            .create_webrtc_server(WebRtcServerOptions::new(WebRtcServerListenInfos::new(
                ListenInfo {
                    protocol: Protocol::Udp,
                    ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                    announced_address: None,
                    expose_internal_ip: false,
                    port: Some(port),
                    port_range: None,
                    flags: None,
                    send_buffer_size: None,
                    recv_buffer_size: None,
                },
            )))
            .await?;
        self.server = Some(server.clone());
        let configuration = WebRtcTransportConfig::default();
        let send = self
            .manager
            .create_send_transport(PUBLISHER.into(), &router, server.clone(), &configuration)
            .await?;
        let local_dtls = self
            .publisher
            .lock()
            .await
            .create_send_transport(
                send.id.clone(),
                send.ice_parameters,
                send.ice_candidates,
                send.dtls_parameters,
            )
            .await?;
        self.manager
            .connect_transport(PUBLISHER, &send.id, local_dtls)
            .await?;

        let captured = Arc::new(std::sync::Mutex::new(None));
        let capture_handler = server.on_new_webrtc_transport({
            let captured = captured.clone();
            move |transport| *captured.lock().unwrap() = Some(transport.clone())
        });
        let receive = self
            .manager
            .create_recv_transport(RECEIVER.into(), &router, server, &configuration)
            .await?;
        let native_receive = captured
            .lock()
            .unwrap()
            .take()
            .context("Owned native receive transport was not captured")?;
        drop(capture_handler);
        let local_dtls = self
            .receiver
            .lock()
            .await
            .create_recv_transport(
                receive.id.clone(),
                receive.ice_parameters,
                receive.ice_candidates,
                receive.dtls_parameters,
            )
            .await?;
        self.manager
            .connect_transport(RECEIVER, &receive.id, local_dtls)
            .await?;

        let configuration = MediaConfig::video_only();
        let (_, video_ssrc) = self.publisher.lock().await.send_ssrcs().await?;
        let mut parameters = extract_rtp_parameters(
            MediaKind::Video,
            video_ssrc,
            configuration.video_bitrate_kbps,
        );
        // Only the producer's advertised feedback differs between controls.
        // Both retain the real peer, SFU, generator and five-second cadence.
        for codec in &mut parameters.codecs {
            if let RtpCodecParameters::Video { rtcp_feedback, .. } = codec {
                if feedback_enabled {
                    anyhow::ensure!(
                        rtcp_feedback.contains(&RtcpFeedback::NackPli)
                            && rtcp_feedback.contains(&RtcpFeedback::CcmFir),
                        "Positive control must advertise supported keyframe feedback"
                    );
                } else {
                    rtcp_feedback.clear();
                }
            }
        }
        let producer = self
            .manager
            .create_producer(PUBLISHER, MediaKind::Video, parameters, AppData::default())
            .await?;
        let capabilities: RtpCapabilities =
            serde_json::from_value(serde_json::to_value(router.rtp_capabilities())?)?;
        let consumer = self
            .manager
            .create_consumer(
                RECEIVER,
                producer.id(),
                capabilities,
                AppData::default(),
                None,
                None,
            )
            .await?;
        let producer_id = producer.id().to_string();
        let consumer_id = consumer.id().to_string();
        let consumer_ssrc = consumer
            .rtp_parameters()
            .encodings
            .first()
            .and_then(|encoding| encoding.ssrc)
            .context("Owned video consumer has no SSRC")?;
        self.publisher_metrics.record_publisher(&producer_id, false);
        self.receiver_metrics.subscribe(&producer_id, false);
        self.receiver_metrics
            .record_consumer(&consumer_id, &producer_id, consumer_ssrc);
        {
            let mut receiver = self.receiver.lock().await;
            receiver.record_consumer(producer_id, MediaKind::Video, consumer.rtp_parameters())?;
            receiver.renegotiate_consumers().await?;
        }
        self.publisher.lock().await.wait_send_connected().await?;
        while native_receive.dtls_state() != DtlsState::Connected {
            sleep(Duration::from_millis(10)).await;
        }
        // Match the existing maximum SSRC registration allowance, before any
        // video is generated. This is not part of the receiver observation.
        sleep(Duration::from_millis(200)).await;
        self.tasks.spawn(send_real_media_loop(
            self.publisher.clone(),
            MediaGenerator::new(configuration.clone()),
            configuration,
            self.publisher_metrics.clone(),
            PUBLISHER.into(),
            1,
        ));
        let initial = loop {
            self.ensure_sender_running()?;
            if let Some(event) =
                diagnostic_events(&self.publisher_metrics)
                    .into_iter()
                    .find(|event| {
                        event.kind == "video-keyframe-queued"
                            && event.details["frameIndex"].as_u64() == Some(0)
                    })
            {
                break self.publisher_origin + Duration::from_millis(event.elapsed_ms);
            }
            sleep(Duration::from_millis(10)).await;
        };
        while producer
            .get_stats()
            .await?
            .iter()
            .all(|stat| stat.packet_count == 0)
        {
            self.ensure_sender_running()?;
            sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep_until((initial + RESUME_PHASE).into()).await;
        anyhow::ensure!(
            initial.elapsed() < Duration::from_secs(2),
            "Fixture missed its predeclared receiver resume phase"
        );
        anyhow::ensure!(
            self.receiver_metrics
                .generate_report()
                .total_packets_received
                == 0,
            "A paused consumer received RTP before its explicit resume"
        );
        let resume = Instant::now();
        self.receiver_metrics
            .diagnostic_event("fixture-resume", serde_json::json!({}));
        self.manager.resume_consumer(RECEIVER, &consumer_id).await?;
        let end = resume + OBSERVATION;
        let mut packets_by_second = [0_u64; 8];
        let mut previous_packets = 0;
        while Instant::now() < end {
            self.ensure_sender_running()?;
            let packets = self
                .receiver_metrics
                .generate_report()
                .total_packets_received;
            let second = resume.elapsed().as_secs() as usize;
            if let Some(bucket) = packets_by_second.get_mut(second) {
                *bucket += packets.saturating_sub(previous_packets);
            }
            previous_packets = packets;
            tokio::time::sleep_until((Instant::now() + Duration::from_millis(10)).min(end).into())
                .await;
        }
        // Do not leave the first case generating traffic during the second.
        // Native/peer handles remain owned for unconditional outer cleanup.
        self.tasks.shutdown().await;
        let receiver = self.receiver_metrics.generate_report();
        let publisher = self.publisher_metrics.generate_report();
        anyhow::ensure!(
            receiver.errors.is_empty() && publisher.errors.is_empty(),
            "Native fixture recorded an RTP or peer error"
        );
        let receiver_events = diagnostic_events(&self.receiver_metrics);
        let first = receiver_events
            .iter()
            .find(|event| event.kind == "track-first-rtp")
            .context("Receiver never observed actual RTP")?;
        anyhow::ensure!(
            first.details["ssrc"].as_u64() == Some(u64::from(consumer_ssrc)),
            "First RTP did not belong to the owned video consumer"
        );
        let first_instant = self.receiver_origin + Duration::from_millis(first.elapsed_ms);
        let resume_event = receiver_events
            .iter()
            .find(|event| event.kind == "fixture-resume")
            .context("Receiver resume marker missing")?;
        let first_rtp_ms = first
            .elapsed_ms
            .checked_sub(resume_event.elapsed_ms)
            .context("RTP preceded the receiver resume marker")?;
        let first_positive = packets_by_second
            .iter()
            .position(|packets| *packets > 0)
            .context("No receiver observation bucket contained RTP")?;
        anyhow::ensure!(
            packets_by_second[first_positive..]
                .iter()
                .all(|packets| *packets > 0),
            "RTP stopped during the fixed observation after startup"
        );
        let events = diagnostic_events(&self.publisher_metrics);
        let periodic = events
            .iter()
            .find(|event| {
                event.kind == "video-keyframe-queued"
                    && event.details["frameIndex"].as_u64() == Some(150)
            })
            .context("Unchanged five-second periodic keyframe was not observed")?;
        let periodic_instant = self.publisher_origin + Duration::from_millis(periodic.elapsed_ms);
        let periodic_after_resume_ms = periodic_instant
            .saturating_duration_since(resume)
            .as_millis() as u64;
        let requested_frames: Vec<_> = events
            .iter()
            .filter(|event| {
                event.kind == "video-keyframe-queued"
                    && event.details["requested"].as_bool() == Some(true)
            })
            .collect();
        let request_events = events
            .iter()
            .filter(|event| event.kind == "keyframe-requested")
            .count();
        let native_feedback: u64 = producer
            .get_stats()
            .await?
            .iter()
            .map(|stat| stat.pli_count + stat.fir_count)
            .sum();
        let evidence = CaseEvidence {
            feedback_enabled,
            resume_after_initial_ms: resume.duration_since(initial).as_millis() as u64,
            first_rtp_ms,
            periodic_after_resume_ms,
            request_events,
            requested_keyframes: requested_frames.len(),
            native_feedback,
            packets_by_second,
        };
        println!("keyframe-startup {}", serde_json::to_string(&evidence)?);
        if feedback_enabled {
            anyhow::ensure!(
                request_events > 0 && native_feedback > 0,
                "Positive control did not observe native keyframe feedback"
            );
            anyhow::ensure!(
                requested_frames.iter().any(|event| {
                    event.details["frameIndex"]
                        .as_u64()
                        .is_some_and(|index| index > 0 && index < 150)
                        && event.details["ssrc"].as_u64() == Some(u64::from(video_ssrc))
                }),
                "Feedback did not queue a keyframe before the periodic frame"
            );
            anyhow::ensure!(
                first_rtp_ms < 2000 && first_instant < periodic_instant,
                "Supported feedback did not unblock actual RTP before the periodic keyframe"
            );
        } else {
            anyhow::ensure!(
                request_events == 0 && requested_frames.is_empty() && native_feedback == 0,
                "Empty-feedback negative control unexpectedly requested a keyframe"
            );
            anyhow::ensure!(
                first_rtp_ms >= 2500
                    && first_rtp_ms + 10 >= periodic_after_resume_ms
                    && first_rtp_ms <= periodic_after_resume_ms + 1000,
                "Empty-feedback control did not isolate the periodic-keyframe startup wait"
            );
        }
        Ok(evidence)
    }

    fn ensure_sender_running(&mut self) -> Result<()> {
        if let Some(completion) = self.tasks.try_join_next() {
            completion.context("Owned media sender panicked")?;
            anyhow::bail!("Owned media sender stopped before observation completed");
        }
        Ok(())
    }

    async fn cleanup(&mut self) -> Result<()> {
        self.tasks.shutdown().await;
        self.publisher_metrics.end_session();
        self.receiver_metrics.end_session();
        let publisher = self.publisher.lock().await.close().await;
        let receiver = self.receiver.lock().await.close().await;
        let remove_publisher = self.manager.remove_participant(PUBLISHER).await;
        let remove_receiver = self.manager.remove_participant(RECEIVER).await;
        drop(self.server.take());
        drop(self.router.take());
        drop(self.worker.take());
        publisher?;
        receiver?;
        for result in [remove_publisher, remove_receiver] {
            match result {
                Ok(()) | Err(MediaError::ParticipantNotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

fn diagnostic_events(metrics: &MetricsCollector) -> Vec<metrics::DiagnosticEntry> {
    metrics
        .generate_report()
        .diagnostics
        .expect("Native startup fixture enables diagnostics")
        .events
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CaseEvidence {
    feedback_enabled: bool,
    resume_after_initial_ms: u64,
    first_rtp_ms: u64,
    periodic_after_resume_ms: u64,
    request_events: usize,
    requested_keyframes: usize,
    native_feedback: u64,
    packets_by_second: [u64; 8],
}

/// Negative and positive controls are both required in one invocation. This
/// checks forwarding startup, not decodability or production resource usage.
/// Neither receiver duration nor the generator's periodic cadence is relaxed.
#[tokio::test]
async fn native_feedback_unblocks_video_before_the_next_periodic_keyframe() {
    let mut baseline = Fixture::new();
    let mut supported = Fixture::new();
    // 39 seconds for both bodies plus two three-second cleanup budgets is the
    // complete 45-second fixture deadline, including failed/timeout cases.
    let outcome = AssertUnwindSafe(tokio::time::timeout(Duration::from_secs(39), async {
        let baseline = baseline.run(false).await?;
        let supported = supported.run(true).await?;
        anyhow::ensure!(
            baseline.first_rtp_ms >= supported.first_rtp_ms + 1000,
            "Paired controls did not distinguish keyframe-feedback startup"
        );
        Ok::<_, anyhow::Error>(())
    }))
    .catch_unwind()
    .await;
    // Evaluate both cleanups before propagating any earlier failure.
    let baseline_cleanup = tokio::time::timeout(Duration::from_secs(3), baseline.cleanup()).await;
    let supported_cleanup = tokio::time::timeout(Duration::from_secs(3), supported.cleanup()).await;
    match outcome {
        Ok(result) => result
            .expect("Paired native keyframe fixture timed out")
            .expect("Paired native keyframe fixture failed"),
        Err(panic) => std::panic::resume_unwind(panic),
    }
    baseline_cleanup
        .expect("Baseline keyframe fixture cleanup timed out")
        .expect("Baseline keyframe fixture cleanup failed");
    supported_cleanup
        .expect("Supported keyframe fixture cleanup timed out")
        .expect("Supported keyframe fixture cleanup failed");
}
