/** Require real TURN allocations; native RTC and server credentials stay intact. */
function installRelayPolicy() {
  window.RTCPeerConnection = new Proxy(window.RTCPeerConnection, {
    construct(target, args, newTarget) {
      return Reflect.construct(
        target,
        [{ ...args[0], iceTransportPolicy: 'relay' }, ...args.slice(1)],
        newTarget,
      );
    },
  });
}

/** Self-contained native selected-pair check; never return addresses or credentials. */
async function selectedRelayPaths() {
  const peers = window.__communityPeers.filter(
    (peer) => peer.connectionState !== 'closed' && peer.remoteDescription,
  );
  if (!peers.length) throw new Error('No negotiated peer connections to verify');
  return await Promise.all(
    peers.map(async (peer) => {
      if (peer.connectionState !== 'connected') throw new Error('Relay peer is not connected');
      if (peer.getConfiguration().iceTransportPolicy !== 'relay')
        throw new Error('Native RTC relay policy was not applied');
      const stats = await peer.getStats();
      const entries = [...stats.values()];
      const selectedIds = new Set(
        entries
          .filter((entry) => entry.type === 'transport' && entry.selectedCandidatePairId)
          .map((entry) => entry.selectedCandidatePairId),
      );
      const pairs = entries.filter(
        (entry) =>
          entry.type === 'candidate-pair' &&
          (selectedIds.size
            ? selectedIds.has(entry.id)
            : entry.nominated === true && entry.state === 'succeeded'),
      );
      if (pairs.length !== 1 || pairs[0].state !== 'succeeded' || pairs[0].nominated !== true)
        throw new Error('Expected one successful native selected candidate pair');
      const pair = pairs[0];
      const local = stats.get(pair.localCandidateId);
      if (local?.candidateType !== 'relay')
        throw new Error('Selected media path bypassed the TURN relay');
      if (
        ![pair.bytesSent, pair.bytesReceived].every(
          (bytes) => Number.isSafeInteger(bytes) && bytes > 0,
        )
      )
        throw new Error('Selected TURN relay has no bidirectional traffic');
      return {
        candidateId: local.id,
        candidateType: local.candidateType,
        bytesSent: pair.bytesSent,
        bytesReceived: pair.bytesReceived,
      };
    }),
  );
}

async function waitForRelayPaths(page, previous) {
  const deadline = performance.now() + 10000;
  let failure;
  do {
    try {
      const paths = await page.evaluate(selectedRelayPaths);
      if (
        previous &&
        (paths.length !== previous.length ||
          paths.some((path, index) => path.candidateId === previous[index].candidateId))
      )
        throw new Error('ICE restart has not selected fresh TURN allocations');
      return paths;
    } catch (error) {
      failure = error;
    }
    await page.waitForTimeout(200);
  } while (performance.now() < deadline);
  throw new Error('TURN relay paths did not become ready', { cause: failure });
}

module.exports = { installRelayPolicy, selectedRelayPaths, waitForRelayPaths };
