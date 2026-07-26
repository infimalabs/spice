// Explicit state authority for serve lanes and their target inventory.

"use strict";

// Lane chrome is a faceted projection of server state: each facet has exactly
// one authority, so every channel -- target inventory, a lane push, a team
// snapshot, a renewal, a pending-only live-bus event, route feedback -- carries
// only the facets its authority owns, and any of them can arrive first, twice,
// or out of order. Mirrors LANE_CHROME_FACET_AUTHORITIES in serve/payload/wire.
const LANE_CHROME_FACET_AUTHORITIES = Object.freeze({
  identity: "target-registry",
  teamConfig: "team-store",
  pendingInbox: "inbox",
  taskBoard: "task-board",
  lifecycle: "lifecycle-reconciler",
  renewal: "team-store",
  activity: "transcript",
});

const LANE_CHROME_FACETS = Object.freeze(
  Object.keys(LANE_CHROME_FACET_AUTHORITIES),
);

const LANE_CHROME_EPOCH_RUNS = /\d+|\D+/g;

class ServeLaneStore {
  #targets = [];
  #targetById = new Map();
  #lanes = new Map();
  #listeners = [];
  #teamSnapshotRevision = 0;
  #groupTopologyByTargetId = new Map();
  #chromeByTargetId = new Map();
  #chromeOrderByTargetId = new Map();

  targetsSnapshot() {
    return this.#targets.slice();
  }

  targetForId(targetId) {
    return this.#targetById.get(String(targetId || ""));
  }

  replaceTargets(nextTargets) {
    if (!Array.isArray(nextTargets))
      throw new TypeError("lane store targets must be an array");
    this.#commitTargets(nextTargets.slice());
    return this.targetsSnapshot();
  }

  updateTarget(targetId, updater) {
    if (typeof updater !== "function")
      throw new TypeError("lane store target updater must be a function");
    const id = String(targetId || "");
    const current = this.#targetById.get(id);
    if (!current) return false;
    const updated = updater(current);
    if (!updated || typeof updated !== "object")
      throw new TypeError("lane store target updater must return a target");
    if (String(updated.id || "") !== id)
      throw new Error("lane store target updater cannot change target id");
    const nextTargets = this.#targets.slice();
    nextTargets[nextTargets.indexOf(current)] = updated;
    this.#commitTargets(nextTargets);
    return true;
  }

  teamSnapshotRevision() {
    return this.#teamSnapshotRevision;
  }

  teamCommandPayload(command, fields = {}) {
    return {
      command,
      expectedRevision: this.#teamSnapshotRevision,
      ...fields,
    };
  }

