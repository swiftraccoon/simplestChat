// Explicit binary parity gate; ordinary helper checks never build a generator.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { isAbsolute, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const root = fileURLToPath(new URL('../', import.meta.url));
const binary = process.env.LOAD_TEST_CAPACITY_BINARY;
const enabled = Boolean(binary);

function command(executable, args) {
  const result = spawnSync(executable, args, {
    cwd: root, encoding: 'utf8', timeout: 30000, maxBuffer: 2 * 1024 * 1024,
  });
  assert.equal(result.error, undefined);
  assert.equal(result.signal, null);
  return result;
}

function parity(args) {
  assert.equal(isAbsolute(binary), true, 'select an explicit absolute capacity executable');
  const native = command(binary, ['capacity', ...args]);
  const reference = command('python3', ['build/capacity.py', ...args]);
  assert.equal(native.status, reference.status, args.join(' '));
  assert.equal(native.stdout, reference.stdout);
  assert.equal(native.stderr, reference.stderr);
  return native;
}

test('compiled capacity command matches reference suggestions and validation without a workload', { skip: !enabled }, () => {
  for (const args of [
    ['suggest', '--vcpus', '8', '--memory-gib', '16'],
    ['suggest', '--vcpus', '4', '--memory-gib', '12', '--env'],
    ['suggest', '--vcpus', '4', '--memory-gib', '12', '--included-egress-tb', '5'],
    ['suggest', '--vcpus', '16', '--memory-gib', '64', '--port-mbps', '100', '--app-cpus', '2.75'],
    ['suggest', '--vcpus', '0', '--memory-gib', '16'],
  ]) parity(args);
  for (const args of [[], ['suggest'], ['compare'], ['invented']]) {
    assert.equal(command(binary, ['capacity', ...args]).status, 2);
  }
  const help = command(binary, ['capacity', '--help']);
  assert.equal(help.status, 0);
  assert.match(help.stdout, /^Usage: load_test capacity suggest/);
});

test('compiled comparison keeps rankings, workload identity and legacy calibration behavior', { skip: !enabled }, t => {
  const directory = mkdtempSync(join(tmpdir(), 'simplestchat capacity CLI '));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const report = (label, monthly) => ({
    label, host: { logicalCpus: 4, cpuModel: 'EPYC reference 🦊' },
    measurement: { meetingSize: 5, browser: { capture: '720p', speakers: 1,
      viewportWidth: 1440, pixelRatio: 2, layout: 'classic' } },
    ceilings: { meetings: { ceiling: 85, workers: 1, egressMbps: 203.15, memoryPeakBytes: 178257920 },
      'large-meeting': { ceiling: 28, bound: 'generator-limited' }, webinar: { ceiling: 175, egressMbps: 175 } },
    projection: { meetingParticipants: 255, lowerBound: true, egressMbpsPerParticipant: 2.39 },
    cost: { dollarsPer1000ParticipantHours: monthly / 255 / 730 * 1000, prices: { monthly } },
  });
  const dear = join(directory, 'dear.json');
  const cheap = join(directory, 'cheap.json');
  writeFileSync(dear, JSON.stringify(report('dear', 40)));
  writeFileSync(cheap, JSON.stringify(report('cheap', 5)));
  const compared = parity(['compare', dear, cheap]);
  assert.match(compared.stdout.split('\n')[1], /^cheap\s/);
  parity(['suggest', '--vcpus', '4', '--memory-gib', '12', '--calibration', cheap]);
  const changed = report('audio', 5);
  changed.measurement.browser.audioOnly = true;
  writeFileSync(cheap, JSON.stringify(changed));
  assert.equal(parity(['compare', dear, cheap]).status, 1);
  assert.equal(parity(['suggest', '--vcpus', '4', '--memory-gib', '12', '--calibration', cheap]).status, 1);
});
