export class RoomPasswordRequiredError extends Error {
  constructor() {
    super('This room requires a password');
    this.name = 'RoomPasswordRequiredError';
  }
}
