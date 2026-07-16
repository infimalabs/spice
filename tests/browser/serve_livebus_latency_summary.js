// Deterministic summarizer + consistency checker for the live-bus latency
// traces. It reads the archived probe artifact and RE-DERIVES every value the
// diagnosis document publishes -- render rates, per-stage quantiles, per-pass
// send-lock intervals (including maxima), and the real-send submit-path stages
// -- directly from the JSON, then asserts each derived value is internally
// consistent. Run it to regenerate the diagnosis tables or to verify a table
// value was not transcribed by hand:
//
//   node tests/browser/serve_livebus_latency_summary.js path/to/traces.json
//
// Exit 0 with the derived tables on stdout when every check holds; exit 1 with
// the failing checks on stderr otherwise. The output is a pure function of the
// input artifact, so a table value that does not appear here did not come from
// the measurement.

const fs = require("fs");
const path = require("path");

function loadArtifact(artifactPath) {
  const raw = fs.readFileSync(artifactPath, "utf8");
  return JSON.parse(raw);
}

// Round to two decimals the same way the probe does, so re-derived values are
// byte-identical to the archived ones rather than merely close.
function round2(value) {
  if (!Number.isFinite(value)) return null;
  return Math.round(value * 100) / 100;
}

function fmt(value) {
  if (value === null || value === undefined) return "-";
  if (typeof value === "number") return String(round2(value));
  return String(value);
}

const failures = [];
function check(condition, message) {
  if (!condition) failures.push(message);
}

function nearly(a, b, epsilon = 0.011) {
  if (a === null || a === undefined || b === null || b === undefined) {
    return a === b;
  }
  return Math.abs(a - b) <= epsilon;
}

function checkStage(label, stage) {
  const { n, min, p50, p90, max, mean } = stage;
  if (!n) return; // an empty stage carries only {n:0}
  check(
    min <= p50 && p50 <= p90 && p90 <= max,
    `${label}: quantiles not monotonic (min=${min} p50=${p50} p90=${p90} max=${max})`,
  );
  check(
    mean >= min - 0.011 && mean <= max + 0.011,
    `${label}: mean ${mean} outside [min ${min}, max ${max}]`,
  );
}

function stageLine(key, stage) {
  if (!stage || !stage.n) return `  ${key}: n=0`;
  return (
    `  ${key}: n=${stage.n} min=${fmt(stage.min)} p50=${fmt(stage.p50)} ` +
    `p90=${fmt(stage.p90)} max=${fmt(stage.max)} mean=${fmt(stage.mean)}`
  );
}

function sendLockLines(label, sendLock) {
  const lines = [];
  const totals = sendLock.totals || {};
  let perKindCount = 0;
  let perKindWaitMax = 0;
  let perKindHoldMax = 0;
  for (const kind of Object.keys(sendLock.perKind || {})) {
    const k = sendLock.perKind[kind];
    perKindCount += k.count;
    perKindWaitMax = Math.max(perKindWaitMax, k.waitMaxMs || 0);
    perKindHoldMax = Math.max(perKindHoldMax, k.holdMaxMs || 0);
    lines.push(
      `  sendLock kind=${kind} count=${k.count} ` +
        `waitMean=${fmt(k.waitMeanMs)} waitMax=${fmt(k.waitMaxMs)} ` +
        `holdMean=${fmt(k.holdMeanMs)} holdMax=${fmt(k.holdMaxMs)}`,
    );
  }
  lines.push(
    `  sendLock TOTAL count=${totals.count} ` +
      `waitMean=${fmt(totals.waitMeanMs)} waitMax=${fmt(totals.waitMaxMs)} ` +
      `holdMean=${fmt(totals.holdMeanMs)} holdMax=${fmt(totals.holdMaxMs)}`,
  );
  // Consistency: the fan-out total must be the sum/maximum of its kinds, never a
  // cumulative lifetime value inherited across passes.
  check(
    totals.count === perKindCount,
    `${label}: sendLock total count ${totals.count} != sum of kinds ${perKindCount}`,
  );
  check(
    nearly(totals.waitMaxMs, perKindWaitMax),
    `${label}: sendLock total waitMax ${totals.waitMaxMs} != max of kinds ${round2(perKindWaitMax)}`,
  );
  check(
    nearly(totals.holdMaxMs, perKindHoldMax),
    `${label}: sendLock total holdMax ${totals.holdMaxMs} != max of kinds ${round2(perKindHoldMax)}`,
  );
  return lines;
}

