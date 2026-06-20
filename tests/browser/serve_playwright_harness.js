const fs = require("fs/promises");
const os = require("os");
const path = require("path");
const { spawn } = require("child_process");
const { chromium } = require("playwright");

const repoRoot = path.resolve(__dirname, "..", "..");
const defaultPlaywrightConfigPath = path.join(
  repoRoot,
  ".spice",
  "agent",
  "playwright-mcp.json",
);
const defaultStartTimeoutMs = 15000;
const defaultStopTimeoutMs = 5000;

function processOutput(stdout, stderr) {
  return [stdout.join(""), stderr.join("")]
    .map((text) => text.trim())
    .filter(Boolean)
    .join("\n");
}

function waitForProcessExit(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null)
    return Promise.resolve({ code: child.exitCode, signal: child.signalCode });
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      child.off("exit", onExit);
      reject(new Error("spice serve did not stop within " + timeoutMs + "ms"));
    }, timeoutMs);
    function onExit(code, signal) {
      clearTimeout(timer);
      resolve({ code, signal });
    }
    child.once("exit", onExit);
  });
}

async function createServeScratch() {
  const tmpRoot = await fs.mkdtemp(path.join(os.tmpdir(), "spice-serve-ui-"));
  const backendDir = path.join(tmpRoot, "task-backend");
  const stopFile = path.join(tmpRoot, "serve.stop");
  await fs.mkdir(backendDir, { recursive: true });
  await fs.writeFile(stopFile, "running\n", "utf8");
  return { backendDir, stopFile, tmpRoot };
}

function spawnServeProcess(options, scratch) {
  const stdout = [];
  const stderr = [];
  const command = options.serveCommand || process.env.SPICE_SERVE_BIN || "spice";
  const args = [
    "serve",
    "--host",
    options.host || "127.0.0.1",
    "--port",
    String(options.port ?? 0),
    "--until",
    scratch.stopFile,
    "--task-backend",
    scratch.backendDir,
  ];
  const child = spawn(command, args, {
    cwd: options.cwd || repoRoot,
    env: { ...process.env, PYTHONUNBUFFERED: "1", ...(options.env || {}) },
    stdio: ["ignore", "pipe", "pipe"],
  });

  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => stdout.push(chunk));
  child.stderr.on("data", (chunk) => stderr.push(chunk));
  return { child, stderr, stdout };
}

function serveStopper(child, stdout, stderr, scratch, options) {
  let stopped = false;
  return async () => {
    if (stopped) return;
    stopped = true;
    let exitResult = {
      code: child.exitCode,
      signal: child.signalCode,
    };
    if (child.exitCode === null && child.signalCode === null) {
      await fs.writeFile(scratch.stopFile, "stop " + Date.now() + "\n", "utf8");
      try {
        exitResult = await waitForProcessExit(
          child,
          options.stopTimeoutMs || defaultStopTimeoutMs,
        );
      } catch (error) {
        child.kill("SIGKILL");
        await waitForProcessExit(child, defaultStopTimeoutMs).catch(() => {});
        throw error;
      }
    }
    await fs.rm(scratch.tmpRoot, { recursive: true, force: true });
    if (exitResult.code !== 0 || exitResult.signal)
      throw new Error(
        "spice serve stopped uncleanly: code=" +
          exitResult.code +
          " signal=" +
          exitResult.signal +
          "\n" +
          processOutput(stdout, stderr),
      );
  };
}

function waitForServeUrl(child, stdout, stderr, options = {}) {
  let ready = false;
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      reject(
        new Error(
          "spice serve did not print a URL within " +
            (options.startTimeoutMs || defaultStartTimeoutMs) +
            "ms\n" +
            processOutput(stdout, stderr),
        ),
      );
    }, options.startTimeoutMs || defaultStartTimeoutMs);

    function resolveFromOutput() {
      const match = stdout.join("").match(/spice serve: (http:\/\/[^\s]+)/);
      if (!match) return;
      ready = true;
      clearTimeout(timeout);
      resolve(match[1]);
    }

    child.stdout.on("data", resolveFromOutput);
    resolveFromOutput();
    child.once("error", (error) => {
      clearTimeout(timeout);
      reject(error);
    });
    child.once("exit", (code, signal) => {
      if (ready) return;
      clearTimeout(timeout);
      reject(
        new Error(
          "spice serve exited before it was ready: code=" +
            code +
            " signal=" +
            signal +
            "\n" +
            processOutput(stdout, stderr),
        ),
      );
    });
  });
}

