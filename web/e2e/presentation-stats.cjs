/** Passive public estimate/native counters. Functions are self-contained for page evaluation. */
function installPresentationSignalStats() {
  window.__presentationReceiveEstimate = null;
  if (!window.WebSocket) return;
  window.WebSocket = new Proxy(window.WebSocket, {
    construct(target, args, newTarget) {
      const socket = Reflect.construct(target, args, newTarget);
      socket.addEventListener('message', (event) => {
        // Retain only the existing public numeric receive estimate. Never
        // retain a signal frame, token, participant id, chat text or URL.
        if (typeof event.data !== 'string' || event.data.length > 1024) return;
        try {
          const message = JSON.parse(event.data);
          if (
            message?.type === 'connectionStats' &&
            Number.isFinite(message.availableBitrate) &&
            message.availableBitrate >= 0 &&
            message.availableBitrate <= 1_000_000_000
          )
            window.__presentationReceiveEstimate = {
              availableBitrate: message.availableBitrate,
              observedAt: performance.now(),
            };
        } catch {
          // Diagnostic parsing never interferes with native message delivery.
        }
      });
      return socket;
    },
  });
}

async function presentationReceiverStats() {
  const screen = document
    .querySelector('.video-tile.screen-share:not(.local) video')
    ?.srcObject?.getVideoTracks()[0];
  const camera = document
    .querySelector('.video-tile:not(.screen-share):not(.local) video')
    ?.srcObject?.getVideoTracks()[0];
  const finite = (value) => (Number.isFinite(value) && value >= 0 ? value : null);
  const lossCount = (value) => (Number.isSafeInteger(value) && value >= 0 ? value : null);
  const result = {
    screenFrames: 0,
    cameraFrames: 0,
    audioPackets: 0,
    audioSamples: null,
    concealedSamples: null,
    availableIncomingBitrate: null,
    serverAvailableBitrate: window.__presentationReceiveEstimate?.availableBitrate ?? null,
    serverEstimateAgeMs: window.__presentationReceiveEstimate
      ? Math.max(0, performance.now() - window.__presentationReceiveEstimate.observedAt)
      : null,
    screenInbound: [],
    remoteDescriptions: [],
    localDescriptions: [],
    probator: {
      receiverIdentified: false,
      packetsReceived: null,
      bytesReceived: null,
      packetsLost: null,
    },
  };
  const describe = (description) => {
    const feedback = ['transport-cc', 'goog-remb', 'nack', 'nack pli', 'ccm fir'];
    const extensionNames = {
      'http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01':
        'transport-wide-cc',
      'http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time': 'abs-send-time',
      'urn:ietf:params:rtp-hdrext:sdes:mid': 'mid',
    };
    const foundFeedback = new Set(),
      foundExtensions = new Set();
    const available = typeof description?.sdp === 'string' && description.sdp.length <= 262144;
    let probator = false;
    if (available)
      for (const line of description.sdp.split(/\r?\n/)) {
        const value = /^a=rtcp-fb:(?:\*|\d{1,3}) ([a-z-]+(?: [a-z-]+)?)$/.exec(line)?.[1];
        if (feedback.includes(value)) foundFeedback.add(value);
        const uri =
          /^a=extmap:\d+(?:\/(?:sendonly|recvonly|sendrecv|inactive))? (\S+)(?: .*)?$/.exec(
            line,
          )?.[1];
        if (Object.hasOwn(extensionNames, uri)) foundExtensions.add(extensionNames[uri]);
        if (line === 'a=mid:probator') probator = true;
      }
    return {
      available,
      type: ['offer', 'answer', 'pranswer', 'rollback'].includes(description?.type)
        ? description.type
        : null,
      feedback: feedback.filter((name) => foundFeedback.has(name)),
      headerExtensions: ['transport-wide-cc', 'abs-send-time', 'mid'].filter((name) =>
        foundExtensions.has(name),
      ),
      probator: available ? probator : null,
    };
  };
  for (const peer of window.__communityPeers) {
    if (peer.connectionState !== 'connected') continue;
    if (result.remoteDescriptions.length < 4)
      result.remoteDescriptions.push(describe(peer.remoteDescription));
    if (result.localDescriptions.length < 4)
      result.localDescriptions.push(describe(peer.localDescription));
    const receivers = peer.getReceivers();
    if (receivers.some((receiver) => receiver.track?.id === 'probator'))
      result.probator.receiverIdentified = true;
    if (receivers.length) {
      // This optional diagnostic must not replace the receiver counters used
      // for assertions when an engine omits transport-wide stats.
      const stats = await peer.getStats().catch(() => new Map());
      const selected = new Set(
        Array.from(stats.values())
          .filter((row) => row.type === 'transport')
          .map((row) => row.selectedCandidatePairId),
      );
      for (const row of stats.values()) {
        // The pinned mediasoup client uses this fixed probation receiver/MID.
        // Match internally only; never retain SDP, native identifiers or SSRCs.
        if (
          row.type === 'inbound-rtp' &&
          (row.ssrc === 1234 || row.mid === 'probator' || row.trackIdentifier === 'probator')
        ) {
          result.probator.receiverIdentified = true;
          for (const name of ['packetsReceived', 'bytesReceived', 'packetsLost'])
            if ((name === 'packetsLost' ? lossCount(row[name]) : finite(row[name])) !== null)
              result.probator[name] = (result.probator[name] ?? 0) + row[name];
        }
        if (
          row.type === 'candidate-pair' &&
          row.state === 'succeeded' &&
          (row.selected || row.nominated || selected.has(row.id)) &&
          finite(row.availableIncomingBitrate) !== null
        )
          result.availableIncomingBitrate = Math.max(
            result.availableIncomingBitrate ?? 0,
            row.availableIncomingBitrate,
          );
      }
    }
    for (const receiver of receivers) {
      const stats = await receiver.getStats();
      for (const row of stats.values()) {
        if (row.type !== 'inbound-rtp') continue;
        if (receiver.track.kind === 'video' && typeof row.framesDecoded === 'number') {
          if (receiver.track === screen) {
            result.screenFrames += row.framesDecoded;
            if (result.screenInbound.length < 4)
              result.screenInbound.push({
                width: finite(row.frameWidth),
                height: finite(row.frameHeight),
                // Only exact dimensions identify one of this scenario's known
                // simulcast layers. Unsupported native fields remain null.
                spatialLayer:
                  { '480x270': 0, '960x540': 1, '1920x1080': 2 }[
                    `${row.frameWidth}x${row.frameHeight}`
                  ] ?? null,
                framesDecoded: finite(row.framesDecoded),
                framesPerSecond: finite(row.framesPerSecond),
                bytesReceived: finite(row.bytesReceived),
                packetsReceived: finite(row.packetsReceived),
                packetsLost: lossCount(row.packetsLost),
              });
          }
          if (receiver.track === camera) result.cameraFrames += row.framesDecoded;
        }
        if (receiver.track.kind === 'audio') {
          result.audioPackets += row.packetsReceived || 0;
          const samples = row.totalSamplesReceived ?? row.jitterBufferEmittedCount;
          if (typeof samples === 'number')
            result.audioSamples = (result.audioSamples ?? 0) + samples;
          if (typeof row.concealedSamples === 'number')
            result.concealedSamples = (result.concealedSamples ?? 0) + row.concealedSamples;
        }
      }
    }
  }
  return result;
}

