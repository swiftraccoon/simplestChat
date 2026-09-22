import { api } from '../src/ui';
import type { RoomClient } from '../src/room';
import type { SignalingClient } from '../src/signaling';
import type {
  AccountProfile,
  ClientMessage,
  RoomSnapshot,
  RoomMembersPage,
  ServerMessage,
} from '../src/protocol';

function expectType<T>(_value: T): void {}

/** Compiled, never executed: expect-error directives must each reject a real mistake. */
export async function checkContracts(
  room: RoomClient,
  message: ServerMessage,
  uncertainAction: 'changeNickname' | 'removeRoomBan',
): Promise<void> {
  const snapshot = await room.requestSocial('getRoomSnapshot');
  expectType<RoomSnapshot>(snapshot);
  const members = await room.requestSocial('listRoomMembers', { offset: 100 });
  expectType<RoomMembersPage>(members);
  await room.requestSocial('listRoomBans');
  await room.requestSocial('setChatPreferences', {
    allowPrivateMessages: false,
    ignoredParticipantIds: [],
  });
  await room.requestSocial('changeNickname', { nickname: 'Name' });
  await room.requestSocial('removeRoomBan', { banId: 'ban' });
  await room.requestSocial('setMemberRole', { targetUserId: 'user', role: 2 });
  await room.requestSocial('reportParticipant', {
    targetParticipantId: 'target',
    reason: 'reason',
  });
  await room.requestSocial('resolveRoomReport', { reportId: 'report', status: 'resolved' });

  // @ts-expect-error A mutation cannot omit its required payload.
  await room.requestSocial('changeNickname');
  // @ts-expect-error An action cannot consume another action's payload.
  await room.requestSocial('removeRoomBan', { nickname: 'Name' });
  // @ts-expect-error Snapshot requests have no payload.
  await room.requestSocial('getRoomSnapshot', { nickname: 'Name' });
  // @ts-expect-error Preferences require both privacy fields.
  await room.requestSocial('setChatPreferences', { allowPrivateMessages: true });
  // @ts-expect-error Pagination is numeric, not an unchecked string.
  await room.requestSocial('listRoomMembers', { offset: '100' });
  // @ts-expect-error Report resolution cannot reopen a report.
  await room.requestSocial('resolveRoomReport', { reportId: 'report', status: 'open' });
  // @ts-expect-error A union action must first be narrowed to correlate its payload.
  await room.requestSocial(uncertainAction, { nickname: 'Name' });
  // @ts-expect-error Callers cannot select arbitrary social response types.
  await room.requestSocial<AccountProfile>('getRoomSnapshot');
  // @ts-expect-error A member page has no bans list.
  expectType<unknown>(members.bans);
  // @ts-expect-error Client signaling also rejects mismatched social payloads.
  expectType<ClientMessage>({ type: 'removeRoomBan', requestId: 'id', nickname: 'Name' });

  if (message.type === 'socialResponse' && message.action === 'getRoomSnapshot') {
    expectType<RoomSnapshot>(message.data);
    // @ts-expect-error Discriminant narrowing does not retain other response fields.
    expectType<unknown>(message.data.bans);
  }
  if (message.type === 'socialResponse' && message.action === 'reportParticipant') {
    expectType<'open'>(message.data.status);
  }

  const profile = await api.accountProfile('token');
  expectType<AccountProfile>(profile);
  const updated = await api.updateProfile('token', {
    display_name: 'Name',
    bio: '',
    avatar_url: null,
  });
  expectType<AccountProfile>(updated);
  expectType<void>(await api.deleteRoom('room', 'token'));
  expectType<string>(
    (await api.recoveryKey('token', { current_password: 'password' })).recovery_key,
  );
  // @ts-expect-error HTTP callers cannot select arbitrary response types.
  await api.accountProfile<RoomSnapshot>('token');
  // @ts-expect-error A no-content response cannot be consumed as a profile.
  expectType<AccountProfile>(await api.deleteRoom('room', 'token'));
  // @ts-expect-error Profile fields follow Rust's complete PATCH contract.
  await api.updateProfile('token', { bio: 'text' });
  // @ts-expect-error Avatar clearing uses null, not a different data shape.
  await api.updateProfile('token', { display_name: 'Name', bio: '', avatar_url: {} });
  // @ts-expect-error Recovery redemption requires all three credential fields.
  await api.redeemRecovery({ email: 'person@example.test', new_password: 'password' });
  // @ts-expect-error An account response is not a room page.
  expectType<unknown>(profile.members);
}

export async function checkMediaContracts(signaling: SignalingClient): Promise<void> {
  const transport = await signaling.request({ type: 'createSendTransport' }, 'transportCreated');
  expectType<string>(transport.transportId);
  // @ts-expect-error A transport response has no consumer ID.
  expectType<string>(transport.consumerId);
  const consumer = await signaling.request(
    { type: 'consume', producerId: 'producer', rtpCapabilities: {} },
    'consumerCreated',
  );
  expectType<string>(consumer.consumerId);
  await signaling.request(
    { type: 'consume', producerId: 'producer', rtpCapabilities: {} },
    // @ts-expect-error A consume command cannot expect a transport acknowledgement.
    'transportCreated',
  );
  // @ts-expect-error Correlation IDs are generated by the signaling client.
  await signaling.request({ type: 'createSendTransport', requestId: 'reused' }, 'transportCreated');
  // @ts-expect-error Events cannot be selected as a request acknowledgement.
  await signaling.request({ type: 'createSendTransport' }, 'newProducer');
  await signaling.request(
    // @ts-expect-error Authentication renewal has a separate request lifecycle.
    { type: 'renewAuthentication', requestId: 'auth', token: 'token' },
    'authenticationRenewed',
  );
  // @ts-expect-error Callers cannot invent extra fields on the expected response.
  await signaling.request<{ type: 'consumerCreated'; invented: string }>(
    { type: 'consume', producerId: 'producer', rtpCapabilities: {} },
    'consumerCreated',
  );
}
