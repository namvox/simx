if (process.env.PLAYWRIGHT_NODE_MODULES) {
  require("module").Module._initPaths();
  module.paths.push(process.env.PLAYWRIGHT_NODE_MODULES);
}

const { spawn, spawnSync } = require("child_process");
const { chromium } = require("playwright");

const autoLease = process.env.SIMX_BENCH_AUTO_LEASE === "1";
const simxBin = process.env.SIMX_BIN || "target/debug/simx";
const leaseSlug = process.env.SIMX_BENCH_SLUG || "h264-pacing-bench";
const leasePort = Number(process.env.SIMX_BENCH_PORT || 8097);
const leaseFps = Number(process.env.SIMX_BENCH_FPS || 70);
const leaseTtl = process.env.SIMX_BENCH_TTL || "5m";
const leaseIdleTimeout = process.env.SIMX_BENCH_IDLE_TIMEOUT || "2m";
const leaseWaitTimeout = process.env.SIMX_BENCH_WAIT_TIMEOUT || "5s";
const leaseStartupTimeoutMs = Number(process.env.SIMX_BENCH_STARTUP_TIMEOUT_MS || 60_000);
const targetUrl =
  process.env.SIMX_VIEWER_URL ||
  (autoLease
    ? `http://127.0.0.1:${leasePort}/${leaseSlug}?transport=h264`
    : "http://127.0.0.1:8092/h264-browser-bench?transport=h264");
const durationMs = Number(process.env.SIMX_BENCH_DURATION_MS || 15_000);
const channel = process.env.PLAYWRIGHT_CHANNEL || "chrome";
const headless = process.env.PLAYWRIGHT_HEADLESS !== "0";
const thresholds = {
  renderedFps: 60,
  frameIntervalP95Ms: 21,
  frameIntervalP99Ms: 33,
  decodeRenderP95Ms: 8,
  serverSourceFps5s: 60,
  serverSentFps5s: 60,
  serverEncodeP95Ms: 12,
  serverDeliveryP95Ms: 120,
};
let leaseProcess = null;

async function main() {
  let browser = null;
  let leaseStarted = false;
  try {
    if (autoLease) {
      await startLease();
      leaseStarted = true;
    }
    browser = await chromium.launch({ channel, headless });
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
    const consoleIssues = [];

    page.on("console", (msg) => {
      const text = msg.text();
      if (["error", "warning"].includes(msg.type())) {
        if (
          text.includes("favicon") ||
          text.includes("Failed to load resource: the server responded with a status of 404")
        ) {
          return;
        }
        consoleIssues.push({ type: msg.type(), text });
      }
    });

    await page.goto(targetUrl, { waitUntil: "domcontentloaded" });
    await page.waitForFunction(
      () => document.getElementById("status")?.textContent !== "Connecting...",
      null,
      { timeout: 10_000 }
    );
    let renderedInitialFrame = true;
    try {
      await page.waitForFunction(
        () => {
          const metrics = JSON.parse(document.getElementById("metrics")?.textContent || "{}");
          return metrics.renderedFrames > 0;
        },
        null,
        { timeout: 15_000 }
      );
    } catch (_) {
      renderedInitialFrame = false;
    }

    const screen = await page.locator("#screen").boundingBox();
    if (!screen) throw new Error("viewer screen was not visible");

    await page.evaluate(() => window.simxMetrics?.reset?.());
    const startedAt = Date.now();
    while (Date.now() - startedAt < durationMs) {
      await drag(page, screen, "up");
      await page.waitForTimeout(120);
      await drag(page, screen, "down");
      await page.waitForTimeout(120);
    }

    await page.waitForTimeout(250);
    const metrics = await readMetrics(page);
    const stats = await readStats(page.url());
    const status = await page.locator("#status").textContent();
    const report = buildReport(page.url(), status, renderedInitialFrame, metrics, stats, consoleIssues);
    console.log(JSON.stringify(report, null, 2));
    if (!report.ok && process.env.SIMX_BENCH_STRICT === "1") {
      process.exitCode = 1;
    }
  } finally {
    if (browser !== null) {
      await browser.close();
    }
    if (autoLease && (leaseStarted || leaseProcess !== null)) {
      releaseLease();
    }
  }
}

async function startLease() {
  leaseProcess = spawn(
    simxBin,
    [
      "lease",
      "--slug",
      leaseSlug,
      "--ttl",
      leaseTtl,
      "--wait-timeout",
      leaseWaitTimeout,
      "--serve",
      "--port",
      String(leasePort),
      "--fps",
      String(leaseFps),
      "--transport",
      "h264",
      "--idle-timeout",
      leaseIdleTimeout,
      "--json",
    ],
    { encoding: "utf8" }
  );
  let stdout = "";
  let stderr = "";
  let exitCode = null;
  leaseProcess.stdout.on("data", (chunk) => {
    stdout += chunk.toString();
  });
  leaseProcess.stderr.on("data", (chunk) => {
    stderr += chunk.toString();
  });
  leaseProcess.on("exit", (code) => {
    exitCode = code;
  });

  const healthUrl = new URL(targetUrl);
  healthUrl.pathname = "/health";
  healthUrl.search = "";
  const startedAt = Date.now();
  while (Date.now() - startedAt < leaseStartupTimeoutMs) {
    if (exitCode !== null) {
      throw new Error(`simx benchmark lease failed (${exitCode}): ${stderr || stdout}`);
    }
    try {
      const response = await fetch(healthUrl);
      if (response.ok) return;
    } catch (_) {}
    await sleep(250);
  }
  throw new Error(`simx benchmark lease did not become ready: ${stderr || stdout}`);
}

