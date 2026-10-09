export interface QuietHours {
  startMinute: number;
  endMinute: number;
  timeZone: string;
}

export interface NotificationPolicy {
  privateMessages: boolean;
  mentions: boolean;
  quietHours: QuietHours | null;
}

export interface ConversationNotificationPolicy {
  muted: boolean;
  snoozedUntil: string | null;
}

export interface ConversationNotificationPreference extends ConversationNotificationPolicy {
  peerId: string;
}

export interface NotificationPreferences extends NotificationPolicy {
  conversations: ConversationNotificationPreference[];
}

const record = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === 'object' && !Array.isArray(value);
const minute = (value: unknown): value is number =>
  typeof value === 'number' && Number.isInteger(value) && value >= 0 && value < 1440;
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function decodeNotificationPreferences(value: unknown): NotificationPreferences {
  const invalid = () => new Error('Invalid notification preferences response');
  if (
    !record(value) ||
    typeof value['privateMessages'] !== 'boolean' ||
    typeof value['mentions'] !== 'boolean' ||
    !Array.isArray(value['conversations']) ||
    value['conversations'].length > 100
  )
    throw invalid();
  let quietHours: QuietHours | null = null;
  const quiet = value['quietHours'];
  if (quiet !== null) {
    if (
      !record(quiet) ||
      !minute(quiet['startMinute']) ||
      !minute(quiet['endMinute']) ||
      quiet['startMinute'] === quiet['endMinute'] ||
      typeof quiet['timeZone'] !== 'string' ||
      !/^[A-Za-z0-9_+/-]{1,64}$/.test(quiet['timeZone'])
    )
      throw invalid();
    try {
      new Intl.DateTimeFormat('en', { timeZone: quiet['timeZone'] });
    } catch {
      throw invalid();
    }
    quietHours = {
      startMinute: quiet['startMinute'],
      endMinute: quiet['endMinute'],
      timeZone: quiet['timeZone'],
    };
  }
  const seen = new Set<string>();
  const conversations = value['conversations'].map(
    (entry: unknown): ConversationNotificationPreference => {
      if (
        !record(entry) ||
        typeof entry['peerId'] !== 'string' ||
        !uuid.test(entry['peerId']) ||
        seen.has(entry['peerId']) ||
        typeof entry['muted'] !== 'boolean' ||
        (entry['snoozedUntil'] !== null &&
          (typeof entry['snoozedUntil'] !== 'string' ||
            !/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,9})?(?:Z|[+-]\d\d:\d\d)$/.test(
              entry['snoozedUntil'],
            ) ||
            !Number.isFinite(Date.parse(entry['snoozedUntil']))))
      )
        throw invalid();
      seen.add(entry['peerId']);
      return {
        peerId: entry['peerId'],
        muted: entry['muted'],
        snoozedUntil: entry['snoozedUntil'],
      };
    },
  );
  return {
    privateMessages: value['privateMessages'],
    mentions: value['mentions'],
    quietHours,
    conversations,
  };
}

/** Start is inclusive, end is exclusive; a repeated DST minute is quiet twice. */
export function withinQuietHours(quiet: QuietHours | null, now = new Date()): boolean {
  if (!quiet) return false;
  try {
    const parts = new Intl.DateTimeFormat('en-GB', {
      timeZone: quiet.timeZone,
      hour: '2-digit',
      minute: '2-digit',
      hourCycle: 'h23',
    }).formatToParts(now);
    const value =
      Number(parts.find((part) => part.type === 'hour')?.value) * 60 +
      Number(parts.find((part) => part.type === 'minute')?.value);
    if (!Number.isFinite(value)) return true;
    return quiet.startMinute < quiet.endMinute
      ? value >= quiet.startMinute && value < quiet.endMinute
      : value >= quiet.startMinute || value < quiet.endMinute;
  } catch {
    // A browser missing a server-supported zone must not bypass chosen silence.
    return true;
  }
}

export function notificationAllowed(
  preferences: NotificationPreferences,
  kind: 'private' | 'mention' | 'room',
  peerId?: string,
  now = new Date(),
): boolean {
  if (withinQuietHours(preferences.quietHours, now)) return false;
  if (kind === 'mention') return preferences.mentions;
  if (kind === 'room') return true;
  if (!preferences.privateMessages) return false;
  const peer = preferences.conversations.find((entry) => entry.peerId === peerId);
  return !peer?.muted && (!peer?.snoozedUntil || Date.parse(peer.snoozedUntil) <= now.getTime());
}
