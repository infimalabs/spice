// A live serve, a real task backend, and one board outage.
//
// The pills read a chrome facet the browser orders by the task revision it
// carries, and it refuses an order it already holds. So a pass that could not
// read the board must publish no board at all: stamping an empty one with the
// live revision seats "no tasks" as the newest truth and leaves every recovery
// behind it refused. This drives that outage against a running serve and holds
// the pills to the count the operator can still see on screen.
const { execFile } = require("child_process");
const fs = require("fs/promises");
const os = require("os");
const path = require("path");
const { promisify } = require("util");
const { repoRoot, withServePage } = require("./serve_playwright_harness");

const execFileAsync = promisify(execFile);
const fixtureThreadId = "11111111111111111111111111111111";
const fixtureProject = "serve.outage";
const fixtureStem = "serve";
const seededTaskCount = 3;
const recoveredTaskCount = seededTaskCount + 1;
// Taskwarrior's own database, the one file whose loss is a board outage and
// nothing else. The backend directory also holds the serve team store, so
// revoking the directory would take teams down with it and prove nothing.
const taskDatabaseRelativePath = path.join("data", "taskchampion.sqlite3");
const unreadableMode = 0o000;
const readableMode = 0o644;
// The pill renders its state counts as one interpunct-joined run, open first.
const pillCountSeparator = "·";
const boardSettleTimeoutMs = 15000;
const pythonTimeoutMs = 20000;

const seedScript = [
  "import sys",
  "from spice.tasks import tw",
  "start, count, project = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]",
  "for index in range(start, start + count):",
  "    tw.run(['add', f'Board outage fixture {index}', f'project:{project}'])",
].join("\n");

const bumpTaskRevisionScript = [
  "from spice.tasks import config",
  "config.mark_task_backend_changed('task')",
].join("\n");

async function createFixtureRepository() {
  const repo = await fs.mkdtemp(path.join(os.tmpdir(), "spice-board-outage-"));
  await execFileAsync("git", ["init", "-q", "-b", "main"], { cwd: repo });
  await fs.writeFile(path.join(repo, "pyproject.toml"), "[tool.spice]\n", "utf8");
  return repo;
}

function runPython(repo, backendDir, script, args = []) {
  return execFileAsync(
    path.join(repoRoot, ".venv", "bin", "python"),
    ["-c", script, ...args],
    {
      cwd: repo,
      env: {
        ...process.env, // env-policy: allow
        CODEX_THREAD_ID: fixtureThreadId,
        SPICE_TASK_BACKEND: backendDir,
      },
      timeout: pythonTimeoutMs,
    }
  );
}

function seedTasks(repo, backendDir, start, count) {
  return runPython(repo, backendDir, seedScript, [
    String(start),
    String(count),
    fixtureProject,
  ]);
}

// Revoking access is the outage a busy or half-migrated backend produces on its
// own: the export fails, the observation carries the error, and the task
// revision is whatever the last writer left.
function setBoardReadable(backendDir, readable) {
  return fs.chmod(
    path.join(backendDir, taskDatabaseRelativePath),
    readable ? readableMode : unreadableMode
  );
}

function bumpTaskRevision(repo, backendDir) {
  return runPython(repo, backendDir, bumpTaskRevisionScript);
}

// The pills track the board through a lane's live-bus subscription, so an
// operator watching a closed view sees only what the page loaded with. Opening
// one is what puts this check on the path the reported staleness runs through.
async function openSubscribedLane(page) {
  const targetId = await page.evaluate(() => {
    const target = laneStore.targetsSnapshot()[0];
    if (!target) throw new Error("serve published no worktree target");
    if (!laneStore.laneForId(target.id)) addLane(target.id);
    return target.id;
  });
  await page.waitForFunction(
    (id) => Boolean(laneStore.laneForId(id)?.liveBusSubscribed),
    targetId,
    { timeout: boardSettleTimeoutMs }
  );
  return targetId;
}

