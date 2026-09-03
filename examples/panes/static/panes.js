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
let zoom = sessionStorage.getItem("panes.zoom") === "1";
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
  if (!selected) { setZoom(false); return; }
  // scroll:false is wireAll re-asserting the current selection on every frame,
  // not a move. Only a deliberate move refits.
  if (!scroll) return;
  // Zoom rides the .selected class, so moving the selection moves the zoom.
  // The pane arriving was hidden: it needs a fit for its new size and a re-pin.
  if (zoom) { fitNow(); restick(); }
  else reveal(selected);
}
function syncEmpty() {
  document.getElementById("empty").hidden = !!document.querySelector(".column");
}

// One probe for the whole strip: every grid shares the same font metrics.
// It carries the grid's own line-height so a measured row matches a rendered
// one exactly -- measuring inside .scroll picked up the chrome line-height.
// Cached, because the probe is a write into the tree the MutationObserver
// watches: an uncached fit() mutates, the observer sees it and schedules
// another fit, and the pair free-runs at the debounce interval. Metrics only
// change when the font or the device pixel ratio does, so both invalidate it.
let cellCache = null;
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

const lastFit = {};
function fit() {
  if (NOFIT) return;
  const cell = measureCell();
  if (!cell.w || !cell.h) return;
  for (const p of paneEls()) {
    const name = p.dataset.pane;
    const box = p.querySelector(".scroll");
    if (!box) continue;
    // A zoomed-away pane measures zero. Without this the floors below would
    // resize its pty to 20x5 and reflow its scrollback out of sight.
    if (!box.clientWidth || !box.clientHeight) continue;
    // Height comes from the pane, not from the box: we set the box's own
    // height below, so reading it back would freeze the row count. The label
    // sits above .scroll, so subtract the siblings that precede it.
    let head = 0;
    for (let s = p.firstElementChild; s && s !== box; s = s.nextElementSibling) {
      head += s.offsetHeight || 0;
    }
    const cols = Math.max(20, Math.floor((box.clientWidth - 16) / cell.w));
    const rows = Math.max(5, Math.floor((p.clientHeight - head) / cell.h));
    // Pin the box to a whole number of rows. The sub-row remainder sits below
    // it showing the pane background, instead of clipping a half row at the
    // top once the pane is pinned to the bottom.
    const h = rows * cell.h + "px";
    if (box.style.height !== h) {
      box.style.flex = "none";
      box.style.height = h;
      // Shrinking the box leaves scrollTop short of the new bottom.
      if (stick[name] !== false) stickToBottom(name);
    }
    const prev = lastFit[name];
    if (!prev || prev.cols !== cols || prev.rows !== rows) {
      lastFit[name] = {cols, rows};
      send(name, {t:"resize", cols, rows});
    }
  }
}
let fitTimer;
// Window drags fire continuously, and every event that crosses a cell boundary
// would cost a resize frame and a forced keyframe, so they coalesce.
function scheduleFit() {
  clearTimeout(fitTimer);
  fitTimer = setTimeout(fit, 80);
}
// A discrete toggle has one known final geometry. There is nothing to coalesce
// and the 80ms is pure latency: it was the whole gap between the pane resizing
// and the pty catching up (80ms of a measured 96ms).
function fitNow() {
  clearTimeout(fitTimer);
  fit();
}

