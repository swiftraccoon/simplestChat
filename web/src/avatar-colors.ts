export interface AvatarColors {
  readonly background: string;
  readonly color: '#000' | '#fff';
}

/**
 * The colors people choose to be recognized by, in the server's order
 * (`CHAT_COLORS` in protocol.rs). Each reaches at least 5:1 against the chat
 * and people-list surfaces, so a name or message in it stays readable.
 */
export const CHAT_PALETTE = {
  rose: '#fb7185',
  red: '#f87171',
  orange: '#fb923c',
  amber: '#fbbf24',
  lime: '#a3e635',
  green: '#4ade80',
  emerald: '#34d399',
  teal: '#2dd4bf',
  cyan: '#22d3ee',
  sky: '#38bdf8',
  blue: '#60a5fa',
  indigo: '#818cf8',
  violet: '#a78bfa',
  purple: '#c084fc',
  fuchsia: '#e879f9',
  pink: '#f472b6',
} as const;
export type ChatColor = keyof typeof CHAT_PALETTE;

/** Convert an opaque sRGB channel to linear light for relative luminance. */
function linearChannel(channel: number): number {
  return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
}

/** The more legible of black or white ink on an opaque sRGB background (channels 0–1). */
function ink(red: number, green: number, blue: number): '#000' | '#fff' {
  const luminance =
    0.2126 * linearChannel(red) + 0.7152 * linearChannel(green) + 0.0722 * linearChannel(blue);
  const blackContrast = (luminance + 0.05) / 0.05;
  const whiteContrast = 1.05 / (luminance + 0.05);
  return blackContrast >= whiteContrast ? '#000' : '#fff';
}

function channels(hex: string): [number, number, number] {
  return [1, 3, 5].map((start) => parseInt(hex.slice(start, start + 2), 16) / 255) as [
    number,
    number,
    number,
  ];
}

/**
 * Hash UTF-16 code units without normalizing names so existing colors,
 * including supplementary characters, remain unchanged.
 */
function nameHue(name: string): number {
  let hash = 0;
  for (let index = 0; index < name.length; index++) {
    hash = name.charCodeAt(index) + ((hash << 5) - hash);
  }
  return Math.abs(hash) % 360;
}

/** A palette entry by name; anything else (a newer palette's name included) is no choice. */
function chosenColor(chosen: string | null | undefined): string | undefined {
  return chosen && Object.prototype.hasOwnProperty.call(CHAT_PALETTE, chosen)
    ? CHAT_PALETTE[chosen as ChatColor]
    : undefined;
}

const PALETTE_HUES = Object.values(CHAT_PALETTE).map((hex) => {
  const [red, green, blue] = channels(hex);
  const max = Math.max(red, green, blue);
  const range = max - Math.min(red, green, blue);
  const sector =
    max === red
      ? (green - blue) / range
      : max === green
        ? 2 + (blue - red) / range
        : 4 + (red - green) / range;
  return { hex, hue: (sector * 60 + 360) % 360 };
});

/**
 * The color that identifies a person in chat, the people list and on their
 * video: the palette entry they chose, otherwise the entry nearest the hue of
 * their automatic avatar, so a name and its avatar always agree.
 */
export function chatColor(name: string, chosen?: string | null): string {
  const picked = chosenColor(chosen);
  if (picked) return picked;
  const hue = nameHue(name);
  let nearest = PALETTE_HUES[0]!;
  let distance = Infinity;
  for (const entry of PALETTE_HUES) {
    const gap = Math.abs(entry.hue - hue);
    const circular = Math.min(gap, 360 - gap);
    if (circular < distance) {
      nearest = entry;
      distance = circular;
    }
  }
  return nearest.hex;
}

/**
 * A chosen palette color fills the avatar. Otherwise preserve the existing
 * name-derived HSL background. Either way select the more legible black or
 * white initial.
 */
export function avatarColors(name: string, chosen?: string | null): AvatarColors {
  const picked = chosenColor(chosen);
  if (picked) return { background: picked, color: ink(...channels(picked)) };
  const hue = nameHue(name);
  const saturation = 0.65;
  const lightness = 0.55;
  const chroma = (1 - Math.abs(2 * lightness - 1)) * saturation;
  const secondary = chroma * (1 - Math.abs(((hue / 60) % 2) - 1));
  const offset = lightness - chroma / 2;
  const [red, green, blue]: readonly [number, number, number] =
    hue < 60
      ? [chroma, secondary, 0]
      : hue < 120
        ? [secondary, chroma, 0]
        : hue < 180
          ? [0, chroma, secondary]
          : hue < 240
            ? [0, secondary, chroma]
            : hue < 300
              ? [secondary, 0, chroma]
              : [chroma, 0, secondary];
  return {
    background: `hsl(${hue}, 65%, 55%)`,
    color: ink(red + offset, green + offset, blue + offset),
  };
}