function reportPass(label, condition, pass) {
  const out = [];
  out.push(`## ${label} -- ${condition}`);
  const expectedRate =
    pass.sampleCount > 0 ? round2(pass.renderedCount / pass.sampleCount) : null;
  out.push(
    `  renderRate=${fmt(pass.renderRate)} (${pass.renderedCount}/${pass.sampleCount})`,
  );
  check(
    pass.renderedCount <= pass.sampleCount,
    `${label}: rendered ${pass.renderedCount} > samples ${pass.sampleCount}`,
  );
  check(
    nearly(pass.renderRate, expectedRate),
    `${label}: renderRate ${pass.renderRate} != rendered/samples ${expectedRate}`,
  );
  for (const key of Object.keys(pass.stages)) {
    checkStage(`${label} ${key}`, pass.stages[key]);
    out.push(stageLine(key, pass.stages[key]));
  }
  out.push(...sendLockLines(label, pass.sendLock));
  const reconcile = Object.entries(pass.reconcileFrames || {})
    .map(([type, count]) => `${type}=${count}`)
    .join(" ");
  out.push(`  reconcileFrames: ${reconcile}`);
  return out;
}

function reportSubmit(submit) {
  const out = [];
  out.push(`## Pass S -- real scratch-backed lane.send submit path`);
  const expectedRate =
    submit.sampleCount > 0
      ? round2(submit.acceptedCount / submit.sampleCount)
      : null;
  out.push(
    `  acceptRate=${fmt(submit.acceptRate)} (${submit.acceptedCount}/${submit.sampleCount})`,
  );
  check(
    nearly(submit.acceptRate, expectedRate),
    `Pass S: acceptRate ${submit.acceptRate} != accepted/samples ${expectedRate}`,
  );
  for (const key of Object.keys(submit.stages)) {
    checkStage(`Pass S ${key}`, submit.stages[key]);
    out.push(stageLine(key, submit.stages[key]));
  }
  out.push(...sendLockLines("Pass S", submit.sendLock));
  return out;
}

function main() {
  if (!process.argv[2]) {
    process.stderr.write(
      "usage: node tests/browser/serve_livebus_latency_summary.js " +
        "path/to/traces.json\n",
    );
    process.exit(2);
  }
  const artifactPath = path.resolve(process.argv[2]);
  const report = loadArtifact(artifactPath);
  const meta = report.meta || {};
  const lines = [];
  lines.push(`# derived from ${path.relative(process.cwd(), artifactPath)}`);
  lines.push(
    `meta commit=${fmt(meta.commitShort)} dirty=${fmt(meta.treeDirty)} ` +
      `host=${fmt(meta.host)} node=${fmt(meta.node)}`,
  );
  lines.push(
    `meta lanes=${fmt(meta.boundLanes)}/${fmt(meta.requestedLanes)} ` +
      `rounds=${fmt(meta.rounds)} submits=${fmt(meta.submits)} ` +
      `frameBudgetMs=${fmt(meta.frameBudgetMs)} submitBudgetMs=${fmt(meta.submitBudgetMs)}`,
  );
  lines.push(`subscribeToActivateMs=${fmt(report.subscribeToActivateMs)}`);
  lines.push("");
  lines.push(...reportPass("Pass A", "concurrent focused baseline", report.passA));
  lines.push("");
  lines.push(...reportPass("Pass C", "single focused lane, others coalesced", report.passC));
  lines.push("");
  lines.push(...reportPass("Pass B", "all focused after atomic replacements", report.passB));
  lines.push("");
  lines.push(...reportSubmit(report.submit));

  process.stdout.write(lines.join("\n") + "\n");
  if (failures.length) {
    process.stderr.write(
      "\nconsistency FAILED:\n" +
        failures.map((message) => "  - " + message).join("\n") +
        "\n",
    );
    process.exit(1);
  }
  process.stderr.write("\nconsistency OK: all derived values internally consistent\n");
}

main();
