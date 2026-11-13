// The Spice resident writes this oh-my-pi extension into its runtime
// directory and loads it with `--extension` on every quota sample.
//
// omp's built-in `openai-codex` usage provider keeps `/wham/usage`
// `credits.balance` only in its raw report. Each fetch delegates to that
// provider once and appends the canonical windowless balance limit when the
// delegated report has none. A balance the provider already normalizes is kept
// unchanged; a missing balance stays missing; a present balance that is not a
// finite decimal fails the report.
//
// omp serves its built-in provider without a word when it cannot load a named
// extension, so every report returned here carries `metadata.spiceExtension`,
// the proof the resident requires before it trusts a Codex report.
import { openaiCodexUsageProvider } from "@oh-my-pi/pi-ai/usage/openai-codex";

const PROVIDER = "openai-codex";
const STAMP = "openai-credit-balance";
const CACHE_VERSION_OFFSET = 1_000_000;
const DECIMAL = /^-?\d+(?:\.\d+)?$/;

// The finite balance one raw `/wham/usage` report carries, if any.
function balanceOf(raw) {
  const credits = raw?.credits;
  if (credits === undefined || credits === null) return undefined;
  if (typeof credits !== "object") throw new Error("OpenAI credits are not an object");
  const balance = credits.balance;
  if (balance === undefined || balance === null) return undefined;
  const value =
    typeof balance === "number"
      ? balance
      : typeof balance === "string" && DECIMAL.test(balance)
        ? Number(balance)
        : NaN;
  if (!Number.isFinite(value)) throw new Error("OpenAI credit balance is not a finite decimal");
  return value;
}

// The delegated report with its canonical balance limit and Spice's stamp.
function normalize(report) {
  const limits = report.limits ?? [];
  const normalized = limits.some((limit) => limit.scope?.windowId === "balance");
  const balance = normalized ? undefined : balanceOf(report.raw);
  const balanceLimit = {
    id: `${PROVIDER}:credits`,
    label: "Credit balance",
    scope: { provider: PROVIDER, windowId: "balance" },
    amount: { remaining: balance, unit: "credits" },
  };
  return {
    ...report,
    limits: balance === undefined ? limits : [...limits, balanceLimit],
    metadata: { ...report.metadata, spiceExtension: STAMP },
  };
}

export default function (pi) {
  pi.registerProvider(PROVIDER, {
    usage: {
      ...openaiCodexUsageProvider,
      // omp keys cached reports by provider and version. Keep this extension
      // in a disjoint range while retaining the delegated provider's version,
      // so an upstream cache retirement also retires our normalized reports.
      cacheVersion:
        CACHE_VERSION_OFFSET + (openaiCodexUsageProvider.cacheVersion ?? 0),
      async fetchUsage(params, ctx) {
        const report = await openaiCodexUsageProvider.fetchUsage(params, ctx);
        return report ? normalize(report) : report;
      },
    },
  });
}
