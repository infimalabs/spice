const fs = require("fs");
const path = require("path");
const { spawn } = require("child_process");

const browserDir = __dirname;
const repoRoot = path.resolve(browserDir, "..", "..");
const defaultManifestPath = path.join(browserDir, "release_smoke_manifest.js");
const maxParallel = 4;
const scenarioTimeoutMs = 120000;

function loadManifest(manifestPath) {
  const loaded = require(manifestPath);
  const releaseSafe = Array.isArray(loaded.releaseSafe) ? loaded.releaseSafe : [];
  const externalState = Array.isArray(loaded.externalState)
    ? loaded.externalState
    : [];
  if (releaseSafe.length === 0)
    throw new Error("release browser manifest has no release-safe scenarios");
  return { externalState, releaseSafe };
}

function assertDefaultManifestComplete(manifest) {
  const inventory = fs
    .readdirSync(browserDir)
    .filter((name) => name.endsWith("_smoke.js"))
    .sort();
  const classified = [
    ...manifest.releaseSafe.map((entry) => entry.path),
    ...manifest.externalState.map((entry) => entry.path),
  ].sort();
  if (JSON.stringify(classified) !== JSON.stringify(inventory))
    throw new Error(
      "release browser manifest must classify every smoke exactly once\n" +
        "inventory=" +
        JSON.stringify(inventory) +
        "\nclassified=" +
        JSON.stringify(classified),
    );
}

function assertRepoLocalPlaywright() {
  try {
    require.resolve("playwright", { paths: [repoRoot] });
  } catch (error) {
    throw new Error(
      "release browser gate requires repo-local Playwright; run npm ci in " +
        repoRoot +
        "\n" +
        error.message,
    );
  }
}

function runScenario(manifestDir, entry) {
  const script = path.resolve(manifestDir, entry.path);
  const startedAt = Date.now();
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [script], {
      cwd: repoRoot,
      env: { ...process.env }, // env-policy: allow
      stdio: ["ignore", "pipe", "pipe"],
    });
    const stdout = [];
    const stderr = [];
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => stdout.push(chunk));
    child.stderr.on("data", (chunk) => stderr.push(chunk));
    const timer = setTimeout(() => child.kill("SIGKILL"), scenarioTimeoutMs);
    child.once("close", (code, signal) => {
      clearTimeout(timer);
      resolve({
        code,
        durationMs: Date.now() - startedAt,
        path: entry.path,
        signal,
        stderr: stderr.join(""),
        stdout: stdout.join(""),
      });
    });
  });
}

async function runConcurrent(manifestDir, entries) {
  const results = new Array(entries.length);
  let nextIndex = 0;
  async function worker() {
    while (nextIndex < entries.length) {
      const index = nextIndex++;
      results[index] = await runScenario(manifestDir, entries[index]);
    }
  }
  await Promise.all(
    Array.from({ length: Math.min(maxParallel, entries.length) }, worker),
  );
  return results;
}

function reportResult(result) {
  const seconds = (result.durationMs / 1000).toFixed(1) + "s";
  if (result.code === 0 && !result.signal) {
    console.log("PASS " + result.path + " " + seconds);
    return true;
  }
  console.error(
    "FAIL " +
      result.path +
      " " +
      seconds +
      " code=" +
      result.code +
      " signal=" +
      result.signal,
  );
  if (result.stdout.trim()) console.error("stdout:\n" + result.stdout.trim());
  if (result.stderr.trim()) console.error("stderr:\n" + result.stderr.trim());
  return false;
}

async function run(manifestPath = defaultManifestPath) {
  const resolvedManifest = path.resolve(manifestPath);
  const manifest = loadManifest(resolvedManifest);
  if (resolvedManifest === defaultManifestPath) assertDefaultManifestComplete(manifest);
  assertRepoLocalPlaywright();
  const manifestDir = path.dirname(resolvedManifest);
  const concurrent = manifest.releaseSafe.filter((entry) => !entry.serial);
  const serial = manifest.releaseSafe.filter((entry) => entry.serial);
  const results = await runConcurrent(manifestDir, concurrent);
  for (const entry of serial) results.push(await runScenario(manifestDir, entry));
  for (const excluded of manifest.externalState)
    console.log("SKIP " + excluded.path + ": " + excluded.reason);
  const passed = results.map(reportResult);
  if (passed.every(Boolean)) return;
  throw new Error("release browser gate failed");
}

function checkDefaultManifest() {
  assertDefaultManifestComplete(loadManifest(defaultManifestPath));
  console.log("PASS release browser manifest completeness");
}

async function main() {
  if (process.argv[2] === "--check-manifest") {
    checkDefaultManifest();
    return;
  }
  await run(process.argv[2]);
}

if (require.main === module) {
  main()
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { checkDefaultManifest, run };
