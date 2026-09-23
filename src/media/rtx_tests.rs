#![forbid(unsafe_code)]

//! Exercise the vendored worker's RTX sequence handling with real RTP packets.
//! Direct transports keep every packet inside an owned worker, without sockets,
//! browsers, a database, or production media. These are packet-accounting tests;
//! browser decode is verified separately.

use mediasoup::prelude::*;
use mediasoup::producer::{DirectProducer, ProducerStat};
use std::future::Future;
use std::num::NonZeroU32;
use std::time::Duration;

const MEDIA_SSRC: u32 = 123_456;
const RTX_SSRC: u32 = 654_321;
const MEDIA_PT: u8 = 96;
const RTX_PT: u8 = 97;

async fn with_producer<F, Fut>(test: F)
where
    F: FnOnce(DirectProducer) -> Fut,
    Fut: Future<Output = ()>,
{
    tokio::time::timeout(Duration::from_secs(10), async {
        let manager = WorkerManager::new();
        let worker = manager
            .create_worker(WorkerSettings::default())
            .await
            .unwrap();
        let router = worker
            .create_router(RouterOptions::new(
                super::RouterConfig::default().media_codecs,
            ))
            .await
            .unwrap();
        let transport = router
            .create_direct_transport(DirectTransportOptions::default())
            .await
            .unwrap();
        let clock_rate = NonZeroU32::new(90_000).unwrap();
        let producer = transport
            .produce(ProducerOptions::new(
                MediaKind::Video,
                RtpParameters {
                    codecs: vec![
                        RtpCodecParameters::Video {
                            mime_type: MimeTypeVideo::Vp8,
                            payload_type: MEDIA_PT,
                            clock_rate,
                            parameters: RtpCodecParametersParameters::default(),
                            rtcp_feedback: vec![RtcpFeedback::Nack, RtcpFeedback::NackPli],
                        },
                        RtpCodecParameters::Video {
                            mime_type: MimeTypeVideo::Rtx,
                            payload_type: RTX_PT,
                            clock_rate,
                            parameters: [("apt", MEDIA_PT.into())].into(),
                            rtcp_feedback: vec![],
                        },
                    ],
                    encodings: vec![RtpEncodingParameters {
                        ssrc: Some(MEDIA_SSRC),
                        rtx: Some(RtpEncodingParametersRtx { ssrc: RTX_SSRC }),
                        ..RtpEncodingParameters::default()
                    }],
                    ..RtpParameters::default()
                },
            ))
            .await
            .unwrap();
        let Producer::Direct(producer) = producer else {
            panic!("direct transport must create a direct producer");
        };
        test(producer).await;
        // Dropping this owned graph releases the direct transport and worker.
    })
    .await
    .expect("native RTX fixture exceeded its deadline");
}

/// RTP v2 with a minimal VP8 payload descriptor and a non-keyframe frame tag.
/// RTX prepends the original sequence number, as specified by RFC 4588.
fn packet(sequence: u16, original_sequence: Option<u16>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16);
    bytes.extend_from_slice(&[0x80, original_sequence.map_or(MEDIA_PT, |_| RTX_PT)]);
    bytes.extend_from_slice(&sequence.to_be_bytes());
    bytes.extend_from_slice(
        &(u32::from(original_sequence.unwrap_or(sequence)) * 3000).to_be_bytes(),
    );
    bytes.extend_from_slice(
        &original_sequence
            .map_or(MEDIA_SSRC, |_| RTX_SSRC)
            .to_be_bytes(),
    );
    if let Some(original) = original_sequence {
        bytes.extend_from_slice(&original.to_be_bytes());
    }
    bytes.extend_from_slice(&[0x10, 0x01]);
    bytes
}

async fn stats(producer: &DirectProducer) -> ProducerStat {
    // Notifications and requests share the ordered worker channel: this request
    // observes the preceding sends without guessing how long packet processing takes.
    let mut stats = Producer::Direct(producer.clone())
        .get_stats()
        .await
        .unwrap();
    assert_eq!(stats.len(), 1);
    let stats = stats.pop().unwrap();
    assert_eq!(stats.ssrc, MEDIA_SSRC);
    stats
}

