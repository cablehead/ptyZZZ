import { chromium } from "playwright";
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(HERE, "../..");
const HTTP_NU = process.env.HTTP_NU || "http-nu";
const OUT = process.env.SHOT_DIR || join(HERE, "shots");
mkdirSync(OUT, { recursive: true });

const port = 3997;
const base = `http://127.0.0.1:${port}`;
const store = mkdtempSync(join(tmpdir(), "panes-shot-"));
const srv = spawn(
  HTTP_NU,
  ["--dev", "--datastar", "--services", "--store", store, `127.0.0.1:${port}`, join(HERE, "serve.nu")],
  { cwd: ROOT, stdio: "ignore" },
);
const reap = () => {
  try { srv.kill("SIGKILL"); } catch {}
  spawnSync("pkill", ["-9", "-f", store]);
};
process.on("exit", reap);

async function waitReady() {
  for (let i = 0; i < 80; i++) {
    try { if ((await fetch(base)).ok) return; } catch {}
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("server did not start");
}
await waitReady();

const launchOpts = { args: ["--no-sandbox", "--disable-dev-shm-usage"] };
if (process.env.CHROMIUM_PATH) launchOpts.executablePath = process.env.CHROMIUM_PATH;
const browser = await chromium.launch(launchOpts);
const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 } });
const page = await ctx.newPage();
const errors = [];
page.on("pageerror", (e) => errors.push(String(e)));
page.on("console", (m) => { if (m.type() === "error" || m.type() === "warning") errors.push(m.type() + ": " + m.text()); });

async function shot(name) {
  const path = join(OUT, name + ".png");
  await page.screenshot({ path, fullPage: false });
  console.log("shot " + path);
}

await page.goto(base, { waitUntil: "domcontentloaded" });
await page.waitForSelector("#grid-p1 .row", { timeout: 12000 });
await page.waitForTimeout(400);
await shot("01-one-pane-navigate");

await page.click(".pane");
await page.waitForTimeout(200);
await shot("02-one-pane-focus");

await page.keyboard.press("Control+Enter"); // back to navigate on linux
await page.waitForTimeout(100);
await page.keyboard.press("n");
await page.waitForSelector("#grid-p2 .row", { timeout: 8000 });
await page.keyboard.press("h");
await page.waitForTimeout(400);
await shot("03-two-columns");

await page.locator(".pane[data-pane='p1']").click();
await page.keyboard.press("Control+Enter");
await page.keyboard.press("s");
await page.waitForSelector("#grid-p3", { timeout: 8000 });
await page.waitForTimeout(400);
await shot("04-split-column");

await page.keyboard.press("f");
await page.waitForTimeout(500);
await shot("05-zoom");
const zoomed = await page.evaluate(() => {
  const strip = document.getElementById("strip").getBoundingClientRect();
  const vis = [...document.querySelectorAll(".pane")].filter((p) => p.offsetParent);
  const box = vis[0]?.getBoundingClientRect();
  return {
    visible: vis.length,
    pane: vis[0]?.dataset.pane,
    fills: box ? box.width > strip.width - 4 && box.height > strip.height - 4 : false,
  };
});
console.log("zoom " + JSON.stringify(zoomed));
if (zoomed.visible !== 1) throw new Error("zoom left " + zoomed.visible + " panes visible");
if (!zoomed.fills) throw new Error("zoomed pane does not fill the strip");

await page.keyboard.press("f");
await page.waitForTimeout(500);
await shot("06-unzoom");
const restored = await page.evaluate(() =>
  [...document.querySelectorAll(".pane")].filter((p) => p.offsetParent).length);
console.log("unzoom_visible " + restored);
if (restored !== 3) throw new Error("unzoom restored " + restored + " panes, want 3");

await page.keyboard.press("Control+k");
await page.waitForTimeout(250);
await shot("07-modk-panel");
await page.keyboard.press("Escape");

await page.locator(".pane[data-pane='p1']").click();
await page.waitForTimeout(150);
await page.keyboard.type("echo vis-ok");
await page.keyboard.press("Enter");
await page.waitForFunction(
  () => document.querySelector("#grid-p1")?.innerText.includes("vis-ok"),
  null,
  { timeout: 8000 },
);
await shot("08-typed");

await page.keyboard.press("Control+Enter");
for (const id of ["p3", "p2", "p1"]) {
  const el = page.locator(`.pane[data-pane='${id}']`);
  if (await el.count()) {
    await el.click();
    await page.keyboard.press("Control+Enter");
    await page.keyboard.press("Control+k");
    await page.waitForTimeout(200);
    await page.keyboard.press("x");
    await page.waitForTimeout(300);
  }
}
await page.waitForSelector("#empty:not([hidden])", { timeout: 5000 });
await shot("09-empty");

await page.keyboard.press("n");
await page.waitForSelector(".pane", { timeout: 8000 });
await page.waitForTimeout(400);
await shot("10-reopen");

const summary = await page.evaluate(() => ({
  panes: document.querySelectorAll(".pane").length,
  columns: document.querySelectorAll(".column").length,
  emptyHidden: document.getElementById("empty")?.hidden,
  hasGrid: !!document.querySelector("[id^='grid-'] .row"),
}));
console.log("summary " + JSON.stringify(summary));
if (summary.panes < 1) throw new Error("reopen left no panes");

if (errors.length) console.log("console_errors " + JSON.stringify(errors.slice(0, 8)));
await browser.close();
reap();
