import { test } from 'node:test';
import assert from 'node:assert/strict';
import { threadTicks, cgroupCpuStat, threadGroup, serverAttribution, netemScript, parseOptions } from './benchmark-podman.mjs';

const stat = (tid, name, utime, stime) => `${tid} (${name}) S 0 1 1 0 -1 4194560 100 0 0 0 ${utime} ${stime} 0 0 20 0 3 0 5 1000 200 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n`;

test('thread ticks are summed per thread name', () => {
  const ticks = threadTicks(stat(1, 'simplestChat', 10, 5) + stat(2, 'mediasoup-worke', 100, 20) + stat(3, 'mediasoup-worke', 50, 10) + stat(4, 'tokio-runtime-w', 7, 1));
  assert.deepEqual(ticks, { simplestChat: 15, 'mediasoup-worke': 180, 'tokio-runtime-w': 8 });
});

test('cgroup cpu.stat requires the throttling counters', () => {
  assert.deepEqual(cgroupCpuStat('usage_usec 10\nuser_usec 5\nsystem_usec 5\nnr_periods 3\nnr_throttled 1\nthrottled_usec 20000\n').throttled_usec, 20000);
  assert.throws(() => cgroupCpuStat('usage_usec 10\n'), /nr_periods/);
});

test('attribution reports per-group CPU seconds and exact cgroup usage over the window', () => {
  const detail = (worker, tokio, usage, periods, throttled, throttledUsec) => ({
    threads: { 'mediasoup-worker-1': worker, 'tokio-runtime-w': tokio, simplestChat: 0 },
    cgroup: { usage_usec: usage, nr_periods: periods, nr_throttled: throttled, throttled_usec: throttledUsec } });
  const samples = [
    { elapsedMs: 0, serverDetail: detail(0, 0, 0, 0, 0, 0) },
    { elapsedMs: 1000, serverDetail: detail(100, 50, 1_500_000, 10, 0, 0) },
    { elapsedMs: 11000, serverDetail: detail(1100, 250, 16_500_000, 110, 20, 400_000) },
    { elapsedMs: 12000, serverDetail: null },
  ];
  const result = serverAttribution(samples, 1000, 11000, 100, 2);
  assert.equal(result.sampledDurationSeconds, 10);
  assert.deepEqual(result.threadCpuSeconds, { mediasoupWorkers: 10, tokioWorkers: 2, other: 0 });
  assert.equal(result.cgroup.usageSeconds, 15);
  assert.equal(result.cgroup.cpuPercentOfQuota, 75);
  assert.equal(result.cgroup.throttledPeriods, 20);
  assert.equal(result.cgroup.throttledPeriodFraction, 0.2);
  assert.equal(result.cgroup.throttledSeconds, 0.4);
  assert.equal(threadGroup('mediasoup-worke'), 'mediasoupWorkers');
  assert.equal(threadGroup('tokio-rt-worker'), 'tokioWorkers');
});

test('netem script impairs only UDP, optionally only the server ports', () => {
  const both = netemScript('loss 5% delay 50ms 10ms', 'udp-both', 41100, 2);
  assert.match(both, /netem limit 200000 loss 5% delay 50ms 10ms/);
  assert.match(both, /match ip protocol 17 0xff flowid 1:3/);
  assert.doesNotMatch(both, /sport/);
  const downlink = netemScript('rate 300kbit', 'udp-downlink', 41100, 2);
  assert.match(downlink, /match ip sport 41100 0xfffe flowid/);
  assert.throws(() => netemScript('rate 300kbit', 'udp-downlink', 41101, 2), /multiple of 2/);
  assert.throws(() => netemScript('loss 5%; rm -rf /', 'udp-both', 41100, 1), /only letters/);
  assert.throws(() => netemScript('loss 5%', 'everything', 41100, 1), /netem-scope/);
});

test('options require the join-limit ramp and validate limits', () => {
  const base = ['--server-image', 'a', '--generator-image', 'b', '--output', 'out'];
  const options = parseOptions([...base, '--clients', '100,200', '--ramp-up', '405', '--subscription-plan', 'ring-v1', '--subscription-seed', '17', '--netem', 'loss 2%']);
  assert.deepEqual(options.scenarios.map(s => [s.name, s.rooms]), [['multi-room-100', 4], ['multi-room-200', 4]]);
  assert.equal(options.cpus, 2);
  assert.equal(options.memory, '2g');
  assert.equal(options.netemScope, 'udp-both');
  assert.throws(() => parseOptions([...base, '--clients', '300', '--ramp-up', '400']), /--ramp-up 605/);
  assert.throws(() => parseOptions([...base, '--clients', '10', '--netem-scope', 'udp-downlink']), /requires --netem/);
  assert.throws(() => parseOptions([...base, '--clients', '10', '--memory', '2GB']), /--memory/);
});

test('a server sampler block yields process, cgroup and thread readings', async () => {
  const { parseServerBlock } = await import('./benchmark-podman.mjs');
  const block = `${stat(1, 'simplestChat', 10, 5)}@@S\nusage_usec 100\nuser_usec 50\nsystem_usec 50\nnr_periods 2\nnr_throttled 1\nthrottled_usec 5\n@@Y\n${stat(1, 'simplestChat', 10, 5)}${stat(2, 'mediasoup-worke', 20, 0)}`;
  const parsed = parseServerBlock(block, 100, 4096);
  assert.equal(parsed.server.cpuSeconds, 0.15);
  assert.equal(parsed.serverDetail.cgroup.nr_throttled, 1);
  assert.deepEqual(parsed.serverDetail.threads, { simplestChat: 15, 'mediasoup-worke': 20 });
});

test('webinar scenarios keep one room, one publisher and per-address ramps', () => {
  const base = ['--server-image', 's', '--generator-image', 'g', '--output', 'out'];
  const options = parseOptions([...base, '--scenarios', 'webinar', '--clients', '300,1000', '--ramp-up', '60']);
  assert.deepEqual(
    options.scenarios.map((s) => [s.name, s.rooms, s.mode, s.sourceAddresses, s.extra]),
    [
      ['webinar-300', 1, 'webinar', 250, ['--publish-ratio', '0.001', '--source-addresses', '250']],
      ['webinar-1000', 1, 'webinar', 250, ['--publish-ratio', '0.001', '--source-addresses', '250']],
    ],
  );
  // Four viewers per loopback address stay under the 10-per-room-and-address
  // limit, so the join spacing no longer forces a ten-minute ramp per hundred.
  assert.throws(() => parseOptions([...base, '--scenarios', 'conference', '--clients', '300', '--ramp-up', '60']), /--ramp-up 1815/);
  assert.throws(() => parseOptions([...base, '--clients', '1001']), /between 2 and 1000/);
});
