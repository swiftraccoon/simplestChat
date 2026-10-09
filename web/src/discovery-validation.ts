import type { RoomListItem } from './protocol';
import { decodeRoomListItem } from './api-validation';
import { boolean, choice, invalid, list, nullable, record, text } from './validation';

export interface Contact {
  accountId: string;
  accountName: string;
  status: 'accepted' | 'incoming' | 'outgoing';
}
export interface ContactsPage {
  accountId: string;
  contacts: Contact[];
}
export interface SavedRoom {
  room: RoomListItem;
  favorite: boolean;
  lastVisited: string | null;
}
export interface SavedRoomsPage {
  rooms: SavedRoom[];
}

export function contactId(value: unknown): string {
  const result = text(value);
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(result)
    ? result.toLowerCase()
    : invalid();
}

export function decodeContacts(value: unknown): ContactsPage {
  const source = record(value);
  const accountId = contactId(source['accountId']);
  const contacts = list((value: unknown): Contact => {
    const entry = record(value);
    const accountName = text(entry['accountName']);
    if (!accountName.length || new TextEncoder().encode(accountName).length > 64) invalid();
    return {
      accountId: contactId(entry['accountId']),
      accountName,
      status: choice('accepted', 'incoming', 'outgoing')(entry['status']),
    };
  })(source['contacts']);
  if (
    contacts.length > 100 ||
    contacts.some((contact) => contact.accountId === accountId) ||
    new Set(contacts.map((contact) => contact.accountId)).size !== contacts.length
  )
    invalid();
  return { accountId, contacts };
}

export function decodeSavedRooms(value: unknown): SavedRoomsPage {
  const rooms = list((value: unknown): SavedRoom => {
    const entry = record(value);
    const lastVisited = nullable(text)(entry['lastVisited']);
    if (
      lastVisited !== null &&
      (lastVisited.length > 40 || !Number.isFinite(Date.parse(lastVisited)))
    )
      invalid();
    return {
      room: decodeRoomListItem(entry['room']),
      favorite: boolean(entry['favorite']),
      lastVisited,
    };
  })(record(value)['rooms']);
  if (
    rooms.length > 150 ||
    rooms.filter((room) => room.favorite).length > 100 ||
    rooms.filter((room) => !room.favorite).length > 50 ||
    rooms.some((room) => !room.favorite && room.lastVisited === null) ||
    new Set(rooms.map((room) => room.room.id)).size !== rooms.length
  )
    invalid();
  return { rooms };
}
