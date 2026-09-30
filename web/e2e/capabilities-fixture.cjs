/** Current public capabilities for browser fixtures with password accounts. */
const capabilities = Object.freeze({
  version: 1,
  accounts: true,
  passwordLogin: true,
  passkeyLogin: false,
  passwordRegistration: 'open',
  passkeyRegistration: 'disabled',
  roomDirectory: true,
  roomCreation: true,
  adHocRooms: true,
});

module.exports = { capabilities };
