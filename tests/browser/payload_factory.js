"use strict";

// One wire-shape authority for the serve browser smokes.
//
// serve streams presentation identity beside one canonical, faceted chrome
// envelope and a team snapshot. Restated inline, that shape drifts smoke-to-smoke;
// centralised here, a smoke declares only its ids, topology, and intentional
// deviations (via a deep-merge override hook).
//
// Two delivery modes share one definition:
//   - Node scope: `const F = require("./payload_factory")` then `F.targetPayload(...)`
//     for smokes that build a payload and pass it into `page.evaluate` as data.
//   - In page:   `page.addScriptTag({ content: F.installScript })` installs the
//     same builders as `window.spicePayloads` for smokes that build payloads
//     during evaluate (replacing per-smoke `builder.toString()` splicing).
//
// Every builder is a self-contained top-level routine that references only its
// siblings and its own literals, so `installScript` serialises the whole set
// (`FACTORY_HELPERS.map((fn) => fn.toString())`) into one page-side IIFE with no
// free module variables to smuggle across -- and no single mega-routine to keep.

// Actor-id prefixes mirror the production sources exactly and must stay in
// lockstep with them:
//   spice/serve/static/app.js   (target: / thread: actor ids)
//   spice/serve/team/ids.py
function targetActorId(id) {
  return "target:" + id;
}
function threadActorId(id) {
  return "thread:" + id;
}

