const PANE = "p1";
const NAMED = ["ArrowUp","ArrowDown","ArrowLeft","ArrowRight","Home","End",
  "PageUp","PageDown","Insert","Delete","Enter","Tab","Backspace","Escape"];
const NOFIT = new URLSearchParams(location.search).has("nofit");
const DEPTH = parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--depth")) || 420;

function keyEvent(ev) {
  if (["Shift","Control","Alt","Meta","CapsLock"].includes(ev.key)) return null;
  const mods = (ev.shiftKey?1:0)|(ev.altKey?2:0)|(ev.ctrlKey?4:0)|(ev.metaKey?8:0);
  if (ev.key.length === 1) {
    if (ev.metaKey) return null;
    if (ev.altKey && !ev.ctrlKey) {
      const codeLetter = /^Key([A-Z])$/.exec(ev.code)?.[1]?.toLowerCase();
      if (!(codeLetter && ev.key.toLowerCase() === codeLetter)) {
        return {t:"key", key: ev.key, mods: mods & ~2};
      }
    }
    return {t:"key", key: ev.key, mods};
  }
  if (NAMED.includes(ev.key) || /^F\d+$/.test(ev.key)) {
    return {t:"key", key: ev.key, mods};
  }
  return null;
}

const queue = {items: [], sending: false};
function send(frame) {
  queue.items.push(JSON.stringify(frame));
  if (side && frame.t !== "resize") spawnPost(frame);
  drain();
}
async function drain() {
  if (queue.sending) return;
  queue.sending = true;
  while (queue.items.length) {
    const batch = queue.items.splice(0).join("\n") + "\n";
    try { await fetch("/input?pane=" + PANE, {method:"POST", body: batch}); } catch {}
  }
  queue.sending = false;
}

const ta = document.createElement("textarea");
ta.id = "compose";
ta.setAttribute("aria-hidden", "true");
ta.tabIndex = -1;
ta.autocapitalize = "off"; ta.autocomplete = "off"; ta.spellcheck = false;
ta.style.cssText = "position:fixed;top:0;left:0;width:1px;height:1px;opacity:0;padding:0;border:0;outline:0;resize:none;overflow:hidden;z-index:-1;";
document.body.appendChild(ta);
let composing = false;
ta.addEventListener("compositionstart", () => { composing = true; });
ta.addEventListener("compositionend", ev => {
  composing = false;
  const data = ev.data || ta.value;
  ta.value = "";
  if (data) send({t:"input", b: data});
});
function parkFocus() {
  requestAnimationFrame(() => {
    const sel = getSelection();
    if (sel && !sel.isCollapsed) return;
    try { ta.focus(); } catch {}
  });
}

const pane = document.querySelector(".pane");
const scrollEl = pane.querySelector(".scroll");
const beam = document.getElementById("beam");
const toggle = document.getElementById("toggle");
let side = false;
let stickOn = true;
let lastWrote = 0;
let cellCache = null;
let lastFit = null;
let fitTimer;

function measureCell() {
  if (cellCache) return cellCache;
  const probe = document.createElement("div");
  probe.className = "cell-probe";
  probe.textContent = "M".repeat(80);
  document.body.appendChild(probe);
  const r = probe.getBoundingClientRect();
  probe.remove();
  if (!r.width || !r.height) return {w: 0, h: 0};
  cellCache = {w: r.width / 80, h: r.height};
  return cellCache;
}

function fit() {
  if (NOFIT) return;
  const cell = measureCell();
  if (!cell.w || !cell.h) return;
  if (!scrollEl.clientWidth || !scrollEl.clientHeight) return;
  const cols = Math.max(20, Math.floor((scrollEl.clientWidth - 16) / cell.w));
  const rows = Math.max(5, Math.floor(pane.clientHeight / cell.h));
  const h = rows * cell.h + "px";
  if (scrollEl.style.height !== h) {
    scrollEl.style.flex = "none";
    scrollEl.style.height = h;
    if (stickOn) toBottom();
  }
  if (!lastFit || lastFit.cols !== cols || lastFit.rows !== rows) {
    lastFit = {cols, rows};
    send({t:"resize", cols, rows});
  }
  sizeWez();
}
function scheduleFit() {
  clearTimeout(fitTimer);
  fitTimer = setTimeout(fit, 80);
}

