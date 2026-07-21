const { execFile } = require("child_process");
const fs = require("fs/promises");
const os = require("os");
const path = require("path");
const { promisify } = require("util");
const { repoRoot, withServePage } = require("./serve_playwright_harness");

const execFileAsync = promisify(execFile);
const fixtureThreadId = "11111111111111111111111111111111";
const hiddenStemCounts = {
  oops: 2,
  maxim_proposal: 3,
  sandbox: 4,
};
const hiddenStemProjects = {
  oops: ".oops.correctness",
  maxim_proposal: ".maxim_proposal.review",
  sandbox: ".sandbox.audit",
};

async function createFixtureRepository() {
  const repo = await fs.mkdtemp(
    path.join(os.tmpdir(), "spice-hidden-stem-browser-")
  );
  await execFileAsync("git", ["init", "-q", "-b", "main"], { cwd: repo });
  await fs.writeFile(
    path.join(repo, "pyproject.toml"),
    '[tool.spice.tasks]\nhidden_stems = ["sandbox"]\n',
    "utf8"
  );
  return repo;
}

async function seedHiddenStemTasks(repo, backendDir) {
  const script = [
    "import json",
    "import sys",
    "from spice.tasks import tw",
    "fixture = json.loads(sys.argv[1])",
    "for stem, row in fixture.items():",
    "    for index in range(1, int(row['count']) + 1):",
    "        tw.run([",
    "            'add',",
    "            f'Hidden {stem} fixture {index}',",
    "            f\"project:{row['project']}\",",
    "        ])",
  ].join("\n");
  await execFileAsync(
    path.join(repoRoot, ".venv", "bin", "python"),
    [
      "-c",
      script,
      JSON.stringify(
        Object.fromEntries(
          Object.entries(hiddenStemCounts).map(([stem, count]) => [
            stem,
            { count, project: hiddenStemProjects[stem] },
          ])
        )
      ),
    ],
    {
      cwd: repo,
      env: {
        ...process.env, // env-policy: allow
        CODEX_THREAD_ID: fixtureThreadId,
        SPICE_TASK_BACKEND: backendDir,
      },
      timeout: 10000,
    }
  );
}

async function waitForProductionInventory(page) {
  await page.waitForFunction(
    (expected) => {
      const inventory =
        laneStore.targetsSnapshot()[0]?.taskFilterInventory || null;
      if (!inventory) return false;
      const rows = new Map(
        (inventory.primaryStems || []).map((stem) => [stem.name, stem])
      );
      return Object.entries(expected).every(
        ([name, count]) => rows.get(name)?.openTaskCount === count
      );
    },
    hiddenStemCounts,
    { timeout: 10000 }
  );
}

async function readProductionState(page) {
  return page.evaluate((expectedNames) => {
    const inventory =
      laneStore.targetsSnapshot()[0]?.taskFilterInventory || {};
    const rows = new Map(
      (inventory.primaryStems || []).map((stem) => [stem.name, stem])
    );
    const pills = Array.from(document.querySelectorAll(".filter-pill")).map(
      (pill) => ({
        label: pill.querySelector(".filter-pill-label")?.textContent || "",
        count: pill.querySelector(".filter-pill-count")?.textContent || "",
        tone: ["ready", "active", "dormant"].find((value) =>
          pill.classList.contains("filter-pill--" + value)
        ),
        borderStyle: getComputedStyle(pill).borderTopStyle,
      })
    );
    return {
      catalogHiddenStems: inventory.catalog?.hiddenStems || [],
      rows: Object.fromEntries(
        expectedNames.map((name) => [name, rows.get(name) || null])
      ),
      pills: Object.fromEntries(
        expectedNames.map((name) => [
          name,
          pills.find((pill) => pill.label === name) || null,
        ])
      ),
    };
  }, Object.keys(hiddenStemCounts));
}

function assertProductionState(state) {
  const expectedNames = Object.keys(hiddenStemCounts);
  if (JSON.stringify(state.catalogHiddenStems) !== JSON.stringify(expectedNames))
    throw new Error(
      "configured hidden-stem catalog mismatch: " + JSON.stringify(state)
    );
  for (const [name, count] of Object.entries(hiddenStemCounts)) {
    const row = state.rows[name];
    if (
      !row ||
      row.openTaskCount !== count ||
      row.deferredTaskCount !== count ||
      row.readyTaskCount !== 0 ||
      row.inFlightTaskCount !== 0 ||
      row.blockedTaskCount !== 0
    )
      throw new Error(
        name + " production inventory row mismatch: " + JSON.stringify(row)
      );
    const pill = state.pills[name];
    if (
      !pill ||
      pill.count !== String(count) ||
      pill.tone !== "dormant" ||
      pill.borderStyle !== "dashed"
    )
      throw new Error(
        name + " production pill mismatch: " + JSON.stringify(pill)
      );
  }
  if (state.rows.oops.oopsTaskCount !== hiddenStemCounts.oops)
    throw new Error("oops row lost its dedicated count: " + JSON.stringify(state));
}

async function run() {
  const fixtureRepo = await createFixtureRepository();
  try {
    return await withServePage(
      {
        cwd: fixtureRepo,
        path: "/?smoke=serve-task-filter-hidden-stems-live-" + Date.now(),
        contextOptions: { viewport: { width: 1280, height: 720 } },
        prepareScratch: (scratch) =>
          seedHiddenStemTasks(fixtureRepo, scratch.backendDir),
      },
      async ({ page, server }) => {
        await waitForProductionInventory(page);
        const state = await readProductionState(page);
        assertProductionState(state);
        return { state, url: server.url };
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
