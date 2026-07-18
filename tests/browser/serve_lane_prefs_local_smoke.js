const { withServePage } = require("./serve_playwright_harness");

// Narration and view prefs are browser-local interface hints, never shared
// TeamConfig state: a lane mounted from the team snapshot initializes
// speechMode/selectedView from the stored hint when one exists and from the
// defaults otherwise; the setters persist per-target hints; and none of it
// moves the team's config revision or sends a team command.

const PREFS_MEMBER_IDS = ["pref-a", "pref-b"];
const PREFS_SNAPSHOT_REVISION = 50;
const PREFS_CONFIG_REVISION = 4;
const PREFS_HINT = { speechMode: "narrate", selectedView: "metrics" };
const PREFS_CHANGED = { speechMode: "quiet", selectedView: "filters" };
const PREFS_SETTLE_MS = 60;

function prefsTargetPayload(id) {
  return {
    id,
    name: id,
    branch: id,
    targetIdentity: {
      targetId: id,
      worktreeName: id,
      branch: id,
      driver: { name: "codex", model: "gpt-5.5", effort: "xhigh" },
      agent: { state: "unconfigured" },
      thread: { state: "bound", threadId: id + "-th" },
    },
    serveAgentIdentity: {
      actorId: "thread:" + id + "-th",
      target: { id },
      thread: { state: "bound", threadId: id + "-th" },
    },
    teamIdentity: {
      state: "member",
      teamId: "team-" + id,
      teamRevision: 1,
      configRevision: 1,
    },
    taskFilters: [],
    laneFilterVersion: "",
    lifetime: "Drive",
    statusLine: {
      pendingInboxCount: 0,
      pendingInboxKeys: [],
      pendingInboxRevision: "p-" + id,
      pendingInboxVersion: 1,
    },
  };
}

// The reduced TeamConfig payload shape: team facts only, no interface prefs.
function prefsTeam(id, configRevision) {
  return {
    teamId: "team-" + id,
    revision: 1,
    config: {
      revision: configRevision,
      lifetime: "Drive",
      taskFilters: [],
      taskFilterEntries: [],
      shellSettings: {},
    },
    splitBack: {},
    members: [{ agentId: "target:" + id }],
  };
}

function prefsStatusLine(id) {
  return {
    pendingInboxCount: 0,
    pendingInboxKeys: [],
    pendingInboxRevision: "p-" + id,
    pendingInboxVersion: 1,
  };
}

