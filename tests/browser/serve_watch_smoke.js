const fs = require("fs/promises");
const os = require("os");
const path = require("path");
const { repoRoot, withServePage } = require("./serve_playwright_harness");

const codexThread = "12345678-1234-1234-1234-123456789abc";
const claudeThread = "87654321-4321-4321-4321-cba987654321";
const methodNotAllowedStatus = 405;

async function createObservedFixtures() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "spice-watch-ui-"));
  const fixtures = path.join(repoRoot, "tests", "fixtures", "session");
  const codexTranscript = path.join(
    root,
    "rollout-2026-01-01T00-00-00-" + codexThread + ".jsonl",
  );
  const claudeTranscript = path.join(root, claudeThread + ".jsonl");
  await fs.copyFile(
    path.join(fixtures, "supervised_codex.jsonl"),
    codexTranscript,
  );
  await fs.copyFile(
    path.join(fixtures, "supervised_claude.jsonl"),
    claudeTranscript,
  );
  return {
    root,
    codexTranscript,
    claudeTranscript,
    claudeBefore: await fs.readFile(claudeTranscript),
  };
}

function watchOptions(root) {
  return {
    args: (scratch) => [
      "watch",
      root,
      "--host",
      "127.0.0.1",
      "--port",
      "0",
      "--until",
      scratch.stopFile,
    ],
  };
}

async function waitForObserverLanes(page, server, browserErrors) {
  try {
    await page
      .locator(".lane--observer")
      .first()
      .waitFor({ state: "visible", timeout: 10000 });
  } catch (error) {
    const diagnostics = await observerDiagnostics(page);
    throw new Error(
      error.message +
        "\npage=" +
        JSON.stringify(diagnostics) +
        "\nbrowser=" +
        JSON.stringify(browserErrors) +
        "\nstdout=" +
        server.stdout() +
        "\nstderr=" +
        server.stderr(),
    );
  }
  await page.waitForFunction(
    () => document.querySelectorAll(".lane--observer").length === 2,
  );
}

async function observerDiagnostics(page) {
  return page.evaluate(async () => {
    const teams = await liveBusRequest("teams.refresh", { query: {} });
    let applyError = "";
    try {
      applyTeamSnapshotPayload(teams.payload || {}, { force: true });
    } catch (applyFailure) {
      applyError = String(applyFailure?.stack || applyFailure);
    }
    return {
      globalStatus:
        typeof globalTransientStatusText === "undefined"
          ? "unavailable"
          : globalTransientStatusText,
      laneCount: document.querySelectorAll(".lane").length,
      targetCount:
        typeof targets === "undefined" ? "unavailable" : targets.length,
      teams,
      applyError,
    };
  });
}

async function assertSessionPicker(page) {
  await page.locator("#open-lane").click();
  const choices = page.locator(
    ".spice-context-menu [data-target-choice-id]",
  );
  await choices.first().waitFor({ state: "visible" });
  if ((await choices.count()) !== 2)
    throw new Error("observer session picker did not list both sessions");
  const menuText = await page.locator(".spice-context-menu").innerText();
  if (menuText.includes("Fast mode") || menuText.includes("new team"))
    throw new Error("observer session picker exposed mutation actions");
  await choices.nth(1).click();
}

async function assertReadOnlySurface(page, server) {
  const surface = await page.evaluate(() => ({
    lanes: document.querySelectorAll(".lane--observer").length,
    composers: document.querySelectorAll(".lane-composer").length,
    teamMenus: document.querySelectorAll("[data-lane-team-menu]").length,
    modeRails: document.querySelectorAll("[data-lane-mode-rail]").length,
    headerLabel: document.querySelector("#open-lane")?.getAttribute("aria-label"),
  }));
  if (
    surface.lanes !== 2 ||
    surface.composers !== 0 ||
    surface.teamMenus !== 0 ||
    surface.modeRails !== 0 ||
    surface.headerLabel !== "Observed sessions"
  )
    throw new Error(
      "observer surface exposed mutation controls: " + JSON.stringify(surface),
    );
  const response = await page.request.post(
    new URL("/api/work/trees/session-missing/send", server.url).toString(),
    { data: { text: "mutation attempt" } },
  );
  if (response.status() !== methodNotAllowedStatus)
    throw new Error("observer POST returned " + response.status());
}

async function appendObservedMessage(page, transcript) {
  await fs.appendFile(
    transcript,
    JSON.stringify({
      timestamp: "2026-01-01T00:00:30Z",
      type: "response_item",
      payload: {
        type: "message",
        role: "assistant",
        phase: "final_answer",
        content: [{ type: "output_text", text: "observer append visible" }],
      },
    }) + "\n",
    "utf8",
  );
  await page
    .getByText("observer append visible", { exact: true })
    .waitFor({ timeout: 15000 });
}

async function exerciseObservedSessions(context, options) {
  await withServePage(
    options,
    async ({ page, server, browserErrors }) => {
      await waitForObserverLanes(page, server, browserErrors);
      await page.getByText("slices rendered", { exact: true }).waitFor();
      await page
        .getByText("claude slices rendered", { exact: true })
        .waitFor();
      await assertSessionPicker(page);
      await assertReadOnlySurface(page, server);
      await appendObservedMessage(page, context.codexTranscript);
    },
  );
}

async function exerciseEmptyState(context, options) {
  await fs.rm(context.codexTranscript);
  await fs.rm(context.claudeTranscript);
  await fs.writeFile(path.join(context.root, "ignored.txt"), "foreign\n", "utf8");
  await withServePage(options, async ({ page }) => {
    const notice = page.locator(".observer-notice--empty");
    await notice.waitFor({ state: "visible" });
    if (
      (await notice.textContent()) !==
      "no recognizable Codex or Claude transcripts found"
    )
      throw new Error("empty observer notice did not report discovery state");
    if ((await page.locator(".lane").count()) !== 0)
      throw new Error("empty observer mounted a lane");
  });
}

async function main() {
  const context = await createObservedFixtures();
  const options = watchOptions(context.root);
  try {
    await exerciseObservedSessions(context, options);
    const claudeAfter = await fs.readFile(context.claudeTranscript);
    if (Buffer.compare(context.claudeBefore, claudeAfter) !== 0)
      throw new Error("watch modified an observed transcript");
    await exerciseEmptyState(context, options);
  } finally {
    await fs.rm(context.root, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