function bottomTarget(box) {
  // Layout coords, not getBoundingClientRect: the stage's 3D transform would
  // mix viewport pixels into scrollTop and unpin the pane while it is side-on.
  const grid = box.firstElementChild;
  let target = 0;
  const rows = box.querySelectorAll(".row");
  for (let i = rows.length - 1; i >= 0; i--) {
    if (rows[i].textContent.trim() !== "") {
      target = rows[i].offsetTop + rows[i].offsetHeight;
      break;
    }
  }
  const cur = box.querySelector(".cursor");
  if (cur) target = Math.max(target, cur.offsetTop + cur.offsetHeight);
  if (grid && target) target += grid.offsetTop || 0;
  const max = box.scrollHeight - box.clientHeight;
  return target ? Math.max(0, Math.min(Math.ceil(target) - box.clientHeight, max)) : max;
}
function atBottom(box) {
  return box.scrollTop >= bottomTarget(box) - 8;
}
function toBottom() {
  const top = bottomTarget(scrollEl);
  if (scrollEl.scrollTop !== top) scrollEl.scrollTop = top;
  lastWrote = top;
}

scrollEl.addEventListener("scroll", () => {
  if (scrollEl.scrollTop === lastWrote) return;
  stickOn = atBottom(scrollEl);
  pane.classList.toggle("unstuck", !stickOn);
}, {passive: true});

function snippetOf(n) {
  if (!n) return null;
  if (n.nodeType !== 1) {
    const t = (n.textContent || "").replace(/\s+/g, " ").trim();
    return t ? t.slice(0, 28) : null;
  }
  const tag = n.tagName.toLowerCase();
  const cls = typeof n.className === "string" ? n.className.trim().split(/\s+/)[0] : "";
  const text = (n.textContent || "").replace(/\s+/g, " ").trim().slice(0, 18);
  const open = cls ? `<${tag} class="${cls}">` : `<${tag}>`;
  return text ? `${open}${text}` : open;
}
function snippetsFrom(muts) {
  const out = [];
  for (const m of muts) {
    if (out.length >= 5) break;
    for (const n of m.addedNodes) {
      if (out.length >= 5) break;
      if (n.nodeType === 1 && n.id && String(n.id).startsWith("grid-")) {
        const rows = n.querySelectorAll(".row");
        const pick = [...rows].slice(-3);
        for (const r of pick) {
          const s = snippetOf(r);
          if (s) out.push(s);
        }
        continue;
      }
      const s = snippetOf(n);
      if (s) out.push(s);
    }
    if (m.type === "characterData") {
      const s = (m.target.data || "").replace(/\s+/g, " ").trim();
      if (s) out.push(s.slice(0, 48));
    }
  }
  return out;
}

function spawnFlyer(text, kind) {
  if (!side || !beamReady || !text) return;
  while (beam.childElementCount > 28) beam.firstElementChild.remove();
  const el = document.createElement("div");
  el.className = "fly " + kind;
  el.textContent = text;
  // Right edge is the 3D point. rotateY(-90) maps local +X to +Z (through the
  // pane). Pinning the right edge keeps the whole string at z <= point, i.e.
  // in the shaft. translate(-50%,-50%) used to center the chip on the pane
  // so POSTs spawned on the camera side.
  el.style.transformOrigin = "100% 50%";
  beam.appendChild(el);
  const yWez = (Math.random() - 0.5) * 80;
  const yPane = (Math.random() - 0.5) * pane.clientHeight * 0.5;
  const face = " rotateY(-90deg)";
  const zWez = -(DEPTH - 50);
  const zPane = -40;
  const at = (y, z) => `translate(0,-50%) translate3d(0px,${y}px,${z}px)` + face;
  const from = kind === "pkt" ? at(yPane, zPane) : at(yWez, zWez);
  const to = kind === "pkt" ? at(yWez, zWez) : at(yPane, zPane);
  el.animate([
    {transform: from, opacity: 0},
    {transform: from, opacity: 1, offset: 0.06},
    {transform: to, opacity: 0.9, offset: 0.82},
    {transform: to, opacity: 0.12}
  ], {duration: kind === "pkt" ? 1100 : 1600, easing: "linear"}).finished
    .then(() => el.remove())
    .catch(() => el.remove());
}

function spawnPost(frame) {
  let clip;
  if (frame.t === "key") clip = `key ${JSON.stringify(frame.key)}`;
  else if (frame.t === "input") clip = "input";
  else if (frame.t === "paste") clip = "paste";
  else clip = frame.t;
  spawnFlyer("POST /input " + clip, "pkt");
}

