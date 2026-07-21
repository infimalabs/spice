// Every *_smoke.js scenario belongs in exactly one list. Release-safe scenarios
// use scratch serve state or page-local fixtures; external-state scenarios stay
// explicit so adding a smoke can never silently skip release validation.
const releaseSafe = [
  { path: "serve_composer_accent_smoke.js" },
  { path: "serve_composer_menu_order_smoke.js" },
  { path: "serve_composer_pending_send_smoke.js" },
  { path: "serve_composer_reorder_smoke.js" },
  { path: "serve_composer_typing_latency_smoke.js", serial: true },
  { path: "serve_fresh_startup_import_shell_smoke.js" },
  { path: "serve_identity_smoke.js" },
  { path: "serve_lane_prefs_local_smoke.js" },
  { path: "serve_lane_reload_smoke.js" },
  { path: "serve_lanes_batch_subscribe_smoke.js" },
  { path: "serve_lifetime_reflow_smoke.js" },
  { path: "serve_lifetime_team_smoke.js" },
  { path: "serve_live_text_slot_smoke.js" },
  { path: "serve_menu_smoke.js" },
  { path: "serve_mobile_layout_smoke.js" },
  { path: "serve_mosaic_ack_resolution_smoke.js" },
  { path: "serve_mosaic_capacity_smoke.js" },
  { path: "serve_mosaic_hidden_reveal_smoke.js" },
  { path: "serve_mosaic_hydration_smoke.js" },
  { path: "serve_mosaic_join_smoke.js" },
  { path: "serve_mosaic_performance_smoke.js", serial: true },
  { path: "serve_mosaic_reduced_motion_smoke.js" },
  { path: "serve_mosaic_render_smoke.js" },
  { path: "serve_mosaic_reservations_smoke.js" },
  { path: "serve_mosaic_scroll_smoke.js" },
  { path: "serve_mosaic_seam_smoke.js" },
  { path: "serve_mosaic_single_column_smoke.js" },
  { path: "serve_mosaic_single_settle_smoke.js" },
  { path: "serve_mosaic_sizing_smoke.js" },
  { path: "serve_mosaic_span_smoke.js" },
  { path: "serve_mosaic_stream_smoke.js" },
  { path: "serve_mosaic_team_smoke.js" },
  { path: "serve_nack_render_smoke.js" },
  { path: "serve_pending_badge_smoke.js", serial: true },
  { path: "serve_reply_card_smoke.js" },
  { path: "serve_scrollbar_gutter_smoke.js" },
  { path: "serve_structural_status_smoke.js", serial: true },
  { path: "serve_submission_lifecycle_smoke.js", serial: true },
  { path: "serve_submit_latency_smoke.js", serial: true },
  { path: "serve_task_filter_hidden_stems_live_smoke.js" },
  { path: "serve_task_filter_pills_smoke.js" },
  { path: "serve_team_metrics_smoke.js" },
  { path: "serve_team_width_smoke.js" },
  { path: "serve_watch_smoke.js" },
];

const externalState = [
  {
    path: "serve_task_card_live_smoke.js",
    reason:
      "creates a task through a bound live lane; run explicitly when validating live task-card delivery",
  },
];

module.exports = { externalState, releaseSafe };
