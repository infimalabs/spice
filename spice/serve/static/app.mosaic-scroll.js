// Mosaic plane scroll behavior: same-frame compensation when scrolled,
// gentle push at top. mosaicSyncPlane (app.mosaic-render.js) already takes
// {scrolled, onCompensate, replaying, reducedMotion} as its options; this
// module supplies {scrolled, onCompensate} from the real lane pane scroll
// container -- the serve pack host is
// its own scroll element inside a lane pane, not the window. `replaying`
// and `reducedMotion` come from the caller (full replay vs incremental
// insert is a render-path concern, not a scroll concern), so this module
// only ever returns the two fields it is actually responsible for.

const MOSAIC_NEAR_TOP_THRESHOLD_PX = 80;

// "At top" is a small threshold above zero, applied to the
// pack host's own scrollTop rather than window scroll.
function mosaicPaneIsScrolled(scrollTop, threshold) {
  const limit = Number.isFinite(threshold)
    ? threshold
    : MOSAIC_NEAR_TOP_THRESHOLD_PX;
  return scrollTop > limit;
}

// Builds the {scrolled, onCompensate} pair mosaicSyncPlane needs from a
// live lane. onCompensate lands the scrollTop adjustment through the
// pane's own scroll-intent suppression (setLaneScrollTopWithoutPaneIntent,
// app.shell.js), so the same-frame jump this compensates for is never
// misread as a user scroll gesture.
function mosaicPlaneScrollOptions(lane, threshold) {
  const host = lane.messagesEl;
  return {
    scrolled: mosaicPaneIsScrolled(host.scrollTop, threshold),
    onCompensate(delta) {
      setLaneScrollTopWithoutPaneIntent(lane, host.scrollTop + delta);
    },
  };
}
