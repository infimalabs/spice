// Explicit state authority for serve lanes and their target inventory.

"use strict";

class ServeLaneStore {
  #targets = [];
  #targetById = new Map();
  #lanes = new Map();
  #listeners = [];

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

  #notify(change) {
    for (const registration of this.#listeners.slice())
      registration.listener(change);
  }
}

const laneStore = new ServeLaneStore();
