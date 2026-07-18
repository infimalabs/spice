// Explicit state authority for serve lanes and their target inventory.

"use strict";

class ServeLaneStore {
  #targets = [];
  #targetById = new Map();
  #lanes = new Map();
  #listeners = [];
  #teamSnapshotRevision = 0;

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
      ...this.#reconcileTeamSnapshot(snapshot, options),
    });
  }

  #acceptTeamSnapshot(payload, snapshot, options) {
    const revision = Math.max(
      0,
      Number((payload || {}).revision || snapshot.globalRevision || 0),
    );
    const previousRevision = this.#teamSnapshotRevision;
    const forced = Boolean(options.force);
    if (revision < previousRevision)
      return {
        disposition: "stale",
        forced,
        incomingRevision: revision,
        previousRevision,
        revision: previousRevision,
      };

    this.#teamSnapshotRevision = revision;
    if ((payload || {}).changed === false && !forced)
      return {
        disposition: "unchanged",
        forced,
        incomingRevision: revision,
        previousRevision,
        revision,
      };
    return {
      disposition: "applied",
      forced,
      incomingRevision: revision,
      previousRevision,
      revision,
    };
  }

  #reconcileTeamSnapshot(snapshot, options) {
    const teams = Array.isArray(snapshot.teams) ? snapshot.teams : [];
    const state = {
      desiredTargetIds: new Set(),
      adds: [],
      updates: [],
      renewals: [],
      groupRuns: [],
    };
    const adapters = this.#teamSnapshotAdapters(options);
    for (const team of teams)
      this.#reconcileTeam(team, state, adapters, teams.length > 1);
    const laneChanges = this.#teamSnapshotLaneChanges(
      state.desiredTargetIds,
      adapters.canRemoveLane,
    );
    return {
      adds: state.adds,
      updates: state.updates,
      renewals: state.renewals,
      groupRuns: state.groupRuns,
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
      this.#recordUnresolvedTeamLanes(team, state, adapters);
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

  #recordUnresolvedTeamLanes(team, state, adapters) {
    for (const value of adapters.unresolvedLaneTargetIds(team)) {
      const targetId = String(value || "");
      if (targetId && this.#lanes.has(targetId))
        state.desiredTargetIds.add(targetId);
    }
  }

  #teamSnapshotLaneChanges(desiredTargetIds, canRemoveLane) {
    const removes = [];
    const retained = [];
    for (const lane of this.#lanes.values()) {
      if (desiredTargetIds.has(lane.targetId)) continue;
      if (canRemoveLane(lane)) removes.push(lane);
      else retained.push(lane);
    }
    return { removes, retained };
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

const laneStore = new ServeLaneStore();
