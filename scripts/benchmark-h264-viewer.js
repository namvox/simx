if (process.env.PLAYWRIGHT_NODE_MODULES) {
  require("module").Module._initPaths();
  module.paths.push(process.env.PLAYWRIGHT_NODE_MODULES);
}

const http = require("http");
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
const controlMode = process.env.SIMX_BENCH_CONTROL_MODE || "single-controller";
const sceneHost = process.env.SIMX_BENCH_SCENE_HOST || "127.0.0.1";
const scenePort = Number(process.env.SIMX_BENCH_SCENE_PORT || 8897);
const scenarioDurationMs = Number(process.env.SIMX_BENCH_DURATION_MS || 15_000);
const sceneSettleMs = Number(process.env.SIMX_BENCH_SCENE_SETTLE_MS || 1_500);
const channel = process.env.PLAYWRIGHT_CHANNEL || "chrome";
const headless = process.env.PLAYWRIGHT_HEADLESS !== "0";
const targetUrl =
  process.env.SIMX_VIEWER_URL ||
  (autoLease
    ? `http://127.0.0.1:${leasePort}/${leaseSlug}?transport=h264`
    : "http://127.0.0.1:8092/h264-browser-bench?transport=h264");
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

const sceneBaseUrl =
  process.env.SIMX_BENCH_SCENE_BASE_URL || `http://${sceneHost}:${scenePort}`;

let leaseProcess = null;
let sceneServer = null;

const scenarioDefinitions = {
  "static-taps": {
    title: "Static taps",
    scene: "static-taps",
    description: "Mostly static app-like surface with repeated taps.",
    async drive(page, screen, deadline) {
      const points = [
        [0.24, 0.26],
        [0.72, 0.26],
        [0.50, 0.48],
        [0.28, 0.72],
        [0.76, 0.72],
      ];
      let index = 0;
      while (Date.now() < deadline) {
        const [nx, ny] = points[index % points.length];
        await tap(page, screen, nx, ny);
        index += 1;
        await page.waitForTimeout(180);
      }
    },
  },
  "smooth-scroll": {
    title: "Smooth scrolling",
    scene: "smooth-scroll",
    description: "Long list with continuous touch drags.",
    async drive(page, screen, deadline) {
      while (Date.now() < deadline) {
        await drag(page, screen, "up", 22);
        await page.waitForTimeout(100);
        await drag(page, screen, "down", 22);
        await page.waitForTimeout(100);
      }
    },
  },
  "keyboard-entry": {
    title: "Keyboard text entry",
    scene: "keyboard-entry",
    description: "Focused text field receiving repeated hardware-key events.",
    async drive(page, screen, deadline) {
      await tap(page, screen, 0.48, 0.27);
      await page.waitForTimeout(400);
      const phrases = [
        "simx h264 keyboard benchmark ",
        "latency frames decode render ",
        "agent typing stress scene ",
      ];
      let index = 0;
      while (Date.now() < deadline) {
        await page.keyboard.type(phrases[index % phrases.length], { delay: 18 });
        index += 1;
        await page.waitForTimeout(120);
      }
    },
  },
  "animation-heavy": {
    title: "Animation heavy",
    scene: "animation-heavy",
    description: "Dense CSS transforms, opacity changes, and moving UI elements.",
    async drive(page, screen, deadline) {
      while (Date.now() < deadline) {
        await tap(page, screen, 0.50, 0.84);
        await page.waitForTimeout(450);
      }
    },
  },
  "full-motion": {
    title: "Full-screen gradient motion",
    scene: "full-motion",
    description: "Full-screen animated color gradients and overlays.",
    async drive(page, _screen, deadline) {
      while (Date.now() < deadline) {
        await page.waitForTimeout(250);
      }
    },
  },
  "text-heavy": {
    title: "Text-heavy UI",
    scene: "text-heavy",
    description: "Dense small text, tables, and subtle scrolling for artifact checks.",
    async drive(page, screen, deadline) {
      while (Date.now() < deadline) {
        await drag(page, screen, "up", 14);
        await page.waitForTimeout(180);
        await drag(page, screen, "down", 14);
        await page.waitForTimeout(180);
      }
    },
  },
};