function isPlainObject(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

// Deep-merge `overrides` onto a freshly-built canonical `base`: nested plain
// objects merge key-by-key so a caller overrides one leaf without restating a
// branch; arrays and primitives replace wholesale. The base is never mutated.
function withOverrides(base, overrides) {
  if (!isPlainObject(overrides)) return base;
  const merged = Object.assign({}, base);
  const keys = Object.keys(overrides);
  for (let i = 0; i < keys.length; i += 1) {
    const key = keys[i];
    const next = overrides[key];
    merged[key] =
      isPlainObject(next) && isPlainObject(merged[key])
        ? withOverrides(merged[key], next)
        : next;
  }
  return merged;
}

function chromeFacet(authority, revision, value, epoch) {
  return {
    authority: authority,
    order: {
      epoch: epoch === undefined ? "" : String(epoch),
      revision: revision,
    },
    value: value,
  };
}

function pendingInbox(revision) {
  return chromeFacet(
    "inbox",
    revision,
    { count: 0, label: "0", keys: [] },
  );
}

// A bound single-agent target payload as serve streams it to the browser.
// `options`: { id, threadId=id, teamId, teamRevision=1, configRevision=teamRevision,
//   worktreeName=id, branch=id, lifetime="Drive", pendingRevision=1,
//   taskBoardEpoch=1 }.
// `overrides` deep-merges over the canonical shape for intentional deviations.
function targetPayload(options, overrides) {
  const opts = options || {};
  const id = opts.id;
  const threadId = opts.threadId === undefined ? id : opts.threadId;
  const teamRevision = opts.teamRevision === undefined ? 1 : opts.teamRevision;
  const configRevision =
    opts.configRevision === undefined ? teamRevision : opts.configRevision;
  const worktreeName =
    opts.worktreeName === undefined ? id : opts.worktreeName;
  const branch = opts.branch === undefined ? id : opts.branch;
  const lifetime = opts.lifetime === undefined ? "Drive" : opts.lifetime;
  const pendingRevision =
    opts.pendingRevision === undefined ? 1 : opts.pendingRevision;
  const taskBoardEpoch =
    opts.taskBoardEpoch === undefined ? 1 : opts.taskBoardEpoch;
  const teamIdentity = {
    state: "member",
    teamId: opts.teamId,
    teamRevision: teamRevision,
    configRevision: configRevision,
  };
  const base = {
    id: id,
    name: id,
    branch: branch,
    targetIdentity: {
      targetId: id,
      worktreeName: worktreeName,
      branch: branch,
      driver: { name: "codex", model: "gpt-5.5", effort: "xhigh" },
      agent: { state: "unconfigured" },
      thread: { state: "bound", threadId: threadId },
    },
    serveAgentIdentity: {
      actorId: threadActorId(threadId),
      target: { id: id },
      thread: { state: "bound", threadId: threadId },
    },
    statusLine: {},
    chrome: {
      targetId: id,
      teamConfig: chromeFacet(
        "team-store",
        teamRevision,
        { teamIdentity: teamIdentity },
      ),
      pendingInbox: pendingInbox(pendingRevision),
      taskBoard: chromeFacet(
        "task-board",
        teamRevision,
        {
          taskFilters: [],
          taskFilterEntries: [],
          effectiveTaskFilters: [],
          taskFilterInventory: {
            revision: String(taskBoardEpoch),
            filters: [],
            primaryStems: [],
            openTaskCount: 0,
          },
          privateTaskCount: 0,
        },
        taskBoardEpoch,
      ),
      renewal: chromeFacet(
        "team-store",
        teamRevision,
        { lifetime: lifetime, renewalIntent: {} },
      ),
    },
  };
  return withOverrides(base, overrides);
}

// The inner team object carried inside a snapshot's `teams` array.
// `options`: { teamId, revision=1, lifetime="Drive", memberIds=[] }.
function teamPayload(options, overrides) {
  const opts = options || {};
  const revision = opts.revision === undefined ? 1 : opts.revision;
  const lifetime = opts.lifetime === undefined ? "Drive" : opts.lifetime;
  const memberIds = opts.memberIds || [];
  const base = {
    teamId: opts.teamId,
    revision: revision,
    config: {
      revision: revision,
      lifetime: lifetime,
      taskFilters: [],
      taskFilterEntries: [],
    },
    splitBack: {},
    members: memberIds.map(function (memberId) {
      return { agentId: targetActorId(memberId) };
    }),
  };
  return withOverrides(base, overrides);
}

// The `applyTeamSnapshotPayload` wrapper around one or more team objects.
// `options`: { revision, changed=true, teams=[], fastMode=false }.
function teamSnapshot(options, overrides) {
  const opts = options || {};
  const base = {
    revision: opts.revision,
    changed: opts.changed === undefined ? true : opts.changed,
    snapshot: {
      globalSettings: {
        fastMode: opts.fastMode === undefined ? false : opts.fastMode,
      },
      teams: opts.teams || [],
    },
  };
  return withOverrides(base, overrides);
}

// Every routine the injected builders reach, in dependency order. Serialising
// each `.toString()` into one IIFE keeps the page-side install self-contained
// without a mega-closure the complexity gate would (rightly) flag.
const FACTORY_HELPERS = [
  isPlainObject,
  withOverrides,
  chromeFacet,
  pendingInbox,
  targetActorId,
  threadActorId,
  targetPayload,
  teamPayload,
  teamSnapshot,
];

// The subset published on `window.spicePayloads` (and exported for Node).
const FACTORY_PUBLIC = [
  targetActorId,
  threadActorId,
  withOverrides,
  chromeFacet,
  pendingInbox,
  targetPayload,
  teamPayload,
  teamSnapshot,
];

// Injected mode: evaluate this in the page to install `window.spicePayloads`
// with the identical builder set. The IIFE gives each injection its own scope,
// so re-installing never collides on a redeclared name.
const installScript =
  "(function () {\n" +
  FACTORY_HELPERS.map(function (fn) {
    return fn.toString();
  }).join("\n") +
  "\nvar root = (window.spicePayloads = window.spicePayloads || {});\n" +
  FACTORY_PUBLIC.map(function (fn) {
    return "root." + fn.name + " = " + fn.name + ";";
  }).join("\n") +
  "\n})();";

module.exports = {
  targetActorId,
  threadActorId,
  withOverrides,
  chromeFacet,
  pendingInbox,
  targetPayload,
  teamPayload,
  teamSnapshot,
  installScript,
};