function releaseLease() {
  const result = spawnSync(simxBin, ["release", "--slug", leaseSlug], { encoding: "utf8" });
  if (result.status !== 0) {
    console.error(
      `simx benchmark release failed (${result.status}): ${result.stderr || result.stdout}`
    );
  }
  if (leaseProcess !== null && leaseProcess.exitCode === null) {
    leaseProcess.kill("SIGTERM");
  }
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function drag(page, box, direction) {
  const x = box.x + box.width * 0.5;
  const startY = direction === "up" ? 0.78 : 0.22;
  const endY = direction === "up" ? 0.22 : 0.78;
  await page.mouse.move(x, box.y + box.height * startY);
  await page.mouse.down();
  await page.mouse.move(x, box.y + box.height * endY, { steps: 18 });
  await page.mouse.up();
}

async function readMetrics(page) {
  return page.evaluate(() => JSON.parse(document.getElementById("metrics")?.textContent || "{}"));
}

async function readStats(viewerUrl) {
  const url = new URL(viewerUrl);
  const parts = url.pathname.split("/").filter(Boolean);
  if (parts.length === 0) return null;
  url.pathname = `/${parts[0]}/stats`;
  url.search = "";
  try {
    const response = await fetch(url);
    if (!response.ok) return null;
    return response.json();
  } catch (_) {
    return null;
  }
}

function buildReport(url, status, renderedInitialFrame, metrics, stats, consoleIssues) {
  const dropRate = serverDropRate(stats);
  const targetMissRate = serverTargetMissRate(stats);
  const checks = {
    renderedInitialFrame,
    renderedFps: metrics.renderedFps >= thresholds.renderedFps,
    frameIntervalP95Ms:
      typeof metrics.frameIntervalP95Ms === "number" &&
      metrics.frameIntervalP95Ms <= thresholds.frameIntervalP95Ms,
    frameIntervalP99Ms:
      typeof metrics.frameIntervalP99Ms === "number" &&
      metrics.frameIntervalP99Ms <= thresholds.frameIntervalP99Ms,
    decodeRenderP95Ms:
      typeof metrics.decodeRenderP95Ms === "number" &&
      metrics.decodeRenderP95Ms <= thresholds.decodeRenderP95Ms,
    serverSourceFps5s:
      stats !== null &&
      typeof stats.source_fps_5s === "number" &&
      stats.source_fps_5s >= thresholds.serverSourceFps5s,
    serverSentFps5s:
      stats !== null &&
      typeof stats.sent_fps_5s === "number" &&
      stats.sent_fps_5s >= thresholds.serverSentFps5s,
    serverEncodeP95Ms:
      stats !== null &&
      typeof stats.encode_latency_ms_p95 === "number" &&
      stats.encode_latency_ms_p95 <= thresholds.serverEncodeP95Ms,
    serverDeliveryP95Ms:
      stats !== null &&
      typeof stats.delivery_latency_ms_p95 === "number" &&
      stats.delivery_latency_ms_p95 <= thresholds.serverDeliveryP95Ms,
    consoleHealth: consoleIssues.length === 0,
  };
  return {
    ok: Object.values(checks).every(Boolean),
    transport: "h264-websocket-webcodecs",
    url,
    status,
    durationMs,
    thresholds,
    checks,
    derived: {
      serverDropRate: dropRate,
      serverTargetMissRate: targetMissRate,
    },
    metrics,
    serverStats: stats,
    consoleIssues,
  };
}

function serverTargetMissRate(stats) {
  if (!stats) return null;
  const targetFps = Number(stats.target_fps);
  const sentFps = Number(stats.sent_fps_5s);
  if (!Number.isFinite(targetFps) || !Number.isFinite(sentFps) || targetFps <= 0) {
    return null;
  }
  return Number((Math.max(0, targetFps - sentFps) / targetFps).toFixed(4));
}

function serverDropRate(stats) {
  if (!stats) return null;
  const sourceFrames = Number(stats.source_frames);
  const droppedFrames = Number(stats.dropped_frames);
  if (!Number.isFinite(sourceFrames) || !Number.isFinite(droppedFrames) || sourceFrames <= 0) {
    return null;
  }
  return Number((droppedFrames / sourceFrames).toFixed(4));
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