#[tokio::test]
async fn stale_rtx_probes_do_not_discard_media_or_reset_its_sequence() {
    for first in [1_u16, 65_500] {
        with_producer(|producer| async move {
            for offset in 0..=2000 {
                producer
                    .send(packet(first.wrapping_add(offset), None))
                    .unwrap();
            }
            let before = stats(&producer).await;
            assert_eq!(before.packet_count, 2001);

            // Repeated old payloads reproduce browser bandwidth probing. Two
            // consecutive old payloads must not look like a primary RTP restart.
            for sequence in 1000..2000 {
                producer.send(packet(sequence, Some(first))).unwrap();
            }
            producer
                .send(packet(2000, Some(first.wrapping_add(1))))
                .unwrap();
            let after = stats(&producer).await;
            assert_eq!(after.packet_count, before.packet_count);
            assert_eq!(after.packets_discarded, 0);
            assert_eq!(after.packets_repaired, 0);
            assert_eq!(after.nack_packet_count, before.nack_packet_count);
            assert_eq!(after.pli_count, before.pli_count);

            producer
                .send(packet(first.wrapping_add(2001), None))
                .unwrap();
            let next = stats(&producer).await;
            assert_eq!(next.packet_count, 2002);
            assert_eq!(next.nack_packet_count, before.nack_packet_count);
            assert_eq!(next.pli_count, before.pli_count);
        })
        .await;
    }
}

#[tokio::test]
async fn requested_rtx_recovers_old_missing_packets_once_including_across_wrap() {
    for first in [1_u16, 65_500] {
        with_producer(|producer| async move {
            producer.send(packet(first, None)).unwrap();
            for offset in 2..=2000 {
                producer
                    .send(packet(first.wrapping_add(offset), None))
                    .unwrap();
            }
            let before = loop {
                let current = stats(&producer).await;
                if current.nack_packet_count > 0 {
                    break current;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            };
            assert_eq!(before.packet_count, 2000);
            producer
                .send(packet(1000, Some(first.wrapping_add(1))))
                .unwrap();
            let recovered = stats(&producer).await;
            assert_eq!(recovered.packet_count, 2001);
            assert_eq!(recovered.packets_repaired, 1);
            assert_eq!(recovered.packets_discarded, 0);

            producer
                .send(packet(1001, Some(first.wrapping_add(1))))
                .unwrap();
            let duplicate = stats(&producer).await;
            assert_eq!(duplicate.packet_count, 2001);
            assert_eq!(duplicate.packets_repaired, 1);
        })
        .await;
    }
}

#[tokio::test]
async fn rtx_can_deliver_a_new_packet_before_the_primary_stream() {
    with_producer(|producer| async move {
        producer.send(packet(65_535, None)).unwrap();
        producer.send(packet(1000, Some(0))).unwrap();
        let recovered = stats(&producer).await;
        assert_eq!(recovered.packet_count, 2);
        assert_eq!(recovered.packets_repaired, 1);
        assert_eq!(recovered.packets_discarded, 0);
        producer.send(packet(1, None)).unwrap();
        let next = stats(&producer).await;
        assert_eq!(next.packet_count, 3);
        assert_eq!(next.nack_packet_count, 0);
    })
    .await;
}

#[tokio::test]
async fn rtx_does_not_bypass_validation_of_a_large_forward_jump() {
    with_producer(|producer| async move {
        producer.send(packet(1, None)).unwrap();
        producer.send(packet(1000, Some(5000))).unwrap();
        let rejected = stats(&producer).await;
        assert_eq!(rejected.packet_count, 1);
        assert_eq!(rejected.packets_discarded, 1);
        assert_eq!(rejected.packets_repaired, 0);
        producer.send(packet(2, None)).unwrap();
        let next = stats(&producer).await;
        assert_eq!(next.packet_count, 2);
        assert_eq!(next.nack_packet_count, 0);
    })
    .await;
}

#[tokio::test]
async fn primary_rtp_still_rejects_packets_outside_its_misorder_window() {
    with_producer(|producer| async move {
        for sequence in 1..=2001 {
            producer.send(packet(sequence, None)).unwrap();
        }
        producer.send(packet(1, None)).unwrap();
        let rejected = stats(&producer).await;
        assert_eq!(rejected.packet_count, 2001);
        assert_eq!(rejected.packets_discarded, 1);
        producer.send(packet(2002, None)).unwrap();
        assert_eq!(stats(&producer).await.packet_count, 2002);
    })
    .await;
}
