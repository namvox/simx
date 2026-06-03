const { chromium } = require("playwright");

const targetUrl = process.env.SIMX_VIEWER_URL || "http://127.0.0.1:8081/codex-longpress-test/";

async function main() {
  const browser = await chromium.launch({ channel: process.env.PLAYWRIGHT_CHANNEL || "chrome", headless: true });
  try {
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
    const sentTouches = [];
    const consoleIssues = [];
    let receivedJpeg = false;

    page.on("console", (msg) => {
      const text = msg.text();
      if (
        ["error", "warning"].includes(msg.type()) &&
        !text.includes("favicon") &&
        !text.includes("Failed to load resource: the server responded with a status of 404")
      ) {
        consoleIssues.push({ type: msg.type(), text });
      }
    });
    page.on("websocket", (ws) => {
      ws.on("framesent", (event) => {
        if (typeof event.payload !== "string") return;
        try {
          const message = JSON.parse(event.payload);
          if (message.type === "touch") sentTouches.push(message);
        } catch (_) {}
      });
      ws.on("framereceived", (event) => {
        if (
          typeof event.payload !== "string" &&
          event.payload[0] === 0xff &&
          event.payload[1] === 0xd8 &&
          event.payload[2] === 0xff
        ) {
          receivedJpeg = true;
        }
      });
    });

    await page.goto(targetUrl, { waitUntil: "domcontentloaded" });
    await page.waitForFunction(() => document.getElementById("status")?.textContent === "Live", null, { timeout: 10_000 });
    await page.waitForFunction(() => document.getElementById("frame")?.src?.startsWith("blob:"), null, { timeout: 10_000 });

    const box = await page.locator("#screen").boundingBox();
    if (!box) throw new Error("viewer screen was not visible");

    await drag(page, box, "left");
    await page.waitForTimeout(250);
    await drag(page, box, "right");
    await page.waitForTimeout(500);

    const swipes = splitSwipes(sentTouches);
    assertSwipe(swipes[0], "left");
    assertSwipe(swipes[1], "right");
    if (!receivedJpeg) throw new Error("stream did not emit a binary JPEG frame");
    if (consoleIssues.length > 0) {
      throw new Error(`unexpected console issues: ${JSON.stringify(consoleIssues)}`);
    }

    console.log(
      JSON.stringify(
        {
          ok: true,
          url: page.url(),
          swipes: swipes.map((swipe) => ({
            phases: swipe.map((message) => message.phase),
            start: swipe[0],
            end: swipe[swipe.length - 1],
          })),
          receivedJpeg,
        },
        null,
        2
      )
    );
  } finally {
    await browser.close();
  }
}

async function drag(page, box, direction) {
  const startX = direction === "left" ? 0.82 : 0.18;
  const endX = direction === "left" ? 0.18 : 0.82;
  const y = direction === "left" ? 0.5 : 0.515;

  await page.mouse.move(box.x + box.width * startX, box.y + box.height * y);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width * endX, box.y + box.height * y, { steps: 12 });
  await page.mouse.up();
}

function splitSwipes(touches) {
  const swipes = [];
  let current = [];
  for (const touch of touches) {
    current.push(touch);
    if (touch.phase === "ended" || touch.phase === "cancelled") {
      swipes.push(current);
      current = [];
    }
  }
  return swipes;
}

function assertSwipe(swipe, direction) {
  if (!swipe) throw new Error(`missing ${direction} swipe`);
  const phases = swipe.map((message) => message.phase);
  const movedCount = phases.filter((phase) => phase === "moved").length;
  if (phases[0] !== "began" || movedCount < 4 || phases[phases.length - 1] !== "ended") {
    throw new Error(`invalid ${direction} swipe phases: ${JSON.stringify(phases)}`);
  }

  const start = swipe[0];
  const end = swipe[swipe.length - 1];
  if (direction === "left" && !(end.nx < start.nx)) {
    throw new Error(`left swipe did not move left: ${start.nx} -> ${end.nx}`);
  }
  if (direction === "right" && !(end.nx > start.nx)) {
    throw new Error(`right swipe did not move right: ${start.nx} -> ${end.nx}`);
  }
  if (end.pressure !== 0) {
    throw new Error(`${direction} swipe did not release pressure`);
  }
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