  applyTeamSnapshot(payload, options = {}) {
    const snapshot = (payload || {}).snapshot || {};
    const acceptance = this.#acceptTeamSnapshot(payload, snapshot, options);
    if (acceptance.disposition !== "applied")
      return this.#publishTeamSnapshotTransition(acceptance);
    return this.#publishTeamSnapshotTransition({
      ...acceptance,
      globalSettings: snapshot.globalSettings,
      ...this.#reconcileTeamSnapshot(
        snapshot,
        options,
        Boolean((payload || {}).differential),
      ),
    });
  }

  #acceptTeamSnapshot(payload, snapshot, options) {
    const revision = Math.max(
      0,
      Number((payload || {}).revision || snapshot.globalRevision || 0),
    );
    const previousRevision = this.#teamSnapshotRevision;
    const forced = Boolean(options.force);
    const differential = Boolean((payload || {}).differential);
    if (revision < previousRevision)
      return {
        disposition: "stale",
        differential,
        forced,
        incomingRevision: revision,
        previousRevision,
        revision: previousRevision,
      };

    this.#teamSnapshotRevision = revision;
    if ((payload || {}).changed === false && !forced)
      return {
        disposition: "unchanged",
        differential,
        forced,
        incomingRevision: revision,
        previousRevision,
        revision,
      };
    return {
      disposition: "applied",
      differential,
      forced,
      incomingRevision: revision,
      previousRevision,
      revision,
    };
  }

  #reconcileTeamSnapshot(snapshot, options, differential) {
    const teams = Array.isArray(snapshot.teams) ? snapshot.teams : [];
    const removedTeamIds = new Set(
      Array.isArray(snapshot.removedTeamIds) ? snapshot.removedTeamIds : [],
    );
    const affectedTeamIds = new Set([
      ...teams.map((team) => String((team || {}).teamId || "")),
      ...removedTeamIds,
    ]);
    const state = {
      desiredTargetIds: new Set(),
      adds: [],
      updates: [],
      renewals: [],
      groupRuns: [],
    };
    const adapters = this.#teamSnapshotAdapters(options);
    const teamCount = differential
      ? Math.max(0, Number(snapshot.teamCount) || 0)
      : teams.length;
    for (const team of teams)
      this.#reconcileTeam(team, state, adapters, teamCount > 1);
    const laneChanges = this.#teamSnapshotLaneChanges(
      state.desiredTargetIds,
      adapters.canRemoveLane,
      affectedTeamIds,
      differential,
    );
    return {
      adds: state.adds,
      updates: state.updates,
      renewals: state.renewals,
      groupRuns: this.#teamSnapshotGroupRuns(
        state.groupRuns,
        affectedTeamIds,
        differential,
      ),
      desiredTargetIds: Array.from(state.desiredTargetIds),
      ...laneChanges,
    };
  }

  #teamSnapshotAdapters(options) {
    return {
      resolveMember:
      typeof options.resolveMember === "function"
        ? options.resolveMember
        : () => ({}),
      unresolvedLaneTargetIds:
        typeof options.unresolvedLaneTargetIds === "function"
          ? options.unresolvedLaneTargetIds
          : () => [],
      emptyTeamTargetId:
        typeof options.emptyTeamTargetId === "function"
          ? options.emptyTeamTargetId
          : (teamId) => "empty-team:" + teamId,
      canRemoveLane:
        typeof options.canRemoveLane === "function"
          ? options.canRemoveLane
          : () => true,
    };
  }

  #reconcileTeam(team, state, adapters, emptyTeamCanClose) {
    const members = Array.isArray((team || {}).members) ? team.members : [];
    const memberTargetIds = [];
    for (const member of members) {
      const resolution =
        adapters.resolveMember(member, team, memberTargetIds.slice()) || {};
      this.#recordTeamMember(
        team,
        member,
        resolution,
        memberTargetIds,
        state,
      );
    }
    if (!members.length)
      this.#recordEmptyTeam(team, state, adapters, emptyTeamCanClose);
    if (memberTargetIds.length < members.length)
      this.#recordUnresolvedTeamLanes(team, state, adapters, memberTargetIds);
    if (memberTargetIds.length > 1)
      state.groupRuns.push(Object.freeze(memberTargetIds.slice()));
  }

  #recordTeamMember(team, member, resolution, memberTargetIds, state) {
    const targetId = String(resolution.targetId || "");
    if (
      !targetId ||
      !this.#targetById.has(targetId) ||
      memberTargetIds.includes(targetId)
    )
      return;
    memberTargetIds.push(targetId);
    state.desiredTargetIds.add(targetId);
    this.#recordTeamChange(
      Object.freeze({ kind: "member", targetId, team, member }),
      state,
    );
    const threadId = String(resolution.threadId || "");
    if (!threadId) return;
    state.renewals.push(
      Object.freeze({
        targetId,
        actorId: String(resolution.actorId || ""),
        threadId,
      }),
    );
  }

  #recordEmptyTeam(team, state, adapters, canClose) {
    const teamId = String((team || {}).teamId || "");
    if (!teamId) return;
    const targetId = String(adapters.emptyTeamTargetId(teamId) || "");
    if (!targetId) return;
    state.desiredTargetIds.add(targetId);
    this.#recordTeamChange(
      Object.freeze({ kind: "emptyTeam", targetId, team, canClose }),
      state,
    );
  }

  #recordTeamChange(change, state) {
    if (this.#lanes.has(change.targetId)) state.updates.push(change);
    else state.adds.push(change);
  }

  // Retention and membership are one decision. A snapshot that cannot resolve a
  // member's actor -- the window while a renewing agent's new thread is already
  // named by the team but not yet by the lane or its target -- keeps that lane
  // open; keeping it out of the run at the same time is the disagreement. The
  // fused host's merged stream is exactly its members' messages, so an ejected
  // lane takes every card it owns off the board, and a run that falls under two
  // dissolves the fusion outright, leaving the visible host showing only its own
  // messages until the next snapshot resolves and restores everything.
  #recordUnresolvedTeamLanes(team, state, adapters, memberTargetIds) {
    for (const value of adapters.unresolvedLaneTargetIds(team)) {
      const targetId = String(value || "");
      if (!targetId || !this.#lanes.has(targetId)) continue;
      state.desiredTargetIds.add(targetId);
      if (!memberTargetIds.includes(targetId))
        this.#retainGroupMember(targetId, memberTargetIds);
    }
  }

  // Its prior seat, not the tail: member order is composer order and drives
  // accent colour, so a lane that re-entered at the end would reshuffle and
  // recolour the composers on every transient refresh -- the same jitter moved
  // from the stream to the composer strip.
  #retainGroupMember(targetId, memberTargetIds) {
    const topology = this.#groupTopologyByTargetId.get(targetId);
    const priorIndex = topology ? topology.memberTargetIds.indexOf(targetId) : -1;
    if (priorIndex < 0) memberTargetIds.push(targetId);
    else memberTargetIds.splice(Math.min(priorIndex, memberTargetIds.length), 0, targetId);
  }

  #teamSnapshotLaneChanges(
    desiredTargetIds,
    canRemoveLane,
    affectedTeamIds,
    differential,
  ) {
    const removes = [];
    const retained = [];
    for (const lane of this.#lanes.values()) {
      if (desiredTargetIds.has(lane.targetId)) continue;
      if (differential && !affectedTeamIds.has(String(lane.teamId || "")))
        continue;
      if (canRemoveLane(lane)) removes.push(lane);
      else retained.push(lane);
    }
    return { removes, retained };
  }

  #teamSnapshotGroupRuns(changedRuns, affectedTeamIds, differential) {
    if (!differential) return changedRuns;
    const changedTargetIds = new Set(changedRuns.flat());
    const retainedRuns = [];
    for (const topology of this.#groupTopologyByTargetId.values()) {
      if (topology.role !== "host") continue;
      const affected = topology.memberTargetIds.some((targetId) => {
        if (changedTargetIds.has(targetId)) return true;
        const lane = this.#lanes.get(targetId);
        return affectedTeamIds.has(String((lane || {}).teamId || ""));
      });
      if (!affected) retainedRuns.push(topology.memberTargetIds.slice());
    }
    return [...retainedRuns, ...changedRuns];
  }

  lanesSnapshot() {
    return Array.from(this.#lanes.values());
  }

  laneForId(targetId) {
    return this.#lanes.get(String(targetId || ""));
  }

  hasLane(targetId) {
    return this.#lanes.has(String(targetId || ""));
  }

  registerLane(lane) {
    const targetId = String((lane || {}).targetId || "");
    if (!targetId) throw new Error("lane store lane target id is required");
    if (this.#lanes.has(targetId))
      throw new Error("lane store lane target id must be unique: " + targetId);
    this.#lanes.set(targetId, lane);
    this.#notifyLaneTransition("registered", lane);
    return lane;
  }

  removeLane(targetId) {
    const id = String(targetId || "");
    const lane = this.#lanes.get(id);
    if (!lane) return undefined;
    this.#lanes.delete(id);
    this.#notifyLaneTransition("removed", lane);
    return lane;
  }

  laneGroupTopology(targetId) {
    return this.#groupTopologyByTargetId.get(String(targetId || "")) || null;
  }

  applyLaneGroups(groupRuns, options = {}) {
    const isLaneOpen =
      typeof options.isLaneOpen === "function" ? options.isLaneOpen : () => true;
    const captureLaneState =
      typeof options.captureLaneState === "function"
        ? options.captureLaneState
        : () => null;
    const priorLaneStateByTargetId = new Map();
    for (const lane of this.#lanes.values())
      priorLaneStateByTargetId.set(lane.targetId, captureLaneState(lane));
    const previousHostByMemberTargetId = this.#laneGroupHostByMemberTargetId();
    const nextTopology = new Map();
    const runs = [];
    for (const run of Array.isArray(groupRuns) ? groupRuns : []) {
      const memberTargetIds = [];
      for (const value of run) {
        const targetId = String(value || "");
        const lane = this.#lanes.get(targetId);
        if (lane && isLaneOpen(lane) && !memberTargetIds.includes(targetId))
          memberTargetIds.push(targetId);
      }
      if (memberTargetIds.length < 2) continue;
      const hostTargetId = this.#stableLaneGroupHost(
        memberTargetIds,
        previousHostByMemberTargetId,
      );
      const members = Object.freeze(memberTargetIds.slice());
      nextTopology.set(
        hostTargetId,
        Object.freeze({ role: "host", hostTargetId, memberTargetIds: members }),
      );
      for (const targetId of memberTargetIds)
        if (targetId !== hostTargetId)
          nextTopology.set(
            targetId,
            Object.freeze({
              role: "member",
              hostTargetId,
              memberTargetIds: members,
            }),
          );
      runs.push(Object.freeze({ hostTargetId, memberTargetIds: members }));
    }
    this.#groupTopologyByTargetId = nextTopology;
    const transition = Object.freeze({
      runs: Object.freeze(runs),
      priorLaneStateByTargetId,
    });
    this.#notify(Object.freeze({ kind: "laneGroups", transition }));
    return transition;
  }

  #laneGroupHostByMemberTargetId() {
    const hosts = new Map();
    for (const topology of this.#groupTopologyByTargetId.values()) {
      if (topology.role !== "host" || topology.memberTargetIds.length < 2)
        continue;
      for (const targetId of topology.memberTargetIds)
        hosts.set(targetId, topology.hostTargetId);
    }
    return hosts;
  }

  #stableLaneGroupHost(memberTargetIds, previousHostByMemberTargetId) {
    for (const targetId of memberTargetIds) {
      const previousHostId = previousHostByMemberTargetId.get(targetId);
      if (previousHostId && memberTargetIds.includes(previousHostId))
        return previousHostId;
    }
    return memberTargetIds[0];
  }

  laneChrome(targetId) {
    return this.#chromeByTargetId.get(String(targetId || "")) || null;
  }

  // One canonical record per target, merged facet by facet. The record carries
  // values only; freshness lives beside it, so a facet that re-announces the
  // value it already published advances the high-water mark without replacing
  // the record every consumer renders from.
  applyLaneChrome(payload) {
    const targetId = String((payload || {}).targetId || "");
    if (!targetId) throw new Error("lane chrome target id is required");
    const record =
      this.#chromeByTargetId.get(targetId) || emptyLaneChromeRecord(targetId);
    // Freshness advances only once the whole payload has been read, so a facet
    // that breaks the contract cannot leave the target half-advanced against
    // values the record never took.
    const orderByFacet = new Map(this.#chromeOrderByTargetId.get(targetId));
    const changedFacets = [];
    const staleFacets = [];
    const values = {};
    for (const name of LANE_CHROME_FACETS) {
      const facet = laneChromeFacet(payload, name);
      if (!facet) continue;
      const order = laneChromeFacetOrder(facet);
      if (!isNewerLaneChromeOrder(order, orderByFacet.get(name))) {
        staleFacets.push(name);
        continue;
      }
      orderByFacet.set(name, order);
      const value = facet.value === undefined ? null : facet.value;
      if (sameLaneChromeValue(value, record[name])) continue;
      values[name] = value;
      changedFacets.push(name);
    }
    this.#chromeOrderByTargetId.set(targetId, orderByFacet);
    const nextRecord = changedFacets.length
      ? Object.freeze({ ...record, ...values })
      : record;
    this.#chromeByTargetId.set(targetId, nextRecord);
    return this.#publishLaneChromeTransition(
      nextRecord,
      changedFacets,
      staleFacets,
    );
  }

  #publishLaneChromeTransition(record, changedFacets, staleFacets) {
    const transition = Object.freeze({
      targetId: record.targetId,
      disposition: laneChromeDisposition(changedFacets, staleFacets),
      changedFacets: Object.freeze(changedFacets.slice()),
      staleFacets: Object.freeze(staleFacets.slice()),
      record,
    });
    this.#notify(Object.freeze({ kind: "laneChrome", transition }));
    return transition;
  }

  subscribe(listener) {
    if (typeof listener !== "function")
      throw new TypeError("lane store listener must be a function");
    const registration = { listener };
    this.#listeners.push(registration);
    let active = true;
    return () => {
      if (!active) return;
      active = false;
      this.#listeners = this.#listeners.filter(
        (candidate) => candidate !== registration,
      );
    };
  }

  #commitTargets(nextTargets) {
    const nextTargetById = new Map();
    for (const target of nextTargets) {
      const id = String((target || {}).id || "");
      if (!id) throw new Error("lane store target id is required");
      if (nextTargetById.has(id))
        throw new Error("lane store target id must be unique: " + id);
      nextTargetById.set(id, target);
    }
    this.#targets = nextTargets;
    this.#targetById = nextTargetById;
    const change = Object.freeze({
      kind: "targets",
      targets: Object.freeze(this.targetsSnapshot()),
    });
    this.#notify(change);
  }

  #notifyLaneTransition(transition, lane) {
    this.#notify(
      Object.freeze({
        kind: "lanes",
        transition,
        lane,
        lanes: Object.freeze(this.lanesSnapshot()),
      }),
    );
  }

  #publishTeamSnapshotTransition(fields) {
    const transition = Object.freeze({
      disposition: fields.disposition,
      differential: fields.differential,
      forced: fields.forced,
      incomingRevision: fields.incomingRevision,
      previousRevision: fields.previousRevision,
      revision: fields.revision,
      globalSettings: fields.globalSettings,
      adds: Object.freeze((fields.adds || []).slice()),
      updates: Object.freeze((fields.updates || []).slice()),
      removes: Object.freeze((fields.removes || []).slice()),
      retained: Object.freeze((fields.retained || []).slice()),
      renewals: Object.freeze((fields.renewals || []).slice()),
      groupRuns: Object.freeze((fields.groupRuns || []).slice()),
      desiredTargetIds: Object.freeze((fields.desiredTargetIds || []).slice()),
    });
    this.#notify(Object.freeze({ kind: "teamSnapshot", transition }));
    return transition;
  }

  #notify(change) {
    for (const registration of this.#listeners.slice())
      registration.listener(change);
  }
}