function prefsSettle(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// Stub the transport before any lane mounts and record every frame so the
// measure can positively count zero teams.command sends.
function prefsInstallStubs(state) {
  // eslint-disable-next-line no-global-assign
  liveBusRequest = async (type, fields = {}) => {
    state.frames.push({ type, ...fields });
    if (type !== "lanes.subscribe") return { type: "bus.ok", payload: {} };
    return {
      type: "lanes.payload",
      lanes: (fields.entries || []).map((entry) => {
        return {
          targetId: entry.targetId,
          subscriptionGeneration: "lane-prefs:" + entry.targetId,
          watcherActive: true,
          watcherError: "",
          payload: {
            messages: [],
            statusLine: window.__prefsStatusLine(entry.targetId),
          },
        };
      }),
    };
  };
}

function prefsApplySnapshot(config, revision) {
  applyTeamSnapshotPayload(
    {
      revision,
      changed: true,
      snapshot: {
        globalSettings: { fastMode: false },
        teams: config.memberIds.map((id) => {
          return window.__prefsTeam(id, config.configRevision);
        }),
      },
    },
    { force: true },
  );
}

function prefsStoredHint(targetId) {
  const hints = JSON.parse(localStorage.getItem(laneStorageKey) || "[]");
  return hints.find((hint) => hint.targetId === targetId) || null;
}

async function prefsMeasure(config) {
  const state = { frames: [] };
  window.__prefsInstallStubs(state);
  laneStore.replaceTargets(
    config.memberIds.map((id) => window.__prefsTarget(id)),
  );
  localStorage.setItem(
    laneStorageKey,
    JSON.stringify([{ targetId: config.memberIds[0], ...config.hint }]),
  );
  window.__prefsApplySnapshot(config, config.snapshotRevision);
  await window.__prefsSettle(config.settleMs);
  const laneA = laneStates.get(config.memberIds[0]);
  const laneB = laneStates.get(config.memberIds[1]);
  const mounted = {
    hintSpeechMode: laneA.speechMode,
    hintSelectedView: laneA.selectedView,
    unhintedSpeechMode: laneB.speechMode,
    unhintedSelectedView: laneB.selectedView,
    pageDefaultSpeechMode: defaultSpeechMode,
    pageDefaultSelectedView: defaultLaneViewMode,
    revisionBefore: laneB.configRevision,
  };
  setLaneSpeechMode(laneB, config.changed.speechMode);
  setLaneSelectedView(laneB, config.changed.selectedView);
  window.__prefsApplySnapshot(config, config.snapshotRevision + 1);
  await window.__prefsSettle(config.settleMs);
  return {
    ...mounted,
    storedHintA: window.__prefsStoredHint(config.memberIds[0]),
    storedHintB: window.__prefsStoredHint(config.memberIds[1]),
    revisionAfter: laneB.configRevision,
    speechAfterResnapshot: laneB.speechMode,
    viewAfterResnapshot: laneB.selectedView,
    teamCommandCount: state.frames.filter((frame) => {
      return frame.type === "teams.command";
    }).length,
  };
}

const PREFS_PAGE_HELPERS = {
  __prefsTarget: prefsTargetPayload,
  __prefsTeam: prefsTeam,
  __prefsStatusLine: prefsStatusLine,
  __prefsSettle: prefsSettle,
  __prefsInstallStubs: prefsInstallStubs,
  __prefsApplySnapshot: prefsApplySnapshot,
  __prefsStoredHint: prefsStoredHint,
};

function prefsPageHelperScript() {
  return Object.entries(PREFS_PAGE_HELPERS)
    .map(([name, helper]) => "window." + name + " = " + helper.toString() + ";")
    .join("\n");
}

function assertPrefsResult(result) {
  const hintA = JSON.stringify({
    targetId: PREFS_MEMBER_IDS[0],
    ...PREFS_HINT,
  });
  const hintB = JSON.stringify({
    targetId: PREFS_MEMBER_IDS[1],
    ...PREFS_CHANGED,
  });
  const failures = {
    "hinted lane did not mount with its stored interface prefs":
      result.hintSpeechMode !== PREFS_HINT.speechMode ||
      result.hintSelectedView !== PREFS_HINT.selectedView,
    "unhinted lane did not mount with interface defaults":
      result.unhintedSpeechMode !== result.pageDefaultSpeechMode ||
      result.unhintedSelectedView !== result.pageDefaultSelectedView,
    "hint and default mount branches did not differentiate":
      result.hintSpeechMode === result.unhintedSpeechMode,
    "hinted lane's stored hint did not survive the mount":
      JSON.stringify(result.storedHintA) !== hintA,
    "setter did not persist the browser-local hint":
      JSON.stringify(result.storedHintB) !== hintB,
    "interface pref change moved the team config revision":
      result.revisionAfter !== result.revisionBefore ||
      result.revisionBefore !== PREFS_CONFIG_REVISION,
    "local prefs did not survive the following snapshot read":
      result.speechAfterResnapshot !== PREFS_CHANGED.speechMode ||
      result.viewAfterResnapshot !== PREFS_CHANGED.selectedView,
    "interface pref change sent a team command":
      result.teamCommandCount !== 0,
  };
  for (const [message, failed] of Object.entries(failures)) {
    if (failed) throw new Error(message + ": " + JSON.stringify(result));
  }
}

async function run() {
  return withServePage(
    {
      path: "/?smoke=lane-prefs-local-" + Date.now(),
      contextOptions: { viewport: { width: 1600, height: 1000 } },
    },
    async ({ page }) => {
      await page.waitForFunction(
        () =>
          typeof applyTeamSnapshotPayload === "function" &&
          typeof setLaneSpeechMode === "function" &&
          typeof setLaneSelectedView === "function" &&
          typeof laneStorageKey === "string",
        { timeout: 10000 },
      );
      await page.addScriptTag({ content: prefsPageHelperScript() });
      const result = await page.evaluate(prefsMeasure, {
        memberIds: PREFS_MEMBER_IDS,
        snapshotRevision: PREFS_SNAPSHOT_REVISION,
        configRevision: PREFS_CONFIG_REVISION,
        hint: PREFS_HINT,
        changed: PREFS_CHANGED,
        settleMs: PREFS_SETTLE_MS,
      });
      assertPrefsResult(result);
      return result;
    },
  );
}

run().then(
  (result) => {
    console.log("serve_lane_prefs_local_smoke ok " + JSON.stringify(result));
  },
  (error) => {
    console.error(error);
    process.exit(1);
  },
);
