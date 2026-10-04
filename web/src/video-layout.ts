/** All video sources share one aspect-preserving, height-matched layout. */
const DEFAULT_RATIO = 16 / 9;
const MIN_HEIGHT = 96;

export interface VideoTileSize {
  width: number;
  height: number;
}

function rowCount(ratios: readonly number[], height: number, width: number, gap: number): number {
  let rows = 1;
  let occupied = 0;
  for (const ratio of ratios) {
    const next = ratio * height;
    if (occupied > 0 && occupied + gap + next > width) {
      rows++;
      occupied = next;
    } else {
      occupied += (occupied > 0 ? gap : 0) + next;
    }
  }
  return rows;
}

function commonHeight(
  ratios: readonly number[],
  width: number,
  height: number,
  gap: number,
): number {
  const widest = ratios.reduce((maximum, ratio) => Math.max(maximum, ratio), 0);
  const maximum = width / widest;
  const minimum = Math.min(MIN_HEIGHT, maximum);
  const fits = (candidate: number, maxRows = Number.POSITIVE_INFINITY): boolean => {
    const rows = rowCount(ratios, candidate, width, gap);
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
  const compactRows = rowCount(ratios, Math.max(minimum, largest * 0.92), width, gap);
  return search(largest, compactRows);
}

/** Sizes remain in DOM order. Only explicit pinning changes the visual order. */
export function calculateVideoLayout(
  aspects: readonly number[],
  width: number,
  height: number,
  gap = 8,
  pinnedIndex = -1,
): VideoTileSize[] {
  const ratios = aspects.map((ratio) =>
    Number.isFinite(ratio) && ratio > 0 ? ratio : DEFAULT_RATIO,
  );
  if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 1 || height <= 0)
    return ratios.map(() => ({ width: 0, height: 0 }));
  if (ratios.length === 0) return [];
  // Leave one CSS pixel for flex's subpixel rounding across a wrapped row.
  const available = width - 1;
  const spacing = Number.isFinite(gap) && gap >= 0 ? gap : 8;
  const pinnedRatio = ratios[pinnedIndex];
  if (pinnedRatio !== undefined && ratios.length > 1) {
    const pinnedHeight = Math.min(height * 0.68, available / pinnedRatio);
    const others = ratios.filter((_, index) => index !== pinnedIndex);
    const stripHeight = commonHeight(others, available, height - pinnedHeight - spacing, spacing);
    return ratios.map((ratio, index) => {
      const tileHeight = index === pinnedIndex ? pinnedHeight : stripHeight;
      return { width: ratio * tileHeight, height: tileHeight };
    });
  }
  const tileHeight = commonHeight(ratios, available, height, spacing);
  return ratios.map((ratio) => ({ width: ratio * tileHeight, height: tileHeight }));
}

/** Observe this page-lifetime stage without reacting to our own style writes. */
export function observeVideoLayout(grid: HTMLElement): void {
  let queued = false;
  const videos = new Set<HTMLVideoElement>();
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
    const currentVideos = new Set<HTMLVideoElement>();
    const ratios = tiles.map((tile) => {
      const video = tile.querySelector('video');
      if (!video) return DEFAULT_RATIO;
      currentVideos.add(video);
      if (!videos.has(video)) {
        video.addEventListener('loadedmetadata', schedule);
        video.addEventListener('resize', schedule);
        videos.add(video);
      }
      return video.videoWidth > 0 && video.videoHeight > 0
        ? video.videoWidth / video.videoHeight
        : DEFAULT_RATIO;
    });
    for (const video of videos) {
      if (currentVideos.has(video)) continue;
      video.removeEventListener('loadedmetadata', schedule);
      video.removeEventListener('resize', schedule);
      videos.delete(video);
    }
    const style = getComputedStyle(grid);
    const width = grid.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
    const height =
      grid.clientHeight - parseFloat(style.paddingTop) - parseFloat(style.paddingBottom);
    if (width <= 1 || height <= 0) return;
    const sizes = calculateVideoLayout(
      ratios,
      width,
      height,
      parseFloat(style.columnGap),
      tiles.findIndex((tile) => tile.classList.contains('pinned')),
    );
    tiles.forEach((tile, index) => {
      const size = sizes[index];
      if (!size) return;
      tile.style.setProperty('--video-tile-width', `${size.width}px`);
      tile.style.setProperty('--video-tile-height', `${size.height}px`);
    });
  };
  new ResizeObserver(schedule).observe(grid);
  new MutationObserver((records) => {
    if (
      records.some(
        (record) =>
          record.type === 'childList' ||
          (record.target instanceof HTMLElement &&
            record.target.parentElement === grid &&
            (record.oldValue ?? '').split(/\s+/).includes('pinned') !==
              record.target.classList.contains('pinned')),
      )
    )
      schedule();
  }).observe(grid, {
    childList: true,
    subtree: true,
    attributes: true,
    attributeFilter: ['class'],
    attributeOldValue: true,
  });
  schedule();
}
