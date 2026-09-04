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

const port = 3998;
const base = `http://127.0.0.1:${port}`;
const store = mkdtempSync(join(tmpdir(), "through-shot-"));
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
page.on("console", (m) => { if (m.type() === "error") errors.push("error: " + m.text()); });

async function shot(name) {
  const path = join(OUT, name + ".png");
  await page.screenshot({ path, fullPage: false });
  console.log("shot " + path);
}

await page.goto(base, { waitUntil: "domcontentloaded" });
await page.waitForSelector("#grid-p1 .row", { timeout: 12000 });
await page.waitForTimeout(400);
await shot("01-front");

await page.keyboard.type("echo through-vis");
await page.keyboard.press("Enter");
await page.waitForFunction(
  () => document.querySelector("#grid-p1")?.innerText.includes("through-vis"),
  null,
  { timeout: 8000 },
);
await shot("02-typed");

await page.click("#toggle");
await page.waitForFunction(() => document.body.classList.contains("side"));
await page.waitForTimeout(1100);
await shot("03-side");

await page.keyboard.type("ls");
await page.waitForTimeout(800);
await shot("04-side-typing");

const info = await page.evaluate(() => {
  const wez = document.querySelector(".wez")?.getBoundingClientRect();
  const slab = document.querySelector(".slab")?.getBoundingClientRect();
  const flies = document.querySelectorAll(".fly").length;
  return {
    side: document.body.classList.contains("side"),
    flies,
    rtt: document.getElementById("link")?.textContent,
    wez: wez && { x: Math.round(wez.x), y: Math.round(wez.y), w: Math.round(wez.width), h: Math.round(wez.height) },
    slab: slab && { x: Math.round(slab.x), y: Math.round(slab.y), w: Math.round(slab.width), h: Math.round(slab.height) },
  };
});
console.log("info " + JSON.stringify(info));
if (!info.side) throw new Error("side class missing after toggle");
if (!info.wez || info.wez.w < 10 || info.wez.h < 10) throw new Error("wezterm box not visible");
if (!info.slab || info.slab.h < 80) throw new Error("pane not visible on the side");
if (info.wez.x + info.wez.w > info.slab.x - 8) throw new Error("wezterm not separated behind the pane");

await page.click("#toggle");
await page.waitForFunction(() => !document.body.classList.contains("side"));
await page.waitForTimeout(1100);
await shot("05-front-again");

await browser.close();
if (errors.length) {
  console.log("errors\n" + errors.join("\n"));
  process.exitCode = 1;
}
reap();
