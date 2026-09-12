export interface AvatarColors {
  readonly background: string;
  readonly color: '#000' | '#fff';
}

/** Convert an opaque sRGB channel to linear light for relative luminance. */
function linearChannel(channel: number): number {
  return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
}

/**
 * Preserve the existing name-derived HSL background and select the more legible
 * black or white initial. Hash UTF-16 code units without normalizing names so
 * existing colors, including supplementary characters, remain unchanged.
 */
export function avatarColors(name: string): AvatarColors {
  let hash = 0;
  for (let index = 0; index < name.length; index++) {
    hash = name.charCodeAt(index) + ((hash << 5) - hash);
  }
  const hue = Math.abs(hash) % 360;
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
  const luminance =
    0.2126 * linearChannel(red + offset) +
    0.7152 * linearChannel(green + offset) +
    0.0722 * linearChannel(blue + offset);
  const blackContrast = (luminance + 0.05) / 0.05;
  const whiteContrast = 1.05 / (luminance + 0.05);
  return {
    background: `hsl(${hue}, 65%, 55%)`,
    color: blackContrast >= whiteContrast ? '#000' : '#fff',
  };
}