function seedBeam() {
  const rows = pane.querySelectorAll(".row");
  const pick = [...rows].filter(r => r.textContent.trim()).slice(-6);
  for (const r of pick) spawnFlyer(snippetOf(r), "html");
}

const orbit = document.querySelector(".orbit");
const stage = document.querySelector(".stage");
const wezEl = document.querySelector(".wez");
const wezScreen = document.querySelector(".wez-screen");
let beamReady = false;
let beamWait = 0;

function sizeWez() {
  if (!wezEl || !pane.clientWidth || !pane.clientHeight) return;
  const h = 200;
  const w = h * (pane.clientWidth / pane.clientHeight);
  wezEl.style.width = w + "px";
  wezEl.style.height = h + "px";
  wezEl.style.margin = `${-h / 2}px 0 0 ${-w / 2}px`;
  syncWezScreen();
}

function syncWezScreen() {
  const src = document.getElementById("grid-" + PANE);
  if (!src || !wezScreen) return;
  const clone = src.cloneNode(true);
  clone.removeAttribute("id");
  for (const el of clone.querySelectorAll("[id]")) el.removeAttribute("id");
  wezScreen.replaceChildren(clone);
  const iw = Math.max(clone.scrollWidth, clone.offsetWidth, 1);
  const dw = wezScreen.clientWidth;
  if (!dw) return;
  const s = dw / iw;
  clone.style.transformOrigin = "top left";
  clone.style.transform = `scale(${s})`;
  // Match the pane viewport, not the clone's layout bottom (blank pty rows).
  clone.style.top = (-(scrollEl.scrollTop) * s) + "px";
}

function openBeam() {
  if (!side || beamReady) return;
  beamReady = true;
  seedBeam();
}

function finishTurn() {
  toggle.textContent = side ? "face on" : "see through";
  openBeam();
}

function setSide(on) {
  side = !!on;
  beamReady = false;
  clearTimeout(beamWait);
  document.body.classList.toggle("side", side);
  toggle.setAttribute("aria-pressed", side ? "true" : "false");
  beam.replaceChildren();
  // Wait out the yaw both ways so the button matches the picture.
  beamWait = setTimeout(finishTurn, 1200);
  parkFocus();
}

function onTurnEnd(ev) {
  if (ev.propertyName && ev.propertyName !== "transform") return;
  finishTurn();
}
orbit.addEventListener("transitionend", onTurnEnd);
stage.addEventListener("transitionend", onTurnEnd);

toggle.addEventListener("click", ev => {
  ev.preventDefault();
  setSide(!side);
});

new MutationObserver(muts => {
  if (stickOn) toBottom();
  syncWezScreen();
  if (side) {
    for (const s of snippetsFrom(muts)) spawnFlyer(s, "html");
  }
}).observe(pane, {childList: true, subtree: true, characterData: true});

addEventListener("resize", () => { cellCache = null; scheduleFit(); });
document.fonts.ready.then(() => { cellCache = null; fit(); });

document.addEventListener("copy", ev => {
  const sel = getSelection(); if (!sel) return;
  const text = sel.toString(); if (!text) return;
  const trimmed = text.split("\n").map(l => l.replace(/[ \t]+$/,"")).join("\n");
  if (trimmed === text) return;
  ev.clipboardData?.setData("text/plain", trimmed);
  ev.preventDefault();
});
addEventListener("paste", ev => {
  const text = ev.clipboardData?.getData("text");
  if (!text) return;
  ev.preventDefault();
  send({t:"paste", s:text});
});

document.addEventListener("click", ev => {
  if (ev.target.closest("#chrome")) return;
  parkFocus();
});

document.addEventListener("keydown", ev => {
  if (ev.isComposing || ev.keyCode === 229 || composing) return;
  if (ev.target && ev.target.id === "toggle") {
    if (ev.key === "Enter" || ev.key === " ") return;
    parkFocus();
  }
  const t = ev.target, tag = t && t.tagName;
  if (t !== ta && (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || (t && t.isContentEditable))) return;
  if (ev.ctrlKey && !ev.altKey && !ev.metaKey && ev.key === "c") {
    const sel = getSelection().toString();
    if (sel) { navigator.clipboard.writeText(sel).catch(()=>{}); ev.preventDefault(); return; }
  }
  const frame = keyEvent(ev);
  if (frame === null) return;
  if (document.activeElement !== ta) ta.focus();
  ev.preventDefault();
  ta.value = "";
  send(frame);
}, {capture: true});

fit();
syncWezScreen();
parkFocus();
