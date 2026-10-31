// The layout: columns of panes, selection, zoom, modes and the actions panel.
// Everything inside one pane (keys, paste, fit, pinning to live output) is
// ptyZZZ's <pty-term> element, static/pty-term.js at the repo root.
const ACTIONS_HINT_MS = 150;

function termOf(id) {
  return document.querySelector(`pty-term[pane="${id}"]`);
}
// Typing goes to whichever pane holds focus, so focus mode parks it on the
// selected pane and navigate mode takes it away from every pane.
function parkFocus() {
  if (mode === "focus" && selected) termOf(selected)?.park?.();
}
function leavePane() {
  if (document.activeElement?.closest("pty-term")) document.activeElement.blur();
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
  if (mode === "navigate") leavePane();
}
function setSelected(id, {focus = false, scroll = true} = {}) {
  selected = id || "";
  try { sessionStorage.setItem("panes.selected", selected); } catch {}
  paneEls().forEach(p => p.classList.toggle("selected", p.dataset.pane === selected));
  if (focus) { setMode("focus"); parkFocus(); }
  if (!selected) { setZoom(false); return; }
  // scroll:false is wireAll re-asserting the current selection on every frame,
  // not a move. Zoom rides the .selected class, so moving the selection moves
  // the zoom; the pane arriving refits itself when its box gets a size.
  if (!scroll || zoom) return;
  reveal(selected);
}
function syncEmpty() {
  document.getElementById("empty").hidden = !!document.querySelector(".column");
}

function reveal(id) {
  paneOf(id)?.closest(".column")?.scrollIntoView({
    inline: "center",
    block: "nearest",
    behavior: "smooth",
  });
}
// Zoom is per viewer, like `selected`: it changes no layout and must not
// outlive a reload of the strip, so it stays out of the store. Each pane
// refits and re-pins itself when zoom changes its box.
function setZoom(on) {
  zoom = !!on && !!selected;
  try { sessionStorage.setItem("panes.zoom", zoom ? "1" : ""); } catch {}
  document.body.classList.toggle("zoom", zoom);
  if (!zoom && selected) requestAnimationFrame(() => reveal(selected));
}
function wireAll() {
  syncEmpty();
  if (!selected || !paneOf(selected)) setSelected(firstPane(), {scroll: false});
  else setSelected(selected, {scroll: false});
}

const strip = document.getElementById("strip");
strip.addEventListener("mousedown", e => {
  if (e.target.closest(".follow")) return;
  const p = e.target.closest(".pane");
  if (!p) return;
  // Select and enter focus mode, but do not park focus yet: focusing the
  // compose textarea on mousedown kills a drag selection before it starts.
  // The click handlers park after mouseup, when a finished selection is
  // visible to the guard in park().
  setSelected(p.dataset.pane);
  setMode("focus");
});
strip.addEventListener("click", e => {
  const btn = e.target.closest(".follow");
  if (btn) {
    btn.closest(".pane")?.querySelector("pty-term")?.follow?.();
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

// Columns and panes arrive and leave as server patches: keep the selection
// and the empty hint in step with what is on the page.
new MutationObserver(wireAll).observe(document.getElementById("strip"), {childList: true, subtree: true});

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
  if (ev.isComposing || ev.keyCode === 229) return;
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

  // In focus mode every other key belongs to the selected pane, which holds
  // DOM focus and handles it itself.
  if (mode === "focus") return;

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
