import type { ChatStyle, ChatStyleKind, TimestampFormat } from '../protocol';
import { CHAT_PALETTE } from '../avatar-colors';

export const LOOKS: readonly { kind: ChatStyleKind; label: string }[] = [
  { kind: 'accent', label: 'Stripe' },
  { kind: 'text', label: 'Colored text' },
  { kind: 'bubble', label: 'Tinted bubble' },
];
export const AUTOMATIC_LOOK: ChatStyle = { color: null, style: 'accent' };

/** The start of a message on one line, as a reply quotes it. */
export function excerpt(text: string): string {
  const flat = text.split(/\s+/).join(' ').trim();
  return flat.length > 140 ? `${flat.slice(0, 140).trimEnd()}…` : flat;
}

/** A stored or typed look reduced to the palette and the known styles, like the server does. */
export function validLook(value: unknown): ChatStyle | null {
  if (typeof value !== 'object' || value === null) return null;
  const { color, style } = value as Record<string, unknown>;
  return {
    color:
      typeof color === 'string' && Object.prototype.hasOwnProperty.call(CHAT_PALETTE, color)
        ? color
        : null,
    style: LOOKS.some((look) => look.kind === style) ? (style as ChatStyleKind) : 'accent',
  };
}

const pad = (value: number): string => String(value).padStart(2, '0');

/** A message time in the viewer's format, always in their local time. */
export function formatChatTime(date: Date, format: TimestampFormat): string {
  const time = `${pad(date.getHours())}:${pad(date.getMinutes())}`;
  switch (format) {
    case 'time':
      return time;
    case 'time12':
      return date.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit', hour12: true });
    case 'seconds':
      return `${time}:${pad(date.getSeconds())}`;
    case 'datetime':
      return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${time}:${pad(date.getSeconds())}`;
    case 'hover':
      return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }
}
