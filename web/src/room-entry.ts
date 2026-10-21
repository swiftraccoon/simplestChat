/** A stable warm-palette ground for each room tile. */
export function roomTone(id: string): string {
  const tones = [
    '#2f3a2a',
    '#3a2d2a',
    '#3a3328',
    '#3d3028',
    '#313a2e',
    '#2a2a2a',
    '#3b2f2f',
    '#24211f',
  ];
  let hash = 0;
  for (const char of id) hash = (hash * 31 + (char.codePointAt(0) ?? 0)) >>> 0;
  return tones[hash % tones.length] ?? tones[0]!;
}

interface DirectoryEntryContext {
  readonly nameInput: Pick<HTMLInputElement, 'value' | 'focus'>;
  getDisplayName: () => string | null;
  requestJoin: (id: string) => void;
  selectRoom: (id: string) => void;
  updateJoin: () => void;
}

/** A tile enters once a name is known; otherwise it selects the room and asks for the name. */
export function createDirectoryEntry(context: DirectoryEntryContext): (id: string) => void {
  return (id) => {
    const name = context.getDisplayName();
    if (name && !context.nameInput.value.trim()) context.nameInput.value = name;
    if (context.nameInput.value.trim()) context.requestJoin(id);
    else {
      context.selectRoom(id);
      context.nameInput.focus();
    }
    context.updateJoin();
  };
}
