/** Receive-only native ICE gathering, without application code, capture or ICE servers. */
async function setupReceiveOnlyPeer(role) {
  if (!['offerer', 'answerer'].includes(role)) throw new Error('Unsupported isolation role');
  const peer = new RTCPeerConnection({ iceServers: [] });
  window.__iceIsolationPeer = peer;
  if (role === 'offerer') {
    peer.addTransceiver('video', { direction: 'recvonly' });
    await peer.setLocalDescription(await peer.createOffer());
  } else {
    // Generate a native offer without starting the helper's ICE gathering.
    const source = new RTCPeerConnection({ iceServers: [] });
    let offer;
    try {
      source.addTransceiver('video', { direction: 'sendonly' });
      offer = await source.createOffer();
    } finally { source.close(); }
    await peer.setRemoteDescription(offer);
    for (const transceiver of peer.getTransceivers()) transceiver.direction = 'recvonly';
    await peer.setLocalDescription(await peer.createAnswer());
  }
  return window.__communityPeers.indexOf(peer);
}

function receiveOnlyGatheringComplete() {
  const peer = window.__iceIsolationPeer;
  return peer?.iceGatheringState === 'complete'
    && window.__communityPeerEvents?.get(peer)?.events.some(event => event.event === 'icecandidate' && event.phase === 'candidate') === true;
}

async function run() {
  const fs = require('node:fs');
  const os = require('node:os');
  const path = require('node:path');
  const { browserOptions } = require('./browser-options.cjs');
  const { installPeerEventTracing } = require('./peer-events.cjs');
  const { collectPeerDiagnostics } = require('./peer-diagnostics.cjs');
  const options = browserOptions(process.env.E2E_BROWSER);
  const moduleName = process.env.PLAYWRIGHT_MODULE || 'playwright';
  const playwright = require(moduleName);
  const artifacts = process.env.E2E_ARTIFACTS || fs.mkdtempSync(path.join(os.tmpdir(), 'simplestchat-ice-isolation.'));
  fs.mkdirSync(artifacts, { recursive: true, mode: 0o700 });
  const report = {
    browser: options.name, browserVersion: null,
    playwrightVersion: require(path.join(path.dirname(require.resolve(moduleName)), 'package.json')).version,
    platform: process.platform, arch: process.arch,
    launchOptions: options.launchOptions, contextOptions: options.contextOptions,
    capture: false, iceServers: [], gatheringDeadlineMs: 15000,
    startedAt: new Date().toISOString(), complete: false, passed: false, cases: [],
    limitations: ['Gathering only: no ICE-lite server, application negotiation, candidate exchange, connectivity or media delivery.'],
  };
  const save = () => fs.writeFileSync(path.join(artifacts, 'ice-isolation-results.json'), `${JSON.stringify(report, null, 2)}\n`, { mode: 0o600 });
  async function deadline(work, milliseconds) {
    let timer;
    try { return await Promise.race([work, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error('Operation deadline exceeded')), milliseconds);
    })]); } finally { clearTimeout(timer); }
  }
  save();
  for (const name of ['offerer', 'answerer']) {
    const result = { name, passed: false, peerIndex: null, stage: 'launch' };
    report.cases.push(result); save();
    let browser, context, page;
    try {
      // A fresh process per role also keeps this check independent of community tests.
      browser = await playwright[options.name].launch(options.launchOptions);
      report.browserVersion = browser.version();
      result.stage = 'context';
      context = await browser.newContext(options.contextOptions);
      // Fulfill a trustworthy loopback origin in-process; never load a running app.
      const origin = 'http://127.0.0.1/ice-isolation';
      await context.route('**/*', route => route.request().url() === origin
        ? route.fulfill({ status: 200, contentType: 'text/html', body: '<!doctype html><title>ICE isolation</title>' })
        : route.abort());
      page = await context.newPage();
      await page.addInitScript(installPeerEventTracing, { announcedIp: process.env.TEST_ANNOUNCE_IP || null });
      result.stage = 'navigation';
      await page.goto(origin, { waitUntil: 'load', timeout: 10000 });
      result.stage = 'negotiation';
      result.peerIndex = await deadline(page.evaluate(setupReceiveOnlyPeer, name), 10000);
      result.stage = 'gathering';
      await page.waitForFunction(receiveOnlyGatheringComplete, null, { timeout: report.gatheringDeadlineMs });
      result.passed = true;
      result.stage = 'complete';
    } catch {
      // Browser errors can contain SDP or candidates. Persist only fixed labels.
      result.error = result.stage === 'gathering'
        ? 'ICE gathering did not publish a candidate and complete within 15000 ms'
        : `${result.stage} failed`;
    } finally {
      if (page) {
        try { result.media = await deadline(page.evaluate(collectPeerDiagnostics), 5000); }
        catch { result.diagnosticsError = 'Peer snapshot unavailable'; result.passed = false; }
        try { await deadline(page.evaluate(() => {
          for (const peer of window.__communityPeers ?? []) peer.close();
        }), 5000); } catch { result.cleanupError = 'Peer cleanup failed'; result.passed = false; }
      }
      for (const resource of [context, browser]) {
        if (!resource) continue;
        try { await deadline(resource.close(), 5000); }
        catch { result.cleanupError = 'Browser cleanup failed'; result.passed = false; }
      }
      save();
    }
    console.log(`${result.passed ? 'PASS' : 'FAIL'} receive-only ICE ${name}${result.error ? `: ${result.error}` : ''}`);
  }
  report.complete = true;
  report.passed = report.cases.every(result => result.passed);
  report.finishedAt = new Date().toISOString();
  save();
  console.log(`ICE isolation artifacts: ${artifacts}`);
  if (!report.passed) process.exitCode = 1;
}

if (require.main === module) {
  if (process.env.ICE_ISOLATION_E2E !== '1') {
    console.error('Set ICE_ISOLATION_E2E=1 to run isolated receive-only ICE gathering.');
    process.exitCode = 1;
  } else {
    run().catch(() => { console.error('ICE isolation runner failed; check the browser selection, installation and artifact directory.'); process.exitCode = 1; });
  }
}

module.exports = { setupReceiveOnlyPeer, receiveOnlyGatheringComplete };
