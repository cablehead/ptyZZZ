const NAMED = ["ArrowUp","ArrowDown","ArrowLeft","ArrowRight","Home","End",
  "PageUp","PageDown","Insert","Delete","Enter","Tab","Backspace","Escape"];
const NOFIT = new URLSearchParams(location.search).has("nofit");
const ACTIONS_HINT_MS = 150;

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

const queues = {};
function send(pane, frame) {
  if (!pane) return;
  const s = queues[pane] ??= {items: [], sending: false};
  s.items.push(JSON.stringify(frame));
  drain(pane);
}
async function drain(pane) {
  const s = queues[pane];
  if (s.sending) return;
  s.sending = true;
  while (s.items.length) {
    const batch = s.items.splice(0).join("\n") + "\n";
    try { await fetch("/input?pane=" + pane, {method:"POST", body: batch}); } catch {}
  }
  s.sending = false;
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
  if (data && mode === "focus") send(selected, {t:"input", b: data});
});
function parkFocus() {
  requestAnimationFrame(() => {
    const sel = getSelection();
    if (sel && !sel.isCollapsed) return;
    try { ta.focus(); } catch {}
  });
}

let mode = "navigate";
let selected = sessionStorage.getItem("panes.selected") || firstPane();
let actionsOwned = false;
let actionsTimer = null;

function firstPane() {
  return document.querySelector(".pane")?.dataset.pane || "";
}
function paneEls() {
  return [...document.querySelectorAll(".pane")];
}
function columns() {
  return [...document.querySelectorAll(".column")];
}
function paneOf(id) {
  return document.querySelector(`.pane[data-pane="${id}"]`);
}

function setMode(next) {
  mode = next;
  document.body.classList.toggle("mode-focus", mode === "focus");
  document.getElementById("mode-badge").textContent = mode;
}
function setSelected(id, {focus = false, scroll = true} = {}) {
  selected = id || "";
  try { sessionStorage.setItem("panes.selected", selected); } catch {}
  paneEls().forEach(p => p.classList.toggle("selected", p.dataset.pane === selected));
  if (focus) setMode("focus");
  if (scroll && selected) reveal(selected);
}
function syncEmpty() {
  document.getElementById("empty").hidden = !!document.querySelector(".column");
}

// One probe for the whole strip: every grid shares the same font metrics.
// It carries the grid's own line-height so a measured row matches a rendered
// one exactly -- measuring inside .scroll picked up the chrome line-height.
function measureCell() {
  const probe = document.createElement("div");
  probe.className = "cell-probe";
  probe.textContent = "M".repeat(80);
  document.body.appendChild(probe);
  const r = probe.getBoundingClientRect();
  probe.remove();
  return {w: r.width / 80, h: r.height};
}

const lastFit = {};
function fit() {
  if (NOFIT) return;
  const cell = measureCell();
  if (!cell.w || !cell.h) return;
  for (const p of paneEls()) {
    const name = p.dataset.pane;
    const box = p.querySelector(".scroll");
    if (!box) continue;
    const cols = Math.max(20, Math.floor((box.clientWidth - 16) / cell.w));
    const rows = Math.max(5, Math.floor((box.clientHeight - 16) / cell.h));
    const prev = lastFit[name];
    if (!prev || prev.cols !== cols || prev.rows !== rows) {
      lastFit[name] = {cols, rows};
      send(name, {t:"resize", cols, rows});
    }
  }
}
let fitTimer;
function scheduleFit() {
  clearTimeout(fitTimer);
  fitTimer = setTimeout(fit, 80);
}