const scenarioAliases = {
  "smooth-scrolling": "smooth-scroll",
  "gradient-motion": "full-motion",
  "text-heavy-ui": "text-heavy",
};

const requestedScenarios = parseScenarioNames(process.env.SIMX_BENCH_SCENARIOS || "all");

async function main() {
  let browser = null;
  let leaseStarted = false;
  try {
    sceneServer = await startSceneServer();
    let leaseInfo = null;
    if (autoLease) {
      leaseInfo = await startLease();
      leaseStarted = true;
    } else if (process.env.SIMX_BENCH_UDID) {
      leaseInfo = { udid: process.env.SIMX_BENCH_UDID };
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
        consoleIssues.push({ atMs: Date.now(), type: msg.type(), text });
      }
    });

    await page.goto(targetUrl, { waitUntil: "domcontentloaded" });
    await waitForViewer(page);
    const renderedInitialFrame = await waitForInitialFrame(page);
    const screen = await page.locator("#screen").boundingBox();
    if (!screen) throw new Error("viewer screen was not visible");

    const scenarioReports = [];
    for (const scenarioName of requestedScenarios) {
      const scenario = scenarioDefinitions[scenarioName];
      if (!scenario) {
        throw new Error(`unknown benchmark scenario: ${scenarioName}`);
      }
      scenarioReports.push(
        await runScenario(page, screen, scenarioName, scenario, leaseInfo, consoleIssues)
      );
    }

    const report = buildSuiteReport(page.url(), renderedInitialFrame, scenarioReports);
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
    if (sceneServer !== null) {
      await closeSceneServer();
    }
  }
}

async function waitForViewer(page) {
  await page.waitForFunction(
    () => document.getElementById("status")?.textContent !== "Connecting...",
    null,
    { timeout: 10_000 }
  );
}

async function waitForInitialFrame(page) {
  try {
    await page.waitForFunction(
      () => {
        const metrics = JSON.parse(document.getElementById("metrics")?.textContent || "{}");
        return metrics.renderedFrames > 0;
      },
      null,
      { timeout: 15_000 }
    );
    return true;
  } catch (_) {
    return false;
  }
}

async function runScenario(page, screen, name, scenario, leaseInfo, consoleIssues) {
  const setup = await setupScenario(scenario, leaseInfo);
  await page.waitForTimeout(sceneSettleMs);
  const baselineStats = await readStats(page.url());
  await page.evaluate(() => window.simxMetrics?.reset?.());
  const startedAt = Date.now();
  const deadline = startedAt + scenarioDurationMs;
  await scenario.drive(page, screen, deadline);
  await page.waitForTimeout(250);

  const metrics = await readMetrics(page);
  const stats = await readStats(page.url());
  const status = await page.locator("#status").textContent();
  const scenarioConsoleIssues = consoleIssues.filter((issue) => issue.atMs >= startedAt);
  return buildScenarioReport({
    name,
    title: scenario.title,
    description: scenario.description,
    setup,
    status,
    metrics,
    stats,
    baselineStats,
    consoleIssues: scenarioConsoleIssues,
  });
}

