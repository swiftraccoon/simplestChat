// Signaling protocol types — mirrors src/signaling/protocol.rs exactly.
// Rust uses #[serde(tag = "type", rename_all = "camelCase")]

// --- Client → Server ---

export type ClientMessage =
  | { type: 'joinRoom'; roomId: string; participantName: string }
  | { type: 'leaveRoom' }
  | { type: 'getRouterRtpCapabilities' }
  | { type: 'createSendTransport' }
  | { type: 'createRecvTransport' }
  | { type: 'connectTransport'; transportId: string; dtlsParameters: DtlsParameters }
  | { type: 'produce'; transportId: string; kind: MediaKind; rtpParameters: RtpParameters; source?: string }
  | { type: 'consume'; producerId: string; rtpCapabilities: RtpCapabilities }
  | { type: 'resumeConsumer'; consumerId: string }
  | { type: 'pauseConsumer'; consumerId: string }
  | { type: 'closeProducer'; producerId: string }
  | { type: 'pauseProducer'; producerId: string }
  | { type: 'resumeProducer'; producerId: string }
  | { type: 'reconnect'; participantId: string; roomId: string; reconnectToken: string }
  | { type: 'restartIce'; transportId: string }
  | { type: 'setConsumerPreferredLayers'; consumerId: string; spatialLayer: number; temporalLayer?: number }
  | { type: 'chatMessage'; content: string };

// --- Server → Client ---

export type ServerMessage =
  | { type: 'roomJoined'; participantId: string; participants: ParticipantInfo[]; reconnectToken: string }
  | { type: 'error'; message: string }
  | { type: 'routerRtpCapabilities'; rtpCapabilities: RtpCapabilitiesFinalized }
  | { type: 'transportCreated'; transportId: string; iceParameters: IceParameters; iceCandidates: IceCandidate[]; dtlsParameters: DtlsParameters; iceServers?: IceServerEntry[] }
  | { type: 'transportConnected'; transportId: string }
  | { type: 'producerCreated'; producerId: string }
  | { type: 'consumerCreated'; consumerId: string; producerId: string; kind: MediaKind; rtpParameters: RtpParameters }
  | { type: 'participantJoined'; participantId: string; participantName: string }
  | { type: 'participantLeft'; participantId: string }
  | { type: 'newProducer'; participantId: string; producerId: string; kind: MediaKind; source?: string }
  | { type: 'producerClosed'; producerId: string }
  | { type: 'producerPaused'; producerId: string }
  | { type: 'producerResumed'; producerId: string }
  | { type: 'consumerResumed'; consumerId: string }
  | { type: 'consumerPaused'; consumerId: string }
  | { type: 'reconnectResult'; success: boolean; participantId: string }
  | { type: 'iceRestarted'; transportId: string; iceParameters: IceParameters }
  | { type: 'connectionStats'; availableBitrate: number | null; rtt: number | null }
  | { type: 'consumerLayersChanged'; consumerId: string; spatialLayer: number | null; temporalLayer: number | null }
  | { type: 'chatReceived'; participantId: string; participantName: string; content: string };

// --- Shared types ---

export interface ParticipantInfo {
  id: string;
  name: string;
  producers: ProducerMetadata[];
}

export interface ProducerMetadata {
  id: string;
  kind: MediaKind;
  source?: string;
}

export type MediaKind = 'audio' | 'video';

// mediasoup types — these are opaque JSON objects passed through
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type DtlsParameters = any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type RtpParameters = any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type RtpCapabilities = any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type RtpCapabilitiesFinalized = any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type IceParameters = any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type IceCandidate = any;

export interface IceServerEntry {
  urls: string[];
  username?: string;
  credential?: string;
}
