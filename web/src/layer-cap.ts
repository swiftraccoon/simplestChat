/**
 * Simulcast layer caps from what a tile can actually show. Publishers send
 * three layers scaled by 4, 2 and 1 from the capture width; a tile rendered
 * at 200 CSS pixels gains nothing from a 1280-pixel layer, and every viewer
 * that asks for it spends downlink the congestion controller then has to take
 * back from everyone. The cap is a ceiling: the server still lowers layers
 * below it when the estimate demands.
 */

/** Widths in device pixels the three layers of a typical 720p capture deliver. */
export const LAYER_WIDTHS = [320, 640, 1280] as const;
/** Step up only when the tile clearly outgrows a layer, down only well below it. */
const UP_MARGIN = 1.15;
const DOWN_MARGIN = 0.85;

/**
 * Spatial layer a tile of `renderedWidth` CSS pixels needs at `pixelRatio`,
 * given the layer it currently has; hysteresis keeps a tile that hovers at a
 * boundary from flapping.
 */
export function spatialLayerForRenderedWidth(
  renderedWidth: number,
  pixelRatio: number,
  current: number | null,
): number {
  const needed = Math.max(0, renderedWidth) * Math.max(1, pixelRatio || 1);
  const top = LAYER_WIDTHS.length - 1;
  const fresh = LAYER_WIDTHS.findIndex((width) => needed <= width);
  const target = fresh < 0 ? top : fresh;
  if (current === null || current < 0 || current > top) return target;
  const currentWidth = LAYER_WIDTHS[current] ?? 1280;
  const targetWidth = LAYER_WIDTHS[target] ?? 1280;
  if (target > current) {
    // Move up only when the tile exceeds the current layer's width by the margin.
    return needed > currentWidth * UP_MARGIN ? target : current;
  }
  if (target < current) {
    // Move down only when the tile fits the lower layer with room to spare.
    return needed < targetWidth * DOWN_MARGIN ? target : current;
  }
  return current;
}
