/** Failure-only, read-only snapshot; self-contained for page.evaluate(). */
async function collectPeerDiagnostics() {
  const choose = (value, allowed) => (allowed.includes(value) ? value : null);
  const token = (value) =>
    typeof value === 'string' && /^[A-Za-z0-9_./+-]{1,128}$/.test(value) ? value : null;
  const directions = ['sendrecv', 'sendonly', 'recvonly', 'inactive'];
  let peers;
  try {
    peers = window.__communityPeers;
  } catch {
    return [{ error: 'Peer registry unavailable' }];
  }
  if (!Array.isArray(peers)) return [{ error: 'Peer registry unavailable' }];
  const omittedPeers = Math.max(0, peers.length - 16);
  return Promise.all(
    peers.slice(-16).map(async (peer, index) => {
      const errors = [],
        truncated = [];
      const note = (list, value) => {
        if (list.length < 32 && !list.includes(value)) list.push(value);
      };
      const read = (object, key) => {
        try {
          return object?.[key];
        } catch {
          note(errors, `${key} unavailable`);
          return undefined;
        }
      };
      const finish = (result) => {
        if (index === 0 && omittedPeers) {
          result.omittedPeers = omittedPeers;
          note(truncated, 'peers');
        }
        if (errors.length) result.errors = errors;
        if (truncated.length) result.truncated = truncated;
        return result;
      };
      const result = {
        connectionState: choose(read(peer, 'connectionState'), [
          'new',
          'connecting',
          'connected',
          'disconnected',
          'failed',
          'closed',
        ]),
        iceConnectionState: choose(read(peer, 'iceConnectionState'), [
          'new',
          'checking',
          'connected',
          'completed',
          'disconnected',
          'failed',
          'closed',
        ]),
        signalingState: choose(read(peer, 'signalingState'), [
          'stable',
          'have-local-offer',
          'have-remote-offer',
          'have-local-pranswer',
          'have-remote-pranswer',
          'closed',
        ]),
        iceGatheringState: choose(read(peer, 'iceGatheringState'), [
          'new',
          'gathering',
          'complete',
        ]),
      };
      try {
        const trace = window.__communityPeerEvents?.get(peer);
        if (trace) {
          // Tracing stores only sanitized fields; copy now, before asynchronous stats.
          result.iceEvents = trace.events.slice(-128).map((event) => ({
            ...event,
            ...(event.candidate ? { candidate: { ...event.candidate } } : {}),
          }));
          result.iceEventsDropped = trace.dropped + Math.max(0, trace.events.length - 128);
          if (Number.isFinite(trace.startedAt))
            result.iceEventsObservedAtMs = Math.max(0, performance.now() - trace.startedAt);
        }
      } catch {
        note(errors, 'ICE event history unavailable');
      }
      if (result.connectionState === 'closed' || result.signalingState === 'closed')
        return finish(result);

      function summarizeDescription(description) {
        if (!description) return null;
        const type = choose(read(description, 'type'), ['offer', 'answer', 'pranswer', 'rollback']);
        const sdp = read(description, 'sdp');
        if (typeof sdp !== 'string') return { type, error: 'SDP unavailable' };
        if (sdp.length > 65536) {
          note(truncated, 'sdp');
          return { type, error: 'SDP size limit exceeded' };
        }
        const summary = { type, bundleMids: [], iceLite: false, endOfCandidates: false, media: [] };
        let current = null,
          sessionDirection = null,
          sessionSetup = null;
        for (const line of sdp.split(/\r?\n/)) {
          if (line.startsWith('m=')) {
            if (summary.media.length >= 32) {
              note(truncated, 'media');
              break;
            }
            const [kind, port, protocol] = line.slice(2).trim().split(/\s+/);
            current = {
              kind: choose(kind, ['audio', 'video', 'application']),
              port:
                /^\d+(?:\/\d+)?$/.test(port) && Number(port.split('/')[0]) <= 65535
                  ? Number(port.split('/')[0])
                  : null,
              protocol: token(protocol),
              direction: sessionDirection,
              mid: null,
              codecs: [],
              candidates: { count: 0, byType: {}, byProtocol: {} },
              endOfCandidates: false,
              bundleOnly: false,
              rtcpMux: false,
              setup: sessionSetup,
            };
            summary.media.push(current);
          } else if (line.startsWith('a=group:BUNDLE ')) {
            const mids = line.slice(15).trim().split(/\s+/).map(token).filter(Boolean);
            summary.bundleMids = mids.slice(0, 32);
            if (mids.length > 32) note(truncated, 'bundleMids');
          } else if (line === 'a=ice-lite') summary.iceLite = true;
          else if (line === 'a=end-of-candidates') (current ?? summary).endOfCandidates = true;
          else if (directions.includes(line.slice(2)) && line.startsWith('a=')) {
            if (current) current.direction = line.slice(2);
            else sessionDirection = line.slice(2);
          } else if (line.startsWith('a=setup:')) {
            const setup = choose(line.slice(8), ['active', 'passive', 'actpass', 'holdconn']);
            if (current) current.setup = setup;
            else sessionSetup = setup;
          } else if (current) {
            if (line === 'a=bundle-only') current.bundleOnly = true;
            else if (line === 'a=rtcp-mux') current.rtcpMux = true;
            else if (line.startsWith('a=mid:')) current.mid = token(line.slice(6));
            else if (line.startsWith('a=rtpmap:')) {
              const match = /^a=rtpmap:(\d+) ([A-Za-z0-9_.-]{1,32})\//.exec(line);
              if (match && Number(match[1]) <= 127) {
                if (current.codecs.length < 32)
                  current.codecs.push({ payloadType: Number(match[1]), name: match[2] });
                else note(truncated, 'codecs');
              }
            } else if (line.startsWith('a=candidate:')) {
              const fields = line.slice(12).trim().split(/\s+/);
              const kind = choose(fields[7], ['host', 'srflx', 'prflx', 'relay']) ?? 'unknown';
              const protocol = choose(fields[2]?.toLowerCase(), ['udp', 'tcp']) ?? 'unknown';
              current.candidates.count++;
              current.candidates.byType[kind] = (current.candidates.byType[kind] ?? 0) + 1;
              current.candidates.byProtocol[protocol] =
                (current.candidates.byProtocol[protocol] ?? 0) + 1;
            }
          }
        }
        return summary;
      }
      result.descriptions = {};
      for (const [name, property] of [
        ['local', 'localDescription'],
        ['remote', 'remoteDescription'],
        ['currentLocal', 'currentLocalDescription'],
        ['currentRemote', 'currentRemoteDescription'],
        ['pendingLocal', 'pendingLocalDescription'],
        ['pendingRemote', 'pendingRemoteDescription'],
      ]) {
        result.descriptions[name] = summarizeDescription(read(peer, property));
      }
      const trackState = (track) =>
        track
          ? {
              kind: choose(read(track, 'kind'), ['audio', 'video']),
              enabled: choose(read(track, 'enabled'), [true, false]),
              muted: choose(read(track, 'muted'), [true, false]),
              readyState: choose(read(track, 'readyState'), ['live', 'ended']),
            }
          : null;
      result.transceivers = [];
      try {
        const getTransceivers = read(peer, 'getTransceivers');
        if (typeof getTransceivers !== 'function') throw new Error('getTransceivers unavailable');
        const transceivers = getTransceivers.call(peer);
        if (!Array.isArray(transceivers)) throw new Error('Invalid transceiver collection');
        if (transceivers.length > 32) note(truncated, 'transceivers');
        result.transceivers = transceivers.slice(0, 32).map((item) => ({
          mid: token(read(item, 'mid')),
          direction: choose(read(item, 'direction'), directions),
          currentDirection: choose(read(item, 'currentDirection'), directions),
          stopped: choose(read(item, 'stopped'), [true, false]),
          senderTrack: trackState(read(read(item, 'sender'), 'track')),
          receiverTrack: trackState(read(read(item, 'receiver'), 'track')),
        }));
      } catch {
        note(errors, 'getTransceivers failed or unavailable');
      }

      result.stats = [];
      let timer;
      const timedOut = {};
      try {
        const getStats = read(peer, 'getStats');
        if (typeof getStats !== 'function') throw new Error('getStats unavailable');
        const stats = await Promise.race([
          Promise.resolve().then(() => getStats.call(peer)),
          new Promise((_, reject) => {
            timer = setTimeout(() => reject(timedOut), 2000);
          }),
        ]);
        const types = [
          'transport',
          'candidate-pair',
          'local-candidate',
          'remote-candidate',
          'inbound-rtp',
          'outbound-rtp',
          'codec',
        ];
        const fields = [
          'id',
          'timestamp',
          'transportId',
          'codecId',
          'localCandidateId',
          'remoteCandidateId',
          'selectedCandidatePairId',
          'ssrc',
          'rtxSsrc',
          'kind',
          'mimeType',
          'payloadType',
          'clockRate',
          'channels',
          'candidateType',
          'protocol',
          'port',
          'tcpType',
          'state',
          'dtlsState',
          'dtlsRole',
          'iceState',
          'iceRole',
          'nominated',
          'active',
          'priority',
          'packetsSent',
          'packetsReceived',
          'packetsLost',
          'bytesSent',
          'bytesReceived',
          'headerBytesSent',
          'headerBytesReceived',
          'framesEncoded',
          'framesDecoded',
          'framesSent',
          'framesReceived',
          'framesDropped',
          'framesPerSecond',
          'frameWidth',
          'frameHeight',
          'keyFramesEncoded',
          'keyFramesDecoded',
          'nackCount',
          'pliCount',
          'firCount',
          'targetBitrate',
          'availableOutgoingBitrate',
          'availableIncomingBitrate',
          'currentRoundTripTime',
          'totalRoundTripTime',
          'jitter',
          'totalEncodeTime',
          'totalDecodeTime',
          'selectedCandidatePairChanges',
          'requestsSent',
          'requestsReceived',
          'responsesSent',
          'responsesReceived',
          'consentRequestsSent',
          'packetsDiscardedOnSend',
          'bytesDiscardedOnSend',
          'retransmittedPacketsSent',
          'retransmittedBytesSent',
          'lastPacketReceivedTimestamp',
          'lastPacketSentTimestamp',
          'qualityLimitationReason',
        ];
        let seen = 0;
        for (const stat of stats.values()) {
          if (++seen > 256) {
            note(truncated, 'stats');
            break;
          }
          const type = read(stat, 'type');
          if (!types.includes(type)) continue;
          const filtered = { type };
          for (const field of fields) {
            const value = read(stat, field);
            if (
              (typeof value === 'number' && Number.isFinite(value)) ||
              typeof value === 'boolean' ||
              (typeof value === 'string' && value.length <= 256)
            )
              filtered[field] = value;
          }
          result.stats.push(filtered);
        }
      } catch (error) {
        note(errors, error === timedOut ? 'getStats timed out' : 'getStats failed or unavailable');
      } finally {
        clearTimeout(timer);
      }
      return finish(result);
    }),
  );
}

module.exports = { collectPeerDiagnostics };