async function setupScenario(scenario, leaseInfo) {
  const sceneUrl = `${sceneBaseUrl}/scenes/${scenario.scene}`;
  const setup = {
    scene: scenario.scene,
    sceneUrl,
    openedInSimulator: false,
    skipped: null,
  };
  if (!leaseInfo?.udid) {
    setup.skipped =
      "No simulator UDID was available. Set SIMX_BENCH_AUTO_LEASE=1 or SIMX_BENCH_UDID to open benchmark scenes automatically.";
    return setup;
  }
  openSimulatorUrl(leaseInfo.udid, sceneUrl);
  setup.openedInSimulator = true;
  return setup;
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
      "--control-mode",
      controlMode,
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
    const leaseInfo = parseFirstJsonObject(stdout);
    try {
      const response = await fetch(healthUrl);
      if (response.ok && leaseInfo) return leaseInfo;
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

function openSimulatorUrl(udid, url) {
  const result = spawnSync("xcrun", ["simctl", "openurl", udid, url], { encoding: "utf8" });
  if (result.status !== 0) {
    throw new Error(`failed to open benchmark scene in simulator: ${result.stderr || result.stdout}`);
  }
}

async function startSceneServer() {
  const server = http.createServer((request, response) => {
    const url = new URL(request.url, `http://${request.headers.host || `${sceneHost}:${scenePort}`}`);
    if (url.pathname === "/health") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ status: "ok" }));
      return;
    }
    const match = url.pathname.match(/^\/scenes\/([^/]+)$/);
    if (!match || !scenarioDefinitions[match[1]]) {
      response.writeHead(404, { "content-type": "text/plain; charset=utf-8" });
      response.end("Not found\n");
      return;
    }
    response.writeHead(200, {
      "content-type": "text/html; charset=utf-8",
      "cache-control": "no-store",
    });
    response.end(sceneHtml(match[1]));
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(scenePort, sceneHost, resolve);
  });
  return server;
}

