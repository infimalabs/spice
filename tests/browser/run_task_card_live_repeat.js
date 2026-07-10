const path = require("path");
const { spawn } = require("child_process");

const browserDir = __dirname;
const repoRoot = path.resolve(browserDir, "..", "..");
const childTimeoutMs = 180000;
const defaultRepeatCount = 3;

function repeatCount(value) {
  const configured = Number(value || defaultRepeatCount);
  if (!Number.isInteger(configured) || configured < 1)
    throw new Error("repeat count must be a positive integer");
  return configured;
}

function runChild(label, args) {
  const startedAt = Date.now();
  return new Promise((resolve) => {
    const child = spawn(process.execPath, args, {
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
    child.once("error", (error) => stderr.push(error.stack || error.message));
    const deadline = setTimeout(() => child.kill("SIGKILL"), childTimeoutMs);
    child.once("close", (code, signal) => {
      clearTimeout(deadline);
      resolve({
        code,
        durationMs: Date.now() - startedAt,
        label,
        signal,
        stderr: stderr.join(""),
        stdout: stdout.join(""),
      });
    });
  });
}

async function run(countValue = process.argv[2], manifestValue = process.argv[3]) {
  const loadManifest = path.resolve(
    manifestValue || path.join(browserDir, "task_card_live_load_manifest.js"),
  );
  const jobs = [
    runChild("browser-manifest-load", [
      path.join(browserDir, "run_release_smokes.js"),
      loadManifest,
    ]),
  ];
  for (let index = 0; index < repeatCount(countValue); index += 1) {
    jobs.push(
      runChild("task-card-repeat-" + (index + 1), [
        path.join(browserDir, "serve_task_card_live_smoke.js"),
      ]),
    );
  }
  const results = await Promise.all(jobs);
  const failures = results.filter(
    (result) => result.code !== 0 || Boolean(result.signal),
  );
  if (failures.length) {
    throw new Error(
      "task-card repeat runner failed:\n" +
        failures
          .map(
            (result) =>
              result.label +
              " code=" +
              result.code +
              " signal=" +
              result.signal +
              "\nstdout:\n" +
              result.stdout.trim() +
              "\nstderr:\n" +
              result.stderr.trim(),
          )
          .join("\n\n"),
    );
  }
  return results.map((result) => ({
    durationMs: result.durationMs,
    label: result.label,
  }));
}

if (require.main === module) {
  run()
    .then((results) => console.log(JSON.stringify({ results }, null, 2)))
    .catch((error) => {
      console.error(error.stack || error.message);
      process.exit(1);
    });
}

module.exports = { run };