// Auto-stick to the bottom, per pane. `stick` means "pinned to the live
// prompt". It must flip only on a genuine user scroll, and the trap is that
// rebuilding a grid also fires scroll events: a morph clamping scrollTop, and
// our own stick write, both look like scrolls. Reading those as user intent is
// what unpinned a pane mid-stream. The `mutating` window swallows every scroll
// event a patch triggers, so stick survives rebuilds.
//
// The old rule -- keep the cursor visible -- is not the same thing. A TUI that
// draws rows below its cursor (the Claude Code CLI input box, with its hint
// line under it) satisfies "cursor in view" while its last rows sit below the
// fold, so the pane stopped tracking.
const stick = {};
let mutating = false;
let mutatingClear = null;
function beginMutating() {
  mutating = true;
  clearTimeout(mutatingClear);
  // Clear on the next task. A mutation's scroll events fire in the rendering
  // step before then, so they land inside the window; a later user scroll does
  // not.
  mutatingClear = setTimeout(() => { mutating = false; }, 0);
}
function scrollBox(name) {
  return document.querySelector(`.pane[data-pane="${name}"] .scroll`);
}
function atBottom(box) {
  return box.scrollHeight - box.scrollTop - box.clientHeight < 8;
}
function stickToBottom(name) {
  const box = scrollBox(name);
  if (!box) return;
  if (box.scrollTop !== box.scrollHeight - box.clientHeight) {
    box.scrollTop = box.scrollHeight;
  }
}
function reveal(id) {
  paneOf(id)?.closest(".column")?.scrollIntoView({
    inline: "center",
    block: "nearest",
    behavior: "smooth",
  });
}
function wirePane(p) {
  const name = p.dataset.pane;
  if (!name || p.dataset.wired) return;
  p.dataset.wired = "1";
  stick[name] = true;
  p.querySelector(".scroll")?.addEventListener("scroll", ev => {
    if (mutating) return;
    stick[name] = atBottom(ev.currentTarget);
  });
}
function wireAll() {
  paneEls().forEach(wirePane);
  syncEmpty();
  if (!selected || !paneOf(selected)) setSelected(firstPane(), {scroll: false});
  else setSelected(selected, {scroll: false});
  scheduleFit();
}

const strip = document.getElementById("strip");
strip.addEventListener("mousedown", e => {
  const p = e.target.closest(".pane");
  if (!p) return;
  setSelected(p.dataset.pane, {focus: true});
});
strip.addEventListener("click", e => {
  if (e.target.closest(".pane")) parkFocus();
});
document.addEventListener("click", e => {
  if (e.target.closest(".pane") || e.target.closest("#status") || e.target.closest(".actions-panel")) return;
  if (mode === "focus") setMode("navigate");
});

document.getElementById("mode-badge").addEventListener("click", () => {
  setMode(mode === "focus" ? "navigate" : "focus");
  if (mode === "focus" && selected) parkFocus();
});

// Every pane tracks its own output, not just the selected one. Datastar morphs
// a patch in synchronously, so one observer batch is one server frame; the
// panes it touched are the ones to re-pin.
new MutationObserver(muts => {
  beginMutating();
  wireAll();
  const touched = new Set();
  for (const m of muts) {
    const el = m.target.nodeType === 1 ? m.target : m.target.parentElement;
    const name = el?.closest?.(".pane")?.dataset.pane;
    if (name) touched.add(name);
  }
  for (const name of touched) {
    if (stick[name] !== false) stickToBottom(name);
  }
}).observe(document.body, {childList: true, subtree: true, characterData: true});
addEventListener("resize", scheduleFit);
document.fonts.ready.then(fit);

document.addEventListener("copy", ev => {
  const sel = getSelection(); if (!sel) return;
  const text = sel.toString(); if (!text) return;
  const trimmed = text.split("\n").map(l => l.replace(/[ \t]+$/,"")).join("\n");
  if (trimmed === text) return;
  ev.clipboardData?.setData("text/plain", trimmed);
  ev.preventDefault();
});
addEventListener("paste", ev => {
  if (mode !== "focus") return;
  const text = ev.clipboardData?.getData("text");
  if (!text) return;
  ev.preventDefault();
  send(selected, {t:"paste", s:text});
});

async function postPane(path, params) {
  const q = new URLSearchParams(params);
  const r = await fetch(path + "?" + q.toString(), {method: "POST"});
  if (!r.ok) return null;
  try { return await r.json(); } catch { return null; }
}

const acts = {
  async "new-column"() {
    const out = await postPane("/pane/new-column", selected ? {after: selected} : {});
    if (out?.id) setSelected(out.id, {focus: mode === "focus"});
  },
  async split() {
    if (!selected) return acts["new-column"]();
    const out = await postPane("/pane/split", {pane: selected});
    if (out?.id) setSelected(out.id, {focus: mode === "focus"});
  },
  async close() {
    if (!selected) return;
    const out = await postPane("/pane/close", {pane: selected});
    setSelected(out?.next || firstPane(), {focus: mode === "focus" && !!(out?.next || firstPane())});
    if (!firstPane()) setMode("navigate");
  },
  "col-prev"() { moveCol(-1); },
  "col-next"() { moveCol(1); },
  "pane-prev"() { moveInCol(-1); },
  "pane-next"() { moveInCol(1); },
};

function moveCol(dir) {
  const cols = columns();
  if (!cols.length) return;
  const cur = paneOf(selected)?.closest(".column");
  let i = cols.indexOf(cur);
  if (i < 0) i = 0;
  const n = (i + dir + cols.length) % cols.length;
  const id = cols[n].querySelector(".pane")?.dataset.pane;
  if (id) setSelected(id);
}
function moveInCol(dir) {
  const col = paneOf(selected)?.closest(".column") || columns()[0];
  if (!col) return;
  const panes = [...col.querySelectorAll(".pane")];
  if (!panes.length) return;
  let i = panes.findIndex(p => p.dataset.pane === selected);
  if (i < 0) i = 0;
  const n = (i + dir + panes.length) % panes.length;
  setSelected(panes[n].dataset.pane);
}

