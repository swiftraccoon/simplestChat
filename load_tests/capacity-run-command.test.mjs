// Explicit compiled-binary gate. Every engine executable is an owned fake;
// no Docker, Podman, VM, network or generator workload is invoked.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { chmodSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { isAbsolute, join } from 'node:path';
import { setTimeout } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const root = fileURLToPath(new URL('../', import.meta.url));
const binary = process.env.LOAD_TEST_CAPACITY_BINARY;
const enabled = Boolean(binary);

const fakeEngine = `#!${process.execPath}
import fs from 'node:fs';
import path from 'node:path';
const a=process.argv.slice(2),d=process.env.CAPACITY_FAKE_ROOT;
const statePath=path.join(d,'container.json');
const state=fs.existsSync(statePath)?JSON.parse(fs.readFileSync(statePath)):null;
fs.appendFileSync(path.join(d,'calls.jsonl'),JSON.stringify(a.map(v=>v.startsWith('METRICS_TOKEN=')?'METRICS_TOKEN=[redacted]':v))+'\\n');
const value=k=>a[a.indexOf(k)+1];
const emit=s=>process.stdout.write(s+'\\n');
if(a[0]==='image'){emit(a[3]==='{{.Id}}'?'sha256:fake':'0123456789abcdef');}
else if(a[0]==='run'&&a.includes('--rm')){
 emit('model name: Fake CPU\\n@@\\nMemTotal: 4194304 kB\\n@@\\ncpu 10 0 0 90 0 0 0 0\\ncpu0 1\\ncpu1 1\\ncpu2 1\\ncpu3 1\\n@@\\ncpu 20 0 0 180 0 0 0 0\\n@@\\n2097152\\n@@\\nfake-kernel\\n@@\\nFake Model');
}else if(a[0]==='run'&&a.includes('--detach')){
 const record={Id:'1'.padStart(64,'0'),Name:'/'+value('--name'),Config:{Image:a.at(-1),Labels:{'simplestchat.capacity.run':value('--label').split('=')[1]}}};
 fs.writeFileSync(statePath,JSON.stringify(record),{mode:0o600});emit(record.Id);
}else if(a[0]==='exec'){
 fs.writeFileSync(path.join(d,'health-seen'),'ready',{mode:0o600});
 process.stderr.write('owned fake health unavailable\\n');process.exitCode=3;
}else if(a[0]==='inspect'){
 if(state&&(a.at(-1)===state.Id||a.at(-1)===state.Name.slice(1))){
  emit(a.includes('--format')?'false':JSON.stringify([state]));
 }else{process.stderr.write('No such container\\n');process.exitCode=1;}
}else if(a[0]==='stop'){assertOwned();emit(state.Id);}
else if(a[0]==='logs'){assertOwned();emit('owned fake log');}
else if(a[0]==='rm'){assertOwned();fs.unlinkSync(statePath);emit(state.Id);}
else{process.stderr.write('refused fake command\\n');process.exitCode=99;}
function assertOwned(){if(!state||a.at(-1)!==state.Id)throw Error('unowned fixture identity');}
`;

test('capacity run validates without discovering or executing a real engine', { skip: !enabled }, () => {
  assert.equal(isAbsolute(binary), true);
  for (const args of [[], ['--server-image', 'server'],
    ['--server-image', 'server', '--generator-image', 'gen', '--server-env', 'BIND_ADDR=0.0.0.0'],
    ['--server-image', 'server', '--generator-image', 'gen', '--server-cpus', 'NaN']]) {
    const result = spawnSync(binary, ['capacity', 'run', ...args], {
      cwd: root, env: { ...process.env, PATH: '/nonexistent-capacity-fake' },
      encoding: 'utf8', timeout: 5000, maxBuffer: 1024 * 1024,
    });
    assert.equal(result.error, undefined);
    assert.equal(result.status, 2);
  }
  const result = spawnSync(binary, ['capacity', 'run', '--help'], {
    encoding: 'utf8', timeout: 5000, maxBuffer: 1024 * 1024,
  });
  assert.equal(result.status, 0);
  assert.match(result.stdout, /^Usage: load_test capacity run/);
});

test('SIGTERM retains interrupted evidence and removes only the inspected owned fake ID', { skip: !enabled, timeout: 20000 }, async t => {
  assert.equal(isAbsolute(binary), true);
  const directory = mkdtempSync(join(tmpdir(), 'capacity measured fake CLI '));
  chmodSync(directory, 0o700);
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const engine = join(directory, 'docker');
  writeFileSync(engine, fakeEngine, { mode: 0o700 });
  const output = join(directory, 'run');
  const child = spawn(binary, ['capacity', 'run', '--server-image', 'fake-server',
    '--generator-image', 'fake-generator', '--engine', 'docker', '--output', output,
    '--workloads', 'meetings', '--quick', '--steps', '1'], {
    cwd: root, env: { ...process.env, PATH: directory, CAPACITY_FAKE_ROOT: directory },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = '', stderr = '';
  child.stdout.setEncoding('utf8').on('data', chunk => { stdout += chunk; });
  child.stderr.setEncoding('utf8').on('data', chunk => { stderr += chunk; });
  const exited = once(child, 'exit');
  t.after(async () => {
    if (child.exitCode === null && child.signalCode === null) {
      child.kill('SIGKILL');
      await exited;
    }
  });
  const deadline = Date.now() + 10000;
  while (!existsSync(join(directory, 'health-seen'))) {
    assert.equal(child.exitCode, null, stderr);
    assert.ok(Date.now() < deadline, 'owned fake engine reached health polling');
    await setTimeout(20);
  }
  child.kill('SIGTERM');
  const [code, signal] = await exited;
  assert.equal(code, 143, stderr);
  assert.equal(signal, null);
  const report = JSON.parse(readFileSync(join(output, 'calibration.json'), 'utf8'));
  assert.equal(report.steps.length, 1);
  assert.equal(report.steps[0].valid, false);
  assert.match(report.steps[0].error, /interrupted/);
  assert.equal(report.ceilings.meetings.bound, 'interrupted');
  assert.equal(report.ceilings.webinar.bound, 'not-run');
  assert.equal(report.projection.meetingParticipants, 0);
  assert.match(stdout, /invalid/);
  assert.equal(existsSync(join(directory, 'container.json')), false);
  const calls = readFileSync(join(directory, 'calls.jsonl'), 'utf8').trim().split('\n').map(JSON.parse);
  const destructive = calls.filter(args => ['stop', 'rm'].includes(args[0]));
  assert.deepEqual(destructive.map(args => args[0]), ['stop', 'rm']);
  assert.ok(destructive.every(args => args.at(-1) === '1'.padStart(64, '0')));
  assert.equal(calls.some(args => args[0] === 'cp'), false);
  const journal = JSON.parse(readFileSync(join(output, 'ownership.json'), 'utf8'));
  assert.equal(journal.containers.length, 2); // probe intent and server exact ID.
  assert.equal(journal.containers.at(-1).id, '1'.padStart(64, '0'));
});