function emptyLaneChromeRecord(targetId) {
  const record = { targetId };
  for (const name of LANE_CHROME_FACETS) record[name] = null;
  return Object.freeze(record);
}

// An absent facet is one this payload's channel has nothing to say about; a
// present facet always names its authority and carries the order that dates it,
// so a payload that reaches the wrong facet -- a team-store renewal landing as
// inbox chrome -- or one that arrives undated is a contract break rather than
// data the reducer should guess at. Both are checked at this one door, which is
// what lets every reader past it take the order as given.
/**
 * @param {LaneChromePayload} payload
 * @param {string} name
 * @returns {(LaneChromeFacet|null)}
 */
function laneChromeFacet(payload, name) {
  const facet = payload[name];
  if (facet === undefined) return null;
  if (!facet || typeof facet !== "object")
    throw new TypeError("lane chrome facet must be an object: " + name);
  if (String(facet.authority || "") !== LANE_CHROME_FACET_AUTHORITIES[name])
    throw new Error("lane chrome facet authority mismatch: " + name);
  if (!facet.order || typeof facet.order !== "object")
    throw new TypeError("lane chrome facet must carry an order: " + name);
  return facet;
}

/**
 * @param {LaneChromeFacet} facet
 * @returns {LaneChromeFacetOrder}
 */