// Auto-stick to the bottom, per pane. `stick` means "pinned to the live
// prompt".
//
// The rule is: never infer intent from a `scroll` event. A scroll event cannot
// say who caused it, so reading one meant reconstructing the answer -- from a
// mutation window, from a remembered write value -- and every new way to
// scroll needed another guard on the pile. Listen instead for the gestures
// that are unambiguously the user, and let `scrollend` say when one is over,
// momentum included.
//
// While a gesture is live the pin yields completely. A patch arriving mid-drag
// must not write scrollTop: in the overscroll rubber band that rips the
// content back out from under the thumb.
//
// A hold always resolves, either on `scrollend` or on its own timer, so a
// browser without `scrollend` (Safari before 26.2) still yields for the
// gesture and re-pins after it, just without the eased settle.
//
// The old rule -- keep the cursor visible -- is not the same thing. A TUI that
// draws rows below its cursor (the Claude Code CLI input box, with its hint
// line under it) satisfies "cursor in view" while its last rows sit below the
// fold, so the pane stopped tracking.
const stick = {};
// How long a gesture owns the scroll when no `scrollend` arrives to close it.
// Long enough to cover touch momentum, short enough that a hold left behind on
// a browser without `scrollend` heals well inside a reading pause.
const HOLD_MS = 700;
const hold = {};
const holdTimer = {};
function held(name) {
  return !!hold[name];
}
function beginHold(name) {
  hold[name] = 1;
  clearTimeout(holdTimer[name]);
  holdTimer[name] = setTimeout(() => resolveHold(name), HOLD_MS);
}
// Every hold ends here, whether `scrollend` closed it or the timer did. It has
// to re-assert rather than just lapse: a hold that only expired would leave a
// pane that still believes it is stuck sitting wherever the gesture left it,
// with no frame guaranteed to arrive and put it right.
function resolveHold(name) {
  if (!hold[name]) return;
  hold[name] = 0;
  clearTimeout(holdTimer[name]);
  const box = scrollBox(name);
  if (!box) return;
  setStick(name, atBottom(box));
  // Only ease back if the release left live content below the fold. Past
  // bottomTarget the user is in the grid's blank tail, seeing everything there
  // is to see, and that is where the browser itself comes to rest -- dragging
  // them back up two rows from a place they chose is a fight, not a settle.
  // The next frame re-pins them anyway, so the offset only survives while
  // nothing is happening, which is exactly when it is worth respecting.
  if (stick[name] !== false && box.scrollTop < bottomTarget(box)) settleToBottom(name);
}
function scrollBox(name) {
  return document.querySelector(`.pane[data-pane="${name}"] .scroll`);
}
// Single writer for stick, so the pane's class always agrees with it and the
// follow button can key off one thing.
function setStick(name, on) {
  stick[name] = on;
  paneOf(name)?.classList.toggle("unstuck", on === false);
}
// The scrollTop that puts the live output at the bottom of the view. Not the end
// of the scroll range: a resize grows the pty's viewport before the app has drawn
// into the new rows, so the grid legitimately ends in blank lines and the range
// ends below anything worth looking at. The cursor counts as content, so a TUI
// that parks it under its last text still keeps it in view.
function bottomTarget(box) {
  const boxTop = box.getBoundingClientRect().top;
  const bottomOf = el => el.getBoundingClientRect().bottom - boxTop + box.scrollTop;
  let target = 0;
  const rows = box.querySelectorAll(".row");
  // Blank rows cluster at the end, so this scan almost always stops immediately.
  for (let i = rows.length - 1; i >= 0; i--) {
    if (rows[i].textContent.trim() !== "") { target = bottomOf(rows[i]); break; }
  }
  const cur = box.querySelector(".cursor");
  if (cur) target = Math.max(target, bottomOf(cur));
  const max = box.scrollHeight - box.clientHeight;
  return target ? Math.max(0, Math.min(Math.ceil(target) - box.clientHeight, max)) : max;
}
// Shares bottomTarget with stickToBottom on purpose: if these two disagreed,
// every re-pin would land somewhere atBottom calls "scrolled away" and unstick
// the pane it had just pinned.
function atBottom(box) {
  return box.scrollTop >= bottomTarget(box) - 8;
}
// Instant on purpose. This runs once per arriving line, and a smooth scroll
// animates over ~300ms: at 16ms between frames each write would abort the last
// animation and restart it, so the view would trail the output permanently.
// Yields to a live gesture -- see `hold`.
function stickToBottom(name) {
  if (held(name)) return;
  const box = scrollBox(name);
  if (!box) return;
  const top = bottomTarget(box);
  if (box.scrollTop !== top) box.scrollTop = top;
}
// The one bottom move that is animated: a single hop to a known place, once the
// user has let go or asked to follow again. Same reasoning as `reveal`.
function settleToBottom(name) {
  const box = scrollBox(name);
  if (!box) return;
  box.scrollTo({top: bottomTarget(box), behavior: "smooth"});
}
function reveal(id) {
  paneOf(id)?.closest(".column")?.scrollIntoView({
    inline: "center",
    block: "nearest",
    behavior: "smooth",
  });
}
// Zoom is per viewer, like `selected`: it changes no layout and must not
// outlive a reload of the strip, so it stays out of the store.
function setZoom(on) {
  zoom = !!on && !!selected;
  try { sessionStorage.setItem("panes.zoom", zoom ? "1" : ""); } catch {}
  document.body.classList.toggle("zoom", zoom);
  fitNow();
  restick();
}
// A hidden pane has no scrollHeight, so stickToBottom cannot run on it while
// it is zoomed away. Re-pin every stuck pane once the new layout has settled.
function restick() {
  requestAnimationFrame(() => {
    for (const p of paneEls()) {
      const name = p.dataset.pane;
      if (stick[name] !== false) stickToBottom(name);
    }
    if (!zoom && selected) reveal(selected);
  });
}
function wirePane(p) {
  const name = p.dataset.pane;
  if (!name || p.dataset.wired) return;
  p.dataset.wired = "1";
  setStick(name, true);
  const box = p.querySelector(".scroll");
  if (!box) return;
  // Intent in. Only the user produces these, so they need no guard.
  for (const kind of ["wheel", "touchstart"]) {
    box.addEventListener(kind, () => beginHold(name), {passive: true});
  }
  // A scrollbar grab, not a click in the text: clicking a pane to focus it must
  // not stop it tracking. The scrollbar sits past clientWidth, so this misses
  // overlay scrollbars, which keep the behavior they already had.
  box.addEventListener("pointerdown", ev => {
    if (ev.offsetX > box.clientWidth) beginHold(name);
  }, {passive: true});
  // A drag longer than HOLD_MS would otherwise expire mid-gesture and let the
  // pin start fighting the thumb again. This only extends a hold that is
  // already live; it decides nothing, and it is inert when nothing is held, so
  // our own writes cannot feed it.
  box.addEventListener("scroll", () => {
    if (held(name)) beginHold(name);
  }, {passive: true});
  // Intent out. `scrollend` fires for programmatic scrolls too, so it resolves
  // nothing unless a gesture was holding. Without that check, a frame landing
  // between our write and the event would read as "scrolled away" and unpin a
  // pane that is tracking perfectly.
  box.addEventListener("scrollend", () => resolveHold(name));
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
  if (e.target.closest(".follow")) return;
  const p = e.target.closest(".pane");
  if (!p) return;
  setSelected(p.dataset.pane, {focus: true});
});
strip.addEventListener("click", e => {
  const follow = e.target.closest(".follow");
  if (follow) {
    const name = follow.closest(".pane")?.dataset.pane;
    // A deliberate "catch me up": drop any hold the click left and ease back,
    // rather than snapping the pane out from under the pointer.
    if (name) { hold[name] = 0; setStick(name, true); settleToBottom(name); }
    e.stopPropagation();
    return;
  }
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
  zoom() { setZoom(!zoom); },
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
setZoom(zoom);
setMode("navigate");
parkFocus();
