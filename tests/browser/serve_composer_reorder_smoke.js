// Gray-box browser check for within-lane composer reordering. It drives the
// real client functions (snapshotComposerReorder / composerReorderDropTarget /
// clearComposerMoveDropHighlights) against real shard DOM and the served CSS,
// asserting the swap contract the operator asked for:
//   - lifting a shard and hovering a neighbor swaps exactly those two slots;
//   - every other shard stays put (no insertion-slide within a lane);
//   - neighbors move via transform only, so no shard gains a horizontal
//     scrollbar (the old marker approach overflowed and oscillated scrollbars);
//   - releasing reorders to the swapped team order;
//   - teardown clears every transform.
const { withServePage } = require("./serve_playwright_harness");

async function run() {
  return withServePage(
    {
      path: "/?smoke=serve-composer-reorder-" + Date.now(),
      contextOptions: { viewport: { width: 1280, height: 720 } },
    },
    async ({ page, server }) => {
      await page.waitForFunction(
        () =>
          typeof snapshotComposerReorder === "function" &&
          typeof composerReorderDropTarget === "function" &&
          typeof clearComposerMoveDropHighlights === "function" &&
          typeof laneStates !== "undefined" &&
          typeof lanesEl !== "undefined",
        { timeout: 10000 },
      );

      const result = await page.evaluate(() => {
        const ids = ["alpha", "beta", "gamma"];
        // Build a real lane-group host: a .composer-shards container with one
        // .composer-shard per member, styled by the served composer.css.
        const shardsEl = document.createElement("div");
        shardsEl.className = "composer-shards";
        shardsEl.dataset.composerShards = "";
        shardsEl.style.width = "640px";
        shardsEl.style.height = "200px";
        for (const id of ids) {
          const shard = document.createElement("div");
          shard.className = "composer-shard";
          shard.dataset.shardTargetId = id;
          const tall = document.createElement("div");
          tall.style.height = "600px";
          tall.style.width = "100%";
          tall.textContent = id;
          shard.append(tall);
          shardsEl.append(shard);
        }
        const hostEl = document.createElement("div");
        hostEl.className = "lane";
        hostEl.append(shardsEl);
        lanesEl.append(hostEl);

        const topology = {
          role: "host",
          hostTargetId: "alpha",
          memberTargetIds: ids.slice(),
        };
        const hostLane = {
          targetId: "alpha",
          closed: false,
          element: hostEl,
          shardsEl,
          groupTopology: topology,
        };
        laneStates.set("alpha", hostLane);
        for (const id of ids.slice(1)) {
          laneStates.set(id, {
            targetId: id,
            closed: false,
            element: hostEl,
            groupTopology: { ...topology, role: "member" },
          });
        }

        function shardRect(id) {
          return shardsEl
            .querySelector('[data-shard-target-id="' + id + '"]')
            .getBoundingClientRect();
        }
        function shardEl(id) {
          return shardsEl.querySelector('[data-shard-target-id="' + id + '"]');
        }

        // Lift alpha, hover the center of gamma -> alpha and gamma swap slots.
        const state = { host: hostLane, targetId: "alpha", reorder: null };
        snapshotComposerReorder(state);
        const snapshotTaken = Boolean(state.reorder);
        const gamma = shardRect("gamma");
        const drop = composerReorderDropTarget(
          state,
          gamma.left + gamma.width / 2,
          gamma.top + gamma.height / 2,
        );

        const transforms = {};
        const horizontalScroll = {};
        for (const id of ids) {
          const el = shardEl(id);
          transforms[id] = el.style.transform || "";
          horizontalScroll[id] = el.scrollWidth - el.clientWidth;
        }
        const containerHorizontalScroll =
          shardsEl.scrollWidth - shardsEl.clientWidth;

        clearComposerMoveDropHighlights();
        const transformsAfterClear = ids.map((id) => shardEl(id).style.transform || "");

        const cleanup = () => {
          hostEl.remove();
          for (const id of ids) laneStates.delete(id);
        };
        cleanup();

        return {
          snapshotTaken,
          dropKind: drop ? drop.kind : null,
          orderedTargetIds: drop ? drop.orderedTargetIds : null,
          transforms,
          horizontalScroll,
          containerHorizontalScroll,
          transformsAfterClear,
        };
      });

      assertReorder(result);
      return { ...result, url: server.url };
    },
  );
}

function assertReorder(result) {
  if (!result.snapshotTaken)
    throw new Error("snapshotComposerReorder did not capture geometry");
  if (result.dropKind !== "reorder")
    throw new Error("expected reorder drop, got " + JSON.stringify(result.dropKind));
  const order = result.orderedTargetIds || [];
  // Logical order starts [alpha, beta, gamma]; swapping alpha<->gamma yields
  // [gamma, beta, alpha]. beta (the untouched middle) keeps its slot.
  const expected = ["gamma", "beta", "alpha"];
  if (order.length !== expected.length || order.some((id, i) => id !== expected[i]))
    throw new Error(
      "swap order mismatch: " + JSON.stringify(order) + " != " + JSON.stringify(expected),
    );
  if (!result.transforms.alpha)
    throw new Error("lifted shard alpha did not move");
  if (!result.transforms.gamma)
    throw new Error("dropped-on shard gamma did not move");
  if (result.transforms.beta)
    throw new Error("untouched shard beta moved: " + result.transforms.beta);
  for (const [id, overflow] of Object.entries(result.horizontalScroll)) {
    if (overflow > 1)
      throw new Error("shard " + id + " gained a horizontal scrollbar: " + overflow);
  }
  if (result.containerHorizontalScroll > 1)
    throw new Error(
      "shards container gained horizontal scroll: " + result.containerHorizontalScroll,
    );
  if (result.transformsAfterClear.some(Boolean))
    throw new Error(
      "transforms not cleared on teardown: " + JSON.stringify(result.transformsAfterClear),
    );
}

if (require.main === module) {
  run()
    .then((result) => {
      console.log(JSON.stringify(result, null, 2));
    })
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run };
