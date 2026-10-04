/** Cameras, screen shares and audio-only participants use identical 16:9 cells. */
const TILE_RATIO = 16 / 9;
const MIN_HEIGHT = 96;

export interface VideoTileSize {
  width: number;
  height: number;
}

function rowCount(count: number, height: number, width: number, gap: number): number {
  const columns = Math.max(1, Math.floor((width + gap) / (TILE_RATIO * height + gap)));
  return Math.ceil(count / columns);
}

function commonHeight(count: number, width: number, height: number, gap: number): number {
  const maximum = width / TILE_RATIO;
  const minimum = Math.min(MIN_HEIGHT, maximum);
  const fits = (candidate: number, maxRows = Number.POSITIVE_INFINITY): boolean => {
    const rows = rowCount(count, candidate, width, gap);
    return rows <= maxRows && rows * candidate + (rows - 1) * gap <= height;
  };
  // Crowded rooms keep readable tiles and scroll from the first row.
  if (!fits(minimum)) return minimum;
  const search = (upper: number, maxRows = Number.POSITIVE_INFINITY): number => {
    let lower = minimum;
    for (let step = 0; step < 24; step++) {
      const middle = (lower + upper) / 2;
      if (fits(middle, maxRows)) lower = middle;
      else upper = middle;
    }
    return lower;
  };
  const largest = search(Math.min(maximum, height));
  // Avoid an extra sparse row for a negligible increase in tile size.
  const compactRows = rowCount(count, Math.max(minimum, largest * 0.92), width, gap);
  return search(largest, compactRows);
}

/** Source aspect and pin state never affect a tile's dimensions. */
export function calculateVideoLayout(
  count: number,
  width: number,
  height: number,
  gap = 8,
): VideoTileSize[] {
  if (!Number.isInteger(count) || count <= 0) return [];
  if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 1 || height <= 0)
    return Array.from({ length: count }, () => ({ width: 0, height: 0 }));
  // Leave one CSS pixel for flex's subpixel rounding across a wrapped row.
  const available = width - 1;
  const spacing = Number.isFinite(gap) && gap >= 0 ? gap : 8;
  const tileHeight = commonHeight(count, available, height, spacing);
  return Array.from({ length: count }, () => ({
    width: TILE_RATIO * tileHeight,
    height: tileHeight,
  }));
}

/** Observe this page-lifetime stage without reacting to our own style writes. */
export function observeVideoLayout(grid: HTMLElement): void {
  let queued = false;
  const schedule = (): void => {
    if (queued) return;
    queued = true;
    requestAnimationFrame(layout);
  };
  const layout = (): void => {
    queued = false;
    const tiles = Array.from(grid.children).filter(
      (child): child is HTMLElement =>
        child instanceof HTMLElement && child.classList.contains('video-tile'),
    );
    const style = getComputedStyle(grid);
    const width = grid.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
    const height =
      grid.clientHeight - parseFloat(style.paddingTop) - parseFloat(style.paddingBottom);
    if (width <= 1 || height <= 0) return;
    const sizes = calculateVideoLayout(tiles.length, width, height, parseFloat(style.columnGap));
    tiles.forEach((tile, index) => {
      const size = sizes[index];
      if (!size) return;
      tile.style.setProperty('--video-tile-width', `${size.width}px`);
      tile.style.setProperty('--video-tile-height', `${size.height}px`);
    });
  };
  new ResizeObserver(schedule).observe(grid);
  new MutationObserver(schedule).observe(grid, { childList: true });
  schedule();
}
