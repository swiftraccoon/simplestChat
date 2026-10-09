import assert from 'node:assert/strict';
import test from 'node:test';
import { deferred, flush, uiFixture } from './ui-fixture.mjs';
import { loadContractModules } from './source-loader.mjs';
import { loadDiscoveryFixture } from './discovery-fixture.mjs';

const OWN = '00000000-0000-4000-8000-000000000001';
const PEER = '00000000-0000-4000-8000-000000000002';
const OTHER = '00000000-0000-4000-8000-000000000003';
const room = (id = 'public') => ({
  id,
  display_name: 'Public room',
  topic: null,
  participant_count: 2,
  password_protected: false,
  moderated: false,
  broadcaster_count: 0,
  description: '',
  image_url: null,
  secret: false,
  name_style: { color: null, style: 'accent' },
  topic_style: { color: null, style: 'accent' },
});
const contact = (status = 'accepted', accountId = PEER) => ({
  accountId,
  accountName: '<script>Peer</script>',
  status,
});

async function fixture() {
  const f = await uiFixture();
  const state = {
    account: OWN,
    token: 'token',
    current: true,
    contacts: [],
    rooms: [],
    calls: [],
    messages: [],
    copied: '',
    joins: [],
  };
  const api = {
    contacts: async () => ({ accountId: OWN, contacts: state.contacts }),
    requestContact: async (...args) => state.calls.push(['request', ...args]),
    acceptContact: async (...args) => state.calls.push(['accept', ...args]),
    removeContact: async (...args) => state.calls.push(['remove', ...args]),
    savedRooms: async () => ({ rooms: state.rooms }),
    saveRoom: async (...args) => state.calls.push(['favorite', ...args]),
  };
  const window = {
    location: { origin: 'https://example.test', href: 'https://example.test/', hash: '' },
    history: { state: {}, replaceState: (_state, _title, url) => state.calls.push(['url', url]) },
  };
  const module = await loadDiscoveryFixture({
    ...f,
    window,
    ui: { ...f.ui, api },
    navigator: {
      clipboard: {
        writeText: async (value) => {
          state.copied = value;
        },
      },
    },
  });
  const host = f.ui.el('div');
  f.document.body.append(host);
  const options = {
    getToken: () => state.token,
    getAccountId: () => state.account,
    current: () => state.current,
    onMessage: (...args) => state.messages.push(args),
    onJoin: (id) => state.joins.push(id),
  };
  return { ...f, state, api, module, host, options, window };
}
function click(host, label) {
  const node = host.querySelectorAll('button').find((node) => node.textContent === label);
  assert.ok(node, label);
  node.click();
}

test('contact offers accept same-origin links only and scrub navigation without sending anything', async () => {
  const f = await fixture();
  assert.equal(f.module.contactLink(PEER), `https://example.test/#contact=${PEER}`);
  assert.equal(f.module.parseContactLink(`https://example.test/#contact=${PEER}`), PEER);
  for (const value of [
    PEER,
    `https://elsewhere.test/#contact=${PEER}`,
    `https://example.test/path#contact=${PEER}`,
    `https://example.test/?token=secret#contact=${PEER}`,
    'javascript:alert(1)',
  ])
    assert.throws(() => f.module.parseContactLink(value), /contact link/);
  f.window.location.href = `https://example.test/#contact=${PEER}`;
  f.window.location.hash = `#contact=${PEER}`;
  assert.equal(f.module.takeContactOffer(), PEER);
  assert.deepEqual(f.state.calls, [['url', 'https://example.test/']]);
});

test('contacts render names as text and only accepted contacts can start a PM', async () => {
  const f = await fixture();
  f.state.contacts = [contact('incoming'), contact('outgoing', OTHER)];
  const mount = f.module.mountContacts(f.host, f.options);
  await flush();
  assert.match(f.host.textContent, /Contacts · 1 request/);
  assert.equal(f.host.querySelectorAll('script').length, 0);
  assert.equal(
    f.host.querySelectorAll('button').some((node) => node.textContent === 'Message'),
    false,
  );
  click(f.host, 'Accept');
  await flush();
  assert.equal(f.state.calls[0][0], 'accept');
  assert.equal(f.state.calls[0][2], PEER);
  f.state.contacts = [contact()];
  await mount.refresh();
  click(f.host, 'Message');
  assert.deepEqual(f.state.messages, [[PEER, '<script>Peer</script>']]);
  click(f.host, 'Copy my contact link');
  await flush();
  assert.equal(f.state.copied, `https://example.test/#contact=${OWN}`);
  mount.dispose();
  assert.equal(f.host.textContent, '');
});