function laneChromeFacetOrder(facet) {
  const order = facet.order;
  return Object.freeze({
    epoch: String(order.epoch || ""),
    revision: Math.max(0, Number(order.revision) || 0),
  });
}

// Freshness is per facet and totally ordered by (epoch, revision). The epoch
// names the authority's counter generation and only ever advances, so an
// authority that restarted and resumed from a lower revision still supersedes,
// while inside one epoch a lower revision is always a redelivery.
/**
 * @param {LaneChromeFacetOrder} order
 * @param {(LaneChromeFacetOrder|undefined)} previous
 * @returns {boolean}
 */
function isNewerLaneChromeOrder(order, previous) {
  if (!previous) return true;
  if (order.epoch !== previous.epoch)
    return compareLaneChromeEpoch(order.epoch, previous.epoch) > 0;
  return order.revision > previous.revision;
}

// Natural order over the epoch: digit runs compare as numbers so generation 10
// supersedes generation 9, and the text around them compares as text so a
// prefixed label still groups. Zero-padded fields -- an ISO instant, say --
// order identically under both rules, so this only ever rescues the encodings
// plain collation would invert.
function compareLaneChromeEpoch(epoch, other) {
  const runs = String(epoch).match(LANE_CHROME_EPOCH_RUNS) || [];
  const otherRuns = String(other).match(LANE_CHROME_EPOCH_RUNS) || [];
  for (let index = 0; index < Math.min(runs.length, otherRuns.length); index++) {
    const run = runs[index];
    const otherRun = otherRuns[index];
    const numeric = /^\d/.test(run) && /^\d/.test(otherRun);
    if (numeric) {
      const digits = run.replace(/^0+(?=\d)/, "");
      const otherDigits = otherRun.replace(/^0+(?=\d)/, "");
      if (digits.length !== otherDigits.length)
        return digits.length < otherDigits.length ? -1 : 1;
      if (digits !== otherDigits) return digits < otherDigits ? -1 : 1;
    }
    if (!numeric && run !== otherRun) return run < otherRun ? -1 : 1;
  }
  if (runs.length !== otherRuns.length)
    return runs.length < otherRuns.length ? -1 : 1;
  return 0;
}

function sameLaneChromeValue(value, other) {
  if (value === other) return true;
  if (!value || !other || typeof value !== "object" || typeof other !== "object")
    return false;
  if (Array.isArray(value) !== Array.isArray(other)) return false;
  const keys = Object.keys(value);
  if (keys.length !== Object.keys(other).length) return false;
  return keys.every((key) => sameLaneChromeValue(value[key], other[key]));
}

function laneChromeDisposition(changedFacets, staleFacets) {
  if (changedFacets.length) return "applied";
  if (staleFacets.length) return "stale";
  return "unchanged";
}

const laneStore = new ServeLaneStore();