async function startServe(options = {}) {
  const scratch = await createServeScratch();
  const { child, stderr, stdout } = spawnServeProcess(options, scratch);
  const stop = serveStopper(child, stdout, stderr, scratch, options);
  try {
    const url = await waitForServeUrl(child, stdout, stderr, options);
    return {
      url,
      backendDir: scratch.backendDir,
      stopFile: scratch.stopFile,
      stdout: () => stdout.join(""),
      stderr: () => stderr.join(""),
      process: child,
      stop,
    };
  } catch (error) {
    await stop().catch(() => {});
    throw error;
  }
}

function collectBrowserErrors(page, options = {}) {
  const consoleTypes = new Set(options.consoleTypes || ["error"]);
  const errors = [];
  page.on("console", (message) => {
    if (!consoleTypes.has(message.type())) return;
    errors.push({
      source: "console",
      type: message.type(),
      text: message.text(),
    });
  });
  page.on("pageerror", (error) => {
    errors.push({
      source: "pageerror",
      type: error.name || "Error",
      text: error.message,
    });
  });
  return errors;
}

function assertNoBrowserErrors(errors) {
  if (errors.length === 0) return;
  throw new Error(
    "browser errors:\n" +
      errors
        .map((error) => error.source + ":" + error.type + ": " + error.text)
        .join("\n"),
  );
}

async function readSharedPlaywrightContextOptions(options = {}) {
  const configPath = options.playwrightConfigPath || defaultPlaywrightConfigPath;
  let raw;
  try {
    raw = await fs.readFile(configPath, "utf8");
  } catch (error) {
    if (error && error.code === "ENOENT")
      throw new Error(
        "missing shared Playwright config at " +
          configPath +
          "; start the agent through spice so browser validation matches the " +
          "operator system appearance",
      );
    throw error;
  }
  let config;
  try {
    config = JSON.parse(raw);
  } catch (error) {
    throw new Error(
      "invalid Playwright config JSON at " + configPath + ": " + error.message,
    );
  }
  if (
    !config ||
    !config.browser ||
    typeof config.browser !== "object" ||
    !config.browser.contextOptions ||
    typeof config.browser.contextOptions !== "object" ||
    Array.isArray(config.browser.contextOptions)
  )
    throw new Error(
      "shared Playwright config at " +
        configPath +
        " must define browser.contextOptions",
    );
  const contextOptions = config.browser.contextOptions;
  return { ...contextOptions };
}

function rejectColorSchemeOverride(contextOptions, configPath) {
  if (
    contextOptions &&
    Object.prototype.hasOwnProperty.call(contextOptions, "colorScheme")
  )
    throw new Error(
      "serve Playwright checks inherit colorScheme from " +
        configPath +
        "; update the shared agent Playwright config instead of setting a " +
        "per-smoke colorScheme override",
    );
}

async function serveBrowserContextOptions(options = {}) {
  const callerContextOptions = options.contextOptions || {};
  const configPath = options.playwrightConfigPath || defaultPlaywrightConfigPath;
  rejectColorSchemeOverride(callerContextOptions, configPath);
  return {
    ...(await readSharedPlaywrightContextOptions({
      ...options,
      playwrightConfigPath: configPath,
    })),
    ...callerContextOptions,
  };
}

async function withServePage(options, callback) {
  const server = await startServe(options);
  let browser = null;
  try {
    browser = await chromium.launch(options.launchOptions || {});
    const context = await browser.newContext(
      await serveBrowserContextOptions(options),
    );
    const page = await context.newPage();
    const browserErrors = collectBrowserErrors(page, options);
    const targetPath = options.path || "/";
    const url = new URL(targetPath, server.url).toString();
    await page.goto(url, {
      waitUntil: options.waitUntil || "domcontentloaded",
      timeout: options.navigationTimeoutMs || defaultStartTimeoutMs,
    });
    const result = await callback({
      browser,
      context,
      page,
      server,
      browserErrors,
      assertNoBrowserErrors: () => assertNoBrowserErrors(browserErrors),
    });
    assertNoBrowserErrors(browserErrors);
    return result;
  } finally {
    if (browser) await browser.close().catch(() => {});
    await server.stop();
  }
}

module.exports = {
  assertNoBrowserErrors,
  collectBrowserErrors,
  defaultPlaywrightConfigPath,
  readSharedPlaywrightContextOptions,
  repoRoot,
  serveBrowserContextOptions,
  startServe,
  withServePage,
};
