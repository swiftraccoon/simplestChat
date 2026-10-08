import type { ChatEntry } from './protocol';
import { decodeChatEntry, decodeHistoryRetention } from './protocol-validation';
import { integer, invalid, list, nullable, object, text } from './validation';

export interface InboxConversation {
  peerId: string;
  peerName: string;
  lastMessage: ChatEntry;
  unreadCount: number;
}
export interface InboxPage {
  conversations: InboxConversation[];
  nextCursor: string | null;
  retentionDays: number;
}
export const decodeInbox = object<InboxPage>({
  conversations: (value) => {
    const items = list(
      object<InboxConversation>({
        peerId: text,
        peerName: text,
        lastMessage: decodeChatEntry,
        unreadCount: integer(),
      }),
    )(value);
    return items.length <= 100 ? items : invalid();
  },
  nextCursor: nullable(text),
  retentionDays: decodeHistoryRetention,
});
export const decodeChatRead = object<{ readMessageId: string | null }>({
  readMessageId: nullable(text),
});

export const decodeInboxUnread = object<{ unreadCount: number }>({ unreadCount: integer(1000) });