async function presentationSenderStats() {
  const finite = (value) => (Number.isFinite(value) && value >= 0 ? value : null);
  const result = {
    screen: [],
    camera: [],
    screenCodec: null,
    screenSpatialStreams: 0,
    screenOutbound: [],
    captures: window.__presentation.captures,
    displays: window.__presentation.displays,
  };
  for (const peer of window.__communityPeers)
    for (const sender of peer.getSenders()) {
      if (sender.track?.kind !== 'video') continue;
      const source = window.__presentation.tracks.includes(sender.track) ? 'screen' : 'camera';
      const parameters = sender.getParameters();
      if (source === 'screen') {
        result.screenCodec =
          parameters.codecs.find((codec) => !codec.mimeType.toLowerCase().endsWith('/rtx'))
            ?.mimeType || null;
        const stats = await sender.getStats();
        for (const row of stats.values())
          if (row.type === 'outbound-rtp') {
            if (row.framesEncoded > 0) result.screenSpatialStreams++;
            if (result.screenOutbound.length < 4) {
              const index = parameters.encodings.findIndex(
                (encoding) => encoding.rid && encoding.rid === row.rid,
              );
              result.screenOutbound.push({
                encodingIndex: index >= 0 ? index : null,
                width: finite(row.frameWidth),
                height: finite(row.frameHeight),
                framesEncoded: finite(row.framesEncoded),
                framesPerSecond: finite(row.framesPerSecond),
                bytesSent: finite(row.bytesSent),
                packetsSent: finite(row.packetsSent),
                targetBitrate: finite(row.targetBitrate),
                qualityLimitationReason: ['none', 'cpu', 'bandwidth', 'other'].includes(
                  row.qualityLimitationReason,
                )
                  ? row.qualityLimitationReason
                  : null,
              });
            }
          }
      }
      result[source] = parameters.encodings.map((encoding) => ({
        active: encoding.active !== false,
        maxBitrate: encoding.maxBitrate ?? null,
        maxFramerate: encoding.maxFramerate ?? null,
        scalabilityMode: encoding.scalabilityMode ?? null,
      }));
    }
  return result;
}

module.exports = {
  installPresentationSignalStats,
  presentationReceiverStats,
  presentationSenderStats,
};
