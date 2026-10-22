// Credential-free lifecycle fixtures shared by the native/reference parity
// checks. They do not read source text or execute a benchmark workload.
export function lifecycleFixture({ departure = 'abrupt', base = Date.parse('2026-09-11T12:00:00.000Z') } = {}) {
  const participantId = '00000000-0000-4000-8000-000000000001';
  const transportId = '00000000-0000-4000-8000-000000000002';
  const generationId = '00000000-0000-4000-8000-000000000003';
  const workerId = '00000000-0000-4000-8000-000000000004';
  const at = ms => new Date(base + ms).toISOString();
  const marker = (ms, event, fields = '') => `${at(ms)} DEBUG simplestchat::lifecycle: lifecycle event="${event}" participant_id=${participantId}${fields ? ` ${fields}` : ''}`;
  const transport = (ms, event, state) => marker(ms, event, `transport_id=${transportId} transport_type="recv"${state ? ` state=${state}` : ''}`);
  const cleanup = (ms, event) => marker(ms, event, `generation=${generationId}`);
  const clamp = ms => `${at(ms)} ERROR mediasoup::worker: [id:${workerId}] webrtc::GoogCcNetworkController::ClampConstraints() | start bitrate smaller than min bitrate [starting_rate_:30000, min_data_rate_:100000]`;
  const timeline = { schemaVersion: 1, departure,
    events: [['run_started', 0], ['server_ready', 500], ['generator_started', 1000], ['generator_exited', 14000], ['cleanup_observed', 15000], ['server_stop_requested', 16000], ['server_exited', 17000]].map(([event, elapsedMs]) => ({ event, at: at(elapsedMs), elapsedMs })),
    workload: { startedAt: at(1000).replace('Z', '+00:00'), finishedAt: at(13900).replace('Z', '+00:00'), rampUpSecs: 1, warmupSecs: 2, durationSecs: 8 } };
  const lines = [clamp(600), transport(1500, 'transport_created'), transport(2000, 'transport_ice', 'Connected'),
    transport(2500, 'transport_dtls', 'Connected'), clamp(5000), transport(12000, 'transport_ice', 'Disconnected'),
    clamp(12100), marker(12200, departure === 'abrupt' ? 'grace_started' : 'explicit_leave_started'),
    ...(departure === 'abrupt' ? [marker(13000, 'grace_cleanup_started')] : []),
    cleanup(13100, 'media_cleanup_started'), transport(13200, 'transport_closed'), clamp(13300), cleanup(13400, 'media_cleanup_finished'),
    ...(departure === 'explicit-leave' ? [marker(13500, 'explicit_leave_finished')] : []), clamp(15500), clamp(16500)];
  return { lines, timeline, at, marker, transport, cleanup, clamp, ids: [participantId, transportId, generationId, workerId] };
}