test('contact refreshes and actions cannot mutate another account after identity changes', async () => {
  const f = await fixture();
  const pending = deferred();
  f.api.contacts = () => pending.promise;
  const mount = f.module.mountContacts(f.host, f.options);
  const before = f.host.textContent;
  f.state.account = OTHER;
  pending.resolve({ accountId: OWN, contacts: [contact()] });
  await flush();
  assert.equal(f.host.textContent, before);
  assert.equal(f.host.querySelectorAll('.discovery-row').length, 0);
  click(f.host, 'Copy my contact link');
  await flush();
  assert.equal(f.state.copied, '');
  await mount.refresh();
  mount.dispose();
});

test('contact offer requires a click and ignores a completion after dialog closure', async () => {
  const f = await fixture();
  const pending = deferred();
  f.api.requestContact = async (...args) => {
    f.state.calls.push(args);
    return pending.promise;
  };
  f.module.offerContact(PEER, f.options);
  assert.equal(f.state.calls.length, 0);
  const dialog = f.document.querySelectorAll('dialog')[0];
  click(dialog, 'Send request');
  await flush();
  assert.equal(f.state.calls.length, 1);
  dialog.close();
  pending.resolve();
  await flush();
  assert.doesNotMatch(dialog.textContent, /Request sent/);
});

test('saved rooms group favorites and recents and persist explicit toggles', async () => {
  const f = await fixture();
  f.state.rooms = [
    { room: room(), favorite: true, lastVisited: null },
    { room: room('recent'), favorite: false, lastVisited: '2026-10-09T10:00:00Z' },
  ];
  const mount = f.module.mountSavedRooms(f.host, f.options);
  await flush();
  assert.match(f.host.textContent, /Favorites/);
  assert.match(f.host.textContent, /Recent rooms/);
  const stars = f.host
    .querySelectorAll('button')
    .filter((node) => node.getAttribute('aria-pressed') !== null);
  stars[0].click();
  await flush();
  assert.deepEqual(f.state.calls[0].slice(0, 4), ['favorite', 'token', 'public', false]);
  f.host
    .querySelectorAll('button')
    .find((node) => node.className === 'discovery-room-link')
    .click();
  assert.deepEqual(f.state.joins, ['public']);
  mount.dispose();
});

test('discovery decoders enforce account ownership, bounds, statuses, and unique room identities', async () => {
  const modules = await loadContractModules();
  const { decodeContacts, decodeSavedRooms } = modules['./discovery-validation'];
  assert.deepEqual(decodeContacts({ accountId: OWN, contacts: [contact()] }).contacts, [contact()]);
  assert.throws(() => decodeContacts({ accountId: OWN, contacts: [contact('accepted', OWN)] }));
  assert.throws(() => decodeContacts({ accountId: OWN, contacts: [contact('blocked')] }));
  assert.throws(() => decodeContacts({ accountId: OWN, contacts: [contact(), contact()] }));
  assert.throws(() =>
    decodeContacts({ accountId: OWN, contacts: [{ ...contact(), accountName: 'x'.repeat(65) }] }),
  );
  const entry = { room: room(), favorite: true, lastVisited: null };
  assert.equal(decodeSavedRooms({ rooms: [entry] }).rooms.length, 1);
  assert.throws(() => decodeSavedRooms({ rooms: [entry, entry] }));
  assert.throws(() => decodeSavedRooms({ rooms: [{ ...entry, favorite: false }] }));
  assert.throws(() => decodeSavedRooms({ rooms: [{ ...entry, lastVisited: 'not a time' }] }));
});