async function readBoardState(page) {
  const state = await page.evaluate((stem) => {
    const target = laneStore.targetsSnapshot()[0];
    const inventory = target
      ? laneChromeTaskBoard(target.id).taskFilterInventory || null
      : null;
    const pill = Array.from(document.querySelectorAll(".filter-pill")).find(
      (node) => node.querySelector(".filter-pill-label")?.textContent === stem
    );
    return {
      openTaskCount: inventory ? inventory.openTaskCount : null,
      revision: inventory ? inventory.revision : null,
      pillCount: pill
        ? pill.querySelector(".filter-pill-count")?.textContent || ""
        : "",
    };
  }, fixtureStem);
  return { ...state, pillOpenCount: state.pillCount.split(pillCountSeparator)[0] };
}

function waitForOpenTaskCount(page, expected) {
  return page.waitForFunction(
    (count) => {
      const target = laneStore.targetsSnapshot()[0];
      const inventory = target
        ? laneChromeTaskBoard(target.id).taskFilterInventory || null
        : null;
      return Boolean(inventory) && inventory.openTaskCount === count;
    },
    expected,
    { timeout: boardSettleTimeoutMs }
  );
}

// Reading the wire directly is what keeps the on-screen assertion honest: it
// proves the outage really was in force for a served pass, so pills that held
// their count held it against a live degraded publication rather than because
// nothing was ever published.
async function readServedLaneFacets(serverUrl) {
  const response = await fetch(new URL("/api/work/trees", serverUrl), {
    headers: { accept: "application/json" },
  });
  if (!response.ok)
    throw new Error("work trees request failed: HTTP " + response.status);
  const payload = await response.json();
  const tree = (payload.workTrees || [])[0] || {};
  return Object.keys(tree.chrome || {}).sort();
}

function assertHeldThroughOutage(before, during, servedFacets) {
  if (before.openTaskCount !== seededTaskCount)
    throw new Error("seeded board never reached the pills: " + JSON.stringify(before));
  if (before.pillOpenCount !== String(seededTaskCount))
    throw new Error("seeded pill count mismatch: " + JSON.stringify(before));
  if (servedFacets.includes("taskBoard"))
    throw new Error(
      "a pass that could not read the board still published it: " +
        JSON.stringify(servedFacets)
    );
  if (servedFacets.length === 0)
    throw new Error(
      "the degraded pass published no facets at all, so it proves nothing " +
        "about withholding one: " + JSON.stringify(servedFacets)
    );
  if (during.openTaskCount !== seededTaskCount || during.pillCount !== before.pillCount)
    throw new Error(
      "the outage emptied the pills instead of leaving them alone: " +
        JSON.stringify({ before, during })
    );
}

async function run() {
  const fixtureRepo = await createFixtureRepository();
  try {
    return await withServePage(
      {
        cwd: fixtureRepo,
        path: "/?smoke=serve-task-board-outage-pills-live-" + Date.now(),
        contextOptions: { viewport: { width: 1280, height: 720 } },
        prepareScratch: (scratch) =>
          seedTasks(fixtureRepo, scratch.backendDir, 1, seededTaskCount),
      },
      async ({ page, server }) => {
        await openSubscribedLane(page);
        await waitForOpenTaskCount(page, seededTaskCount);
        const before = await readBoardState(page);

        await setBoardReadable(server.backendDir, false);
        try {
          // The revision moves while the board cannot be read, which is the only
          // way a pass is asked to publish a board it never saw.
          await bumpTaskRevision(fixtureRepo, server.backendDir);
          const servedFacets = await readServedLaneFacets(server.url);
          const during = await readBoardState(page);
          assertHeldThroughOutage(before, during, servedFacets);
        } finally {
          await setBoardReadable(server.backendDir, true);
        }

        await seedTasks(fixtureRepo, server.backendDir, recoveredTaskCount, 1);
        await waitForOpenTaskCount(page, recoveredTaskCount);
        const after = await readBoardState(page);
        if (after.pillOpenCount !== String(recoveredTaskCount))
          throw new Error(
            "the pills stopped tracking the board after the outage: " +
              JSON.stringify(after)
          );
        return { before, after, url: server.url };
      }
    );
  } finally {
    await fs.rm(fixtureRepo, { recursive: true, force: true });
  }
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
