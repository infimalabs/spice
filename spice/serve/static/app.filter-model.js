// Pure lane task-filter derivation helpers.

function taskFilterEffectiveAssignedNames(inventory, assignedFilters) {
  const covered = new Set();
  const stems = new Map(
    ((inventory || {}).primaryStems || []).map((stem) => [stem.name, stem]),
  );
  for (const assignedFilter of assignedFilters || []) {
    if (!assignedFilter) continue;
    covered.add(assignedFilter);
    const stem = stems.get(assignedFilter);
    if (!stem) continue;
    for (const stemFilter of stem.filters || []) {
      if (stemFilter) covered.add(stemFilter);
    }
  }
  return covered;
}

function availableTaskFilterNames(inventory, assignedFilters) {
  if (!inventory) return [];
  const covered = taskFilterEffectiveAssignedNames(inventory, assignedFilters);
  return (inventory.filters || [])
    .map((filter) => filter.name)
    .filter((filter) => filter && !covered.has(filter))
    .sort();
}

function availableTaskFilterOpenTaskCount(inventory, assignedFilters) {
  if (!inventory) return 0;
  const covered = taskFilterEffectiveAssignedNames(inventory, assignedFilters);
  return (inventory.filters || []).reduce((total, filter) => {
    if (!filter.name || covered.has(filter.name)) return total;
    return total + Math.max(0, Number(filter.openTaskCount) || 0);
  }, 0);
}

function taskFilterOpenCount(inventory, filter) {
  if (!inventory) return 0;
  const stem = (inventory.primaryStems || []).find(
    (item) => item.name === filter,
  );
  if (stem) return Math.max(0, Number(stem.openTaskCount) || 0);
  const row = (inventory.filters || []).find((item) => item.name === filter);
  return row ? Math.max(0, Number(row.openTaskCount) || 0) : 0;
}

function taskCountBadgeCount(count) {
  return String(Math.max(0, Number(count) || 0));
}