function comboKey(e) {
  const parts = [];
  if (e.metaKey) parts.push("cmd");
  if (e.ctrlKey) parts.push("ctrl");
  if (e.altKey) parts.push("alt");
  if (e.shiftKey) parts.push("shift");
  let k = e.key;
  if (k.length === 1 && /[a-zA-Z]/.test(k)) k = k.toLowerCase();
  parts.push(k.toLowerCase());
  return parts.join("+");
}

const backdrop = document.getElementById("actions-backdrop");
function preselectActionsRow() {
  const rows = [...document.querySelectorAll(".actions-panel .picker-row")];
  rows.forEach((r, i) => r.classList.toggle("sel", i === 0));
}
function openActionsOwn() {
  actionsOwned = true;
  if (actionsTimer === null) {
    actionsTimer = setTimeout(() => {
      actionsTimer = null;
      if (actionsOwned) {
        backdrop.hidden = false;
        preselectActionsRow();
      }
    }, ACTIONS_HINT_MS);
  }
}
function endActions() {
  actionsOwned = false;
  if (actionsTimer !== null) clearTimeout(actionsTimer);
  actionsTimer = null;
  backdrop.hidden = true;
}
function runActionKey(key) {
  const row = document.querySelector(`.actions-panel [data-key="${key}"]`);
  if (!row) return false;
  endActions();
  row.click();
  return true;
}
function moveActionsSel(dir) {
  const rows = [...document.querySelectorAll(".actions-panel .picker-row")];
  if (!rows.length) return;
  const i = rows.findIndex(r => r.classList.contains("sel"));
  const n = ((i < 0 ? 0 : i) + dir + rows.length) % rows.length;
  rows.forEach(r => r.classList.remove("sel"));
  rows[n].classList.add("sel");
  rows[n].scrollIntoView({block: "nearest"});
}

document.querySelectorAll(".actions-panel .picker-row").forEach(row => {
  row.addEventListener("click", () => {
    const act = acts[row.dataset.act];
    endActions();
    act?.();
  });
});
backdrop.addEventListener("click", e => {
  if (e.target === backdrop) endActions();
});

document.addEventListener("keydown", ev => {
  if (ev.isComposing || ev.keyCode === 229 || composing) return;
  const combo = comboKey(ev);

  if (actionsOwned) {
    const k = ev.key;
    if (["Shift","Control","Alt","Meta","CapsLock"].includes(k)) return;
    ev.preventDefault();
    ev.stopPropagation();
    if (combo === "cmd+k" || combo === "ctrl+k") { endActions(); return; }
    if (k === "ArrowDown" || (ev.ctrlKey && k === "n")) moveActionsSel(1);
    else if (k === "ArrowUp" || (ev.ctrlKey && k === "p")) moveActionsSel(-1);
    else if (k === "Enter") {
      const sel = document.querySelector(".actions-panel .picker-row.sel");
      endActions();
      (sel || document.querySelector(".actions-panel .picker-row"))?.click();
    } else if (k === "Escape") {
      endActions();
    } else {
      runActionKey(k);
    }
    return;
  }

  if (combo === "cmd+enter" || combo === "ctrl+enter") {
    ev.preventDefault();
    ev.stopPropagation();
    if (!selected) return;
    setMode(mode === "focus" ? "navigate" : "focus");
    if (mode === "focus") parkFocus();
    return;
  }
  if (combo === "cmd+k" || (combo === "ctrl+k" && mode === "navigate")) {
    ev.preventDefault();
    ev.stopPropagation();
    openActionsOwn();
    return;
  }

  if (mode === "focus") {
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
    send(selected, frame);
    return;
  }

  if (ev.key === "Enter" && !ev.metaKey && !ev.ctrlKey && !ev.altKey) {
    if (!selected) return;
    ev.preventDefault();
    ev.stopPropagation();
    setMode("focus");
    parkFocus();
    return;
  }
  if (ev.ctrlKey || ev.altKey || ev.metaKey || ev.key === "x") return;
  const row = document.querySelector(`.actions-panel [data-key="${ev.key}"]`);
  if (!row) return;
  ev.preventDefault();
  ev.stopPropagation();
  row.click();
}, {capture: true});

wireAll();
setMode("navigate");
parkFocus();
