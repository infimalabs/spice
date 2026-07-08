// Isolated-lane fixture: the standard way for a browser smoke to get a lane.
//
// Deliberately bypasses addLane()/targets: those attach to a REAL target's
// live-bus (subscribeLaneToLiveBus), which in a dev sandbox running
// alongside other live agents pulls in real, independently-growing
// conversation data -- exactly the kind of background noise a smoke must
// not be exposed to. addEmptyTeamLane() builds the same lane shape
// (messagesEl, knownMessages, shardTextareas, etc.) without ever
// subscribing to a backend; flipping emptyTeam off afterward routes it
// through the normal (non-empty-team) render paths under full control of
// the smoke's own message pushes. syncEmptyTeamLane guards on
// lane.emptyTeam, so later team-snapshot syncs never reset the lane.
//
// installIsolatedLaneFixture waits for the first server lane (".lane") so
// the initial team snapshot has already been reconciled before the smoke
// fabricates its lane -- applyTeamSnapshotPayload closes lanes it does not
// recognize, so creating the fixture lane before that first snapshot would
// race a reconciliation close. Snapshots after boot only arrive on team
// changes, which smokes do not trigger.
//
// The one deliberate exception to this fixture is
// serve_task_card_live_smoke.js: its entire subject is live-bus delivery to
// a lane bound to a real thread, so it must keep addLane() on a real
// target.

// Upper bound, not a wait: boot is usually ~2s, but the shared sandbox can
// stall past 10s when sibling agents load the machine.
const isolatedLaneReadyTimeoutMs = 20000;

const isolatedLaneRequiredGlobals = [
  "addEmptyTeamLane",
  "emptyTeamTargetId",
  "laneStates",
];

const isolatedLaneGlobalNamePattern = /^[A-Za-z_$][A-Za-z0-9_$]*$/;

// Page-side: resolve (creating on first use) the isolated lane for teamId.
// Idempotent per teamId; smokes needing several lanes pass distinct ids.
function resolveIsolatedLane(teamId) {
  const targetId = emptyTeamTargetId(teamId);
  if (!laneStates.has(targetId)) {
    addEmptyTeamLane({ teamId, revision: 1, config: {} });
    const created = laneStates.get(targetId);
    if (created) {
      // addEmptyTeamLane's sync pass rendered the empty-team "import agent"
      // panel into messagesEl -- DOM built from the REAL sandbox target
      // list, which the mosaic render path never removes (it only manages
      // its own plane/extent). Clear it once at creation so nothing derived
      // from live targets pollutes the host or its scroll extents.
      created.messagesEl.replaceChildren(created.historySentinelEl);
      created.renderedMessageFingerprint = "";
    }
  }
  const lane = laneStates.get(targetId);
  if (!lane)
    throw new Error("failed to create isolated lane for team " + teamId);
  lane.emptyTeam = false;
  return lane;
}

// Waits for app boot (first team-snapshot lane plus the fixture's page
// globals, and any extra globals the smoke needs), then injects
// resolveIsolatedLane into the page. app globals declared with const/let
// are not window properties, so readiness is a bare-identifier expression.
async function installIsolatedLaneFixture(page, options = {}) {
  const globals = [
    ...isolatedLaneRequiredGlobals,
    ...(options.globals || []),
  ];
  for (const name of globals) {
    if (!isolatedLaneGlobalNamePattern.test(name))
      throw new Error("invalid page global name: " + name);
  }
  const timeout = options.timeoutMs || isolatedLaneReadyTimeoutMs;
  await page.waitForSelector(".lane", { timeout });
  await page.waitForFunction(
    globals.map((name) => 'typeof ' + name + ' !== "undefined"').join(" && "),
    null,
    { timeout },
  );
  await page.addScriptTag({ content: resolveIsolatedLane.toString() });
}

module.exports = { installIsolatedLaneFixture, resolveIsolatedLane };