async function closeSceneServer() {
  await new Promise((resolve) => sceneServer.close(resolve));
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function tap(page, box, nx, ny) {
  const x = box.x + box.width * nx;
  const y = box.y + box.height * ny;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.waitForTimeout(45);
  await page.mouse.up();
}

async function drag(page, box, direction, steps) {
  const x = box.x + box.width * 0.5;
  const startY = direction === "up" ? 0.78 : 0.22;
  const endY = direction === "up" ? 0.22 : 0.78;
  await page.mouse.move(x, box.y + box.height * startY);
  await page.mouse.down();
  await page.mouse.move(x, box.y + box.height * endY, { steps });
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

function buildSuiteReport(url, renderedInitialFrame, scenarios) {
  const checks = {
    renderedInitialFrame,
    scenariosPassed: scenarios.every((scenario) => scenario.ok),
  };
  const failures = Object.entries(checks)
    .filter(([, passed]) => !passed)
    .map(([name]) => name);
  const report = {
    ok: Object.values(checks).every(Boolean),
    transport: "h264-websocket-webcodecs",
    url,
    scenarioNames: scenarios.map((scenario) => scenario.name),
    durationMs: scenarioDurationMs,
    totalScenarioDurationMs: scenarioDurationMs * scenarios.length,
    sceneBaseUrl,
    lease: {
      autoLease,
      slug: autoLease ? leaseSlug : null,
      port: autoLease ? leasePort : null,
      fps: autoLease ? leaseFps : null,
      controlMode: autoLease ? controlMode : null,
    },
    thresholds,
    checks,
    failures,
    scenarioResults: scenarios,
  };
  if (scenarios.length === 1) {
    report.metrics = scenarios[0].metrics;
    report.serverStats = scenarios[0].serverStats;
    report.consoleIssues = scenarios[0].consoleIssues;
    report.derived = scenarios[0].derived;
    report.scenarios = scenarios;
  }
  return report;
}

function buildScenarioReport({
  name,
  title,
  description,
  setup,
  status,
  metrics,
  stats,
  baselineStats,
  consoleIssues,
}) {
  const dropRate = serverDropRate(stats, baselineStats);
  const targetMissRate = serverTargetMissRate(stats);
  const checks = {
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
  const failures = Object.entries(checks)
    .filter(([, passed]) => !passed)
    .map(([checkName]) => checkName);
  return {
    ok: Object.values(checks).every(Boolean),
    name,
    title,
    description,
    status,
    durationMs: scenarioDurationMs,
    setup,
    checks,
    failures,
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

function serverDropRate(stats, baselineStats) {
  if (!stats || !baselineStats) return null;
  const sourceFrames = Number(stats.source_frames) - Number(baselineStats.source_frames);
  const droppedFrames = Number(stats.dropped_frames) - Number(baselineStats.dropped_frames);
  if (
    !Number.isFinite(sourceFrames) ||
    !Number.isFinite(droppedFrames) ||
    sourceFrames <= 0 ||
    droppedFrames < 0
  ) {
    return null;
  }
  return Number((droppedFrames / sourceFrames).toFixed(4));
}

function parseScenarioNames(value) {
  const names = value
    .split(",")
    .map((name) => name.trim())
    .filter(Boolean);
  if (names.length === 1 && names[0] === "all") {
    return Object.keys(scenarioDefinitions);
  }
  return names.map((name) => scenarioAliases[name] || name);
}

function parseFirstJsonObject(text) {
  const start = text.indexOf("{");
  if (start === -1) return null;
  let depth = 0;
  let inString = false;
  let escaped = false;
  for (let index = start; index < text.length; index += 1) {
    const char = text[index];
    if (inString) {
      if (escaped) {
        escaped = false;
      } else if (char === "\\") {
        escaped = true;
      } else if (char === "\"") {
        inString = false;
      }
      continue;
    }
    if (char === "\"") {
      inString = true;
    } else if (char === "{") {
      depth += 1;
    } else if (char === "}") {
      depth -= 1;
      if (depth === 0) {
        return JSON.parse(text.slice(start, index + 1));
      }
    }
  }
  return null;
}

function sceneHtml(name) {
  const bodies = {
    "static-taps": `
      <main class="static">
        <h1>Checkout Controls</h1>
        <section class="grid">
          ${Array.from({ length: 9 }, (_, index) => `<button>Action ${index + 1}</button>`).join("")}
        </section>
        <p class="caption">Mostly static scene. Taps change button states but should not create continuous motion.</p>
      </main>
      <script>
        document.querySelectorAll("button").forEach((button) => {
          button.addEventListener("click", () => button.classList.toggle("selected"));
        });
      </script>
    `,
    "smooth-scroll": `
      <main class="list">
        <header><h1>Inbox</h1><p>120 rows with mixed icon, text, and color detail.</p></header>
        ${Array.from({ length: 120 }, (_, index) => `
          <article class="row">
            <span class="badge">${String(index + 1).padStart(2, "0")}</span>
            <div><strong>Simulator benchmark row ${index + 1}</strong><p>Scrolling text detail, secondary copy, and stable separators.</p></div>
          </article>
        `).join("")}
      </main>
    `,
    "keyboard-entry": `
      <main class="keyboard">
        <h1>Keyboard Entry</h1>
        <label for="entry">Streaming notes</label>
        <textarea id="entry" autofocus spellcheck="false">Tap here, then type benchmark text. </textarea>
        <p class="caption">Hardware-key messages should update this field while the H.264 stream stays smooth.</p>
      </main>
    `,
    "animation-heavy": `
      <main class="animation-heavy">
        <h1>Motion Board</h1>
        <div class="stage">
          ${Array.from(
            { length: 48 },
            (_, index) =>
              `<i style="--i:${index};--x:${(index % 8) * 12};--y:${(index % 6) * 14}"></i>`
          ).join("")}
        </div>
        <button>Pulse</button>
      </main>
    `,
    "full-motion": `
      <main class="gradient-motion">
        <h1>Gradient Motion</h1>
        <p>Full-screen color movement stresses inter-frame encoding and browser presentation.</p>
      </main>
    `,
    "text-heavy": `
      <main class="text-heavy">
        <h1>Ledger Review</h1>
        <table>
          <thead><tr><th>Account</th><th>Status</th><th>Amount</th></tr></thead>
          <tbody>
            ${Array.from({ length: 96 }, (_, index) => `
              <tr>
                <td>Northwest Operations ${index + 1}</td>
                <td>${index % 3 === 0 ? "Needs review" : "Reconciled"}</td>
                <td>$${(143 + index * 17).toLocaleString()}</td>
              </tr>
            `).join("")}
          </tbody>
        </table>
      </main>
    `,
  };
  return `<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>simx ${name}</title>
  <style>
    :root { color-scheme: light; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
    * { box-sizing: border-box; }
    body { margin: 0; min-height: 100vh; background: #f7f8fb; color: #151923; }
    main { min-height: 100vh; padding: 28px 18px; }
    h1 { margin: 0 0 14px; font-size: 32px; letter-spacing: 0; }
    p { line-height: 1.35; }
    button { min-height: 52px; border: 1px solid #b9c2d0; border-radius: 8px; background: #fff; color: #151923; font: inherit; font-weight: 700; }
    button.selected { background: #1f6feb; color: #fff; }
    .caption { color: #526070; }
    .grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 12px; margin: 24px 0; }
    .list { padding: 0; background: #eef2f8; }
    .list header { position: sticky; top: 0; z-index: 2; padding: 22px 18px 14px; background: rgba(247, 248, 251, 0.94); backdrop-filter: blur(12px); border-bottom: 1px solid #d7dde7; }
    .row { display: flex; gap: 12px; align-items: center; min-height: 78px; padding: 12px 16px; border-bottom: 1px solid #d7dde7; background: #fff; }
    .row p { margin: 4px 0 0; color: #667386; font-size: 14px; }
    .badge { width: 42px; height: 42px; display: grid; place-items: center; border-radius: 50%; background: #1f6feb; color: #fff; font-weight: 800; }
    .keyboard textarea { width: 100%; min-height: 280px; padding: 16px; border: 2px solid #1f6feb; border-radius: 8px; font: 20px ui-monospace, SFMono-Regular, Menlo, monospace; line-height: 1.35; }
    .keyboard label { display: block; margin-bottom: 8px; font-weight: 800; }
    .animation-heavy { overflow: hidden; background: #10141d; color: #f7f8fb; }
    .stage { position: relative; height: 70vh; border: 1px solid #384153; overflow: hidden; background: radial-gradient(circle at 30% 20%, #284d8f, #10141d 48%); }
    .stage i { position: absolute; width: 42px; height: 42px; border-radius: 12px; background: hsl(calc(var(--i) * 17), 82%, 58%); left: calc(var(--x) * 1%); top: calc(var(--y) * 1%); animation: drift calc(1.4s + var(--i) * .02s) ease-in-out infinite alternate; opacity: .86; }
    @keyframes drift { from { transform: translate3d(0, 0, 0) rotate(0deg); } to { transform: translate3d(44px, 68px, 0) rotate(180deg); } }
    .gradient-motion { display: grid; align-content: center; min-height: 100vh; color: white; background: linear-gradient(120deg, #1f6feb, #21a67a, #f0b429, #db3a34); background-size: 400% 400%; animation: gradientShift 2.2s linear infinite; }
    .gradient-motion h1, .gradient-motion p { padding: 0 20px; text-shadow: 0 2px 16px rgba(0, 0, 0, .34); }
    @keyframes gradientShift { 0% { background-position: 0% 50%; } 50% { background-position: 100% 50%; } 100% { background-position: 0% 50%; } }
    .text-heavy { padding: 16px 10px; background: #fff; }
    table { width: 100%; border-collapse: collapse; font-size: 13px; }
    th, td { padding: 8px 6px; border-bottom: 1px solid #d7dde7; text-align: left; white-space: nowrap; }
    th { position: sticky; top: 0; background: #151923; color: #fff; }
  </style>
</head>
<body>${bodies[name]}</body>
</html>`;
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
