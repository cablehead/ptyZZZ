// <pty-term pane="name">: one ptyZZZ terminal pane in the browser.
//
// The server renders the element with its grid inside, the box the page's
// /sse patches land in by element id:
//
//   <pty-term pane="a"><div class="scroll"><div id="grid-a">...</div></div></pty-term>
//
// A page that morphs the element's surroundings (Datastar skips patches
// under data-ignore-morph, so it cannot fence the terminal off) renders the
// current screen into the grid, gives the parts ids, keeps the element's own
// attributes with data-preserve-attr, and renders the composition box:
//
//   <pty-term id="pty-a" pane="a" data-preserve-attr="class data-scope-children data-rocket-host">
//     <div class="scroll" id="scroll-a" data-preserve-attr="style"><div id="grid-a">...</div></div>
//     <textarea class="compose" id="compose-a" aria-hidden="true" tabindex="-1"></textarea>
//   </pty-term>
//
// The element owns everything else one pane needs: keys and composed text,
// paste, mouse and focus reports, the send queue to /input, fitting the pty
// to its box, and pinning the view to the live output. The page owns the stream and the layout. Keys
// reach a pane through the browser's focus: the pane listens on itself, so
// whichever pane holds focus gets what is typed.
//
// Props:
//   pane   the pane name; the server's /input?pane= and grid-<pane> id
//   nofit  observe without resizing the pty (screenshots, a second viewer);
//          a page URL with ?nofit sets it for every pane
//
// Host API:
//   park()    put focus on this pane, so typed keys go to its pty
//   follow()  scroll to the live output and pin to it again
//   The host carries the class `unstuck` while the user has scrolled away.
//
// Styles: static/pty-term.css.

import { rocket } from "/static/vendor/datastar-rocket@1.0.4.js";

// The client is byte-blind: it ships semantic key events and the emulator
// encodes them against its live input modes (application cursor keys,
// bracketed paste, ...). See PROTOCOL.md {t:key}/{t:paste}.
const NAMED = ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End",
  "PageUp", "PageDown", "Insert", "Delete", "Enter", "Tab", "Backspace", "Escape"];

function keyEvent(ev) {
  if (["Shift", "Control", "Alt", "Meta", "CapsLock"].includes(ev.key)) return null;
  const mods = (ev.shiftKey ? 1 : 0) | (ev.altKey ? 2 : 0) | (ev.ctrlKey ? 4 : 0) | (ev.metaKey ? 8 : 0);
  if (ev.key.length === 1) {
    // Cmd+char belongs to the browser/OS (copy, reload, ...); Cmd with the
    // named keys below is terminal input (Cmd+Left = CSI 1;9D).
    if (ev.metaKey) return null;
    // Option as compose (non-US layouts) delivers a finished glyph in
    // ev.key; drop the alt bit so it isn't re-encoded as a Meta chord.
    if (ev.altKey && !ev.ctrlKey) {
      const codeLetter = /^Key([A-Z])$/.exec(ev.code)?.[1]?.toLowerCase();
      if (!(codeLetter && ev.key.toLowerCase() === codeLetter)) {
        return { t: "key", key: ev.key, mods: mods & ~2 };
      }
    }
    return { t: "key", key: ev.key, mods };
  }
  if (NAMED.includes(ev.key) || /^F\d+$/.test(ev.key)) {
    return { t: "key", key: ev.key, mods };
  }
  return null;
}

// One cell size for the page: every grid shares the same font metrics. The
// probe goes inside a grid so it carries that grid's font and row pitch.
// Metrics only change with the font or the device pixel ratio, so a window
// resize and the real font arriving both clear it.
let cellCache = null;
function measureCell(grid) {
  if (cellCache) return cellCache;
  const probe = document.createElement("div");
  probe.textContent = "M".repeat(80);
  probe.style.cssText = "position:absolute;top:-9999px;left:-9999px;visibility:hidden;white-space:pre";
  grid.appendChild(probe);
  const r = probe.getBoundingClientRect();
  probe.remove();
  if (!r.width || !r.height) return null;
  cellCache = { w: r.width / 80, h: r.height };
  return cellCache;
}
addEventListener("resize", () => { cellCache = null; });

// Rows are space-padded to full width; strip trailing whitespace per line so
// a copy from any pane matches what a terminal emulator would yield.
document.addEventListener("copy", (ev) => {
  const sel = getSelection();
  if (!sel || !sel.anchorNode?.parentElement?.closest("pty-term")) return;
  const text = sel.toString();
  if (!text) return;
  const trimmed = text.split("\n").map((l) => l.replace(/[ \t]+$/, "")).join("\n");
  if (trimmed === text) return;
  ev.clipboardData?.setData("text/plain", trimmed);
  ev.preventDefault();
});

// Without scroll anchoring (Safari), an unpinned pane would jump as rows trim
// off the top; the element puts their height back by hand.
const ANCHORS = CSS.supports("overflow-anchor", "auto");

rocket("pty-term", {
  mode: "light",
  props: ({ string, bool }) => ({
    pane: string.trim,
    nofit: bool,
  }),
  setup: ({ host, props, cleanup, defineHostProp }) => {
    const box = host.querySelector(".scroll");
    if (!box) return;
    const grid = () => box.firstElementChild;
    const nofit = props.nofit || new URLSearchParams(location.search).has("nofit");

    // Awaited send queue: at most one POST in flight, so frames reach the
    // server in press order by construction. While one is in flight the
    // backlog accumulates and drains as a single NDJSON batch, capping the
    // added delay at one round trip.
    // Mouse moves coalesce: only the newest waits for the next drain, and it
    // joins the queue ahead of any later frame so presses keep their order.
    const queue = [];
    let sending = false;
    let pendingMove = null;
    const send = (frame) => {
      if (pendingMove) { queue.push(JSON.stringify(pendingMove)); pendingMove = null; }
      queue.push(JSON.stringify(frame));
      drain();
    };
    const sendMove = (frame) => {
      pendingMove = frame;
      drain();
    };
    const drain = async () => {
      if (sending) return;
      sending = true;
      while (queue.length || pendingMove) {
        if (pendingMove) { queue.push(JSON.stringify(pendingMove)); pendingMove = null; }
        const batch = queue.splice(0).join("\n") + "\n";
        try {
          await fetch("/input?pane=" + encodeURIComponent(props.pane), { method: "POST", body: batch });
        } catch {}
      }
      sending = false;
    };

    // The composition surface (lifted from stacks2099 key-buffer.js): dead
    // keys and IME need the OS composing into a real editable element, or an
    // accent collapses to its base letter. pty-term.css keeps it offscreen.
    // A page that morphs the element's surroundings renders the box itself
    // (`<textarea class="compose">`), so a morph keeps it; otherwise the
    // element adds one.
    let ta = host.querySelector(":scope > .compose");
    if (!ta) {
      ta = document.createElement("textarea");
      ta.className = "compose";
      ta.setAttribute("aria-hidden", "true");
      ta.tabIndex = -1;
      ta.autocapitalize = "off";
      ta.autocomplete = "off";
      ta.spellcheck = false;
      host.appendChild(ta);
      cleanup(() => ta.remove());
    }

    // Park DOM focus on the composition surface, deferred past layout, and
    // never while the user holds a text selection (focusing would collapse
    // it; typing re-parks via the keydown path instead).
    const park = () => {
      requestAnimationFrame(() => {
        const sel = getSelection();
        if (sel && !sel.isCollapsed) return;
        try { ta.focus(); } catch {}
      });
    };
    defineHostProp("park", { value: park });
    // Park on click, not mousedown: click fires after mouseup, so a drag's
    // finished selection is visible to the guard. A plain click must still
    // park, or a dead key (null in keyEvent) would not refocus the surface
    // before composing. At once, not on the next frame: keys typed right
    // after the click would otherwise land outside the pane and be lost.
    host.addEventListener("click", () => {
      const sel = getSelection();
      if (sel && !sel.isCollapsed) return;
      ta.focus();
    });

    let composing = false;
    ta.addEventListener("compositionstart", () => { composing = true; });
    ta.addEventListener("compositionend", (ev) => {
      composing = false;
      const data = ev.data || ta.value;
      ta.value = "";
      if (data) send({ t: "input", b: data });
    });

    host.addEventListener("keydown", (ev) => {
      // Composing keydowns (dead keys, IME) are provisional; skip them.
      // compositionend sends the finished string.
      if (ev.isComposing || ev.keyCode === 229 || composing) return;
      // Selection beats SIGINT: Ctrl+C with a selection copies.
      if (ev.ctrlKey && !ev.altKey && !ev.metaKey && ev.key === "c") {
        const sel = getSelection().toString();
        if (sel) { navigator.clipboard.writeText(sel).catch(() => {}); ev.preventDefault(); return; }
      }
      const frame = keyEvent(ev);
      if (frame === null) return;
      // A key we forward means the user is typing, not selecting: keep
      // focus on the surface so the next dead key composes, and drop any
      // stray char the keypress may have placed in it.
      if (document.activeElement !== ta) ta.focus();
      ev.preventDefault();
      ta.value = "";
      send(frame);
    });

    host.addEventListener("paste", (ev) => {
      const text = ev.clipboardData?.getData("text");
      if (!text) return;
      ev.preventDefault();
      send({ t: "paste", s: text });
    });

    // Focus reports: the emulator answers CSI I / CSI O only if the app
    // turned on ?1004h. Focus moving between parts of the pane is not a change.
    // A press on the grid drops focus to the body until the click handler
    // parks it again, so during a press the verdict waits for the click.
    let focused = null;
    let pressing = false;
    const sendFocus = (on) => {
      if (on === focused) return;
      focused = on;
      send({ t: "focus", on });
    };
    host.addEventListener("focusin", () => sendFocus(true));
    host.addEventListener("focusout", (ev) => {
      if (!pressing && !host.contains(ev.relatedTarget)) sendFocus(false);
    });
    host.addEventListener("pointerdown", () => { pressing = true; }, true);
    const settle = () => {
      if (!pressing) return;
      pressing = false;
      // After the click, which runs in the same task as the pointerup.
      setTimeout(() => sendFocus(document.hasFocus() && host.contains(document.activeElement)));
    };
    addEventListener("pointerup", settle, true);
    addEventListener("pointercancel", settle, true);
    cleanup(() => {
      removeEventListener("pointerup", settle, true);
      removeEventListener("pointercancel", settle, true);
    });

    // Mouse: the pane captures it only while the app has grabbed it (the
    // cursor's data-mouse-grabbed), and never with Shift held, which keeps
    // native text selection and scrollback. See PROTOCOL.md {t:mouse}.
    const grabbed = () => box.querySelector(".cursor")?.dataset.mouseGrabbed === "true";
    const mods = (ev) => (ev.shiftKey ? 1 : 0) | (ev.altKey ? 2 : 0) | (ev.ctrlKey ? 4 : 0) | (ev.metaKey ? 8 : 0);
    // The cell under the pointer, in visible-screen coordinates: the last
    // data-rows rows of the grid are the screen, the rest is scrollback.
    // Clamped to the screen, so a captured drag past the edge still lands.
    const cellAt = (ev) => {
      const g = grid();
      const cell = g && measureCell(g);
      if (!cell) return null;
      const cols = Number(g.dataset.cols) || 1;
      const rows = g.querySelectorAll(":scope > .row");
      const visible = Math.min(Number(g.dataset.rows) || 0, rows.length);
      if (!visible) return null;
      const top = rows[rows.length - visible].getBoundingClientRect();
      const h = top.height || cell.h;
      const fx = (ev.clientX - top.left) / cell.w;
      const fy = (ev.clientY - top.top) / h;
      const x = Math.max(0, Math.min(cols - 1, Math.floor(fx)));
      const y = Math.max(0, Math.min(visible - 1, Math.floor(fy)));
      return {
        x, y,
        x_pixel_offset: Math.max(0, Math.floor((fx - x) * cell.w)),
        y_pixel_offset: Math.max(0, Math.floor((fy - y) * h)),
      };
    };
    // Pointer button index -> protocol name, and its bit in ev.buttons.
    const BUTTONS = [["left", 1], ["middle", 4], ["right", 2]];
    let down = 0;         // ev.buttons bits we sent a press for
    let lastCell = null;  // "x,y" of the last frame sent; moves within it are dropped
    const press = (kind, b, ev) => {
      const p = cellAt(ev);
      if (!p) return;
      lastCell = p.x + "," + p.y;
      send({ t: "mouse", kind, button: BUTTONS[b][0], ...p, mods: mods(ev) });
    };
    const releaseAll = (ev) => {
      for (let b = 0; b < BUTTONS.length; b++) if (down & BUTTONS[b][1]) press("release", b, ev);
      down = 0;
    };
    box.addEventListener("pointerdown", (ev) => {
      if (!grabbed() || ev.shiftKey || !BUTTONS[ev.button]) return;
      // No native selection or focus shift; the click handler still parks.
      ev.preventDefault();
      box.setPointerCapture(ev.pointerId);
      down |= BUTTONS[ev.button][1];
      press("press", ev.button, ev);
    });
    box.addEventListener("pointermove", (ev) => {
      // A second button going down or up mid-drag arrives as a move with
      // ev.button set, not as its own pointerdown/pointerup.
      if (BUTTONS[ev.button]) {
        const bit = BUTTONS[ev.button][1];
        if (!(ev.buttons & bit) && down & bit) { down &= ~bit; press("release", ev.button, ev); }
        else if (ev.buttons & bit && !(down & bit) && down && !ev.shiftKey) { down |= bit; press("press", ev.button, ev); }
        return;
      }
      if (!down && (!grabbed() || ev.shiftKey)) return;
      const p = cellAt(ev);
      if (!p) return;
      const at = p.x + "," + p.y;
      if (at === lastCell) return;
      lastCell = at;
      sendMove({ t: "mouse", kind: "move", ...p, mods: mods(ev) });
    });
    // A release goes out for every press sent, grabbed or not by now, so
    // the app never sees a button stuck down.
    box.addEventListener("pointerup", (ev) => {
      const bit = BUTTONS[ev.button]?.[1];
      if (!bit || !(down & bit)) return;
      down &= ~bit;
      press("release", ev.button, ev);
    });
    box.addEventListener("pointercancel", releaseAll);
    box.addEventListener("contextmenu", (ev) => {
      if (grabbed() && !ev.shiftKey) ev.preventDefault();
    });
    // Wheel: one step per row (or column) of travel, the remainder kept for
    // the next event, so a trackpad's burst of small deltas scrolls by
    // distance, not by event count. deltaMode 1 counts lines, 2 pages.
    let wheelX = 0, wheelY = 0;
    box.addEventListener("wheel", (ev) => {
      if (!grabbed() || ev.shiftKey) return;
      ev.preventDefault();
      const g = grid();
      const cell = g && measureCell(g);
      if (!cell) return;
      const rows = Number(g.dataset.rows) || 1, cols = Number(g.dataset.cols) || 1;
      const [stepX, stepY] = ev.deltaMode === 1 ? [1, 1] : ev.deltaMode === 2 ? [1 / cols, 1 / rows] : [cell.w, cell.h];
      wheelX += ev.deltaX;
      wheelY += ev.deltaY;
      const p = cellAt(ev);
      if (!p) return;
      const step = (button) => send({ t: "mouse", kind: "press", button, ...p, mods: mods(ev) });
      for (; wheelY >= stepY; wheelY -= stepY) step("wheeldown");
      for (; wheelY <= -stepY; wheelY += stepY) step("wheelup");
      for (; wheelX >= stepX; wheelX -= stepX) step("wheelright");
      for (; wheelX <= -stepX; wheelX += stepX) step("wheelleft");
    }, { passive: false });

    // Pin the view to the live output. Every scroll is either ours or the
    // user's: if scrollTop still reads what we last wrote, it was ours.
    // Appending rows moves scrollHeight, never scrollTop, so growth cannot
    // blur that answer. Scrolling away unpins; scrolling back to the bottom
    // pins again.
    let stick = true;
    let lastWrote = -1;
    const setStick = (on) => {
      stick = on;
      host.classList.toggle("unstuck", !on);
    };
    // The scrollTop that puts the live output at the bottom of the view: the
    // last non-blank row or the cursor, whichever is lower. Not the end of
    // the scroll range: after a resize the grid can end in blank rows.
    const bottomTarget = () => {
      const boxTop = box.getBoundingClientRect().top;
      const bottomOf = (el) => el.getBoundingClientRect().bottom - boxTop + box.scrollTop;
      let target = 0;
      const rows = box.querySelectorAll(".row");
      for (let i = rows.length - 1; i >= 0; i--) {
        if (rows[i].textContent.trim() !== "") { target = bottomOf(rows[i]); break; }
      }
      const cursor = box.querySelector(".cursor");
      if (cursor) target = Math.max(target, bottomOf(cursor));
      const max = box.scrollHeight - box.clientHeight;
      return target ? Math.max(0, Math.min(Math.ceil(target) - box.clientHeight, max)) : max;
    };
    // The only writer of scrollTop, and it records what it wrote. Instant,
    // never smooth: a smooth scroll passes through positions nobody wrote.
    const toBottom = () => {
      const top = bottomTarget();
      if (box.scrollTop !== top) box.scrollTop = top;
      lastWrote = box.scrollTop;
    };
    defineHostProp("follow", { value: () => { setStick(true); toBottom(); } });
    box.addEventListener("scroll", () => {
      if (box.scrollTop === lastWrote) return;
      setStick(box.scrollTop >= bottomTarget() - 8);
    }, { passive: true });

    // Datastar morphs a patch in synchronously, so one observer batch is one
    // server frame. A pinned pane re-pins; an unpinned one without scroll
    // anchoring holds the line under the user's eye by adding back the
    // height of the rows a batch trimmed off the top. A row both removed and
    // re-added in one batch is a morph, not a trim.
    const output = new MutationObserver((muts) => {
      if (stick) { toBottom(); return; }
      if (ANCHORS || box.scrollTop === 0) return;
      const removed = new Set(), added = new Set();
      for (const m of muts) {
        if (m.type !== "childList") continue;
        for (const n of m.removedNodes) if (n.nodeType === 1 && n.classList.contains("row")) removed.add(n.id);
        for (const n of m.addedNodes) if (n.nodeType === 1 && n.classList.contains("row")) added.add(n.id);
      }
      let trimmed = 0;
      for (const id of removed) if (!added.has(id)) trimmed++;
      if (!trimmed) return;
      const h = box.querySelector(".row")?.offsetHeight || cellCache?.h || 0;
      box.scrollTop = Math.max(0, box.scrollTop - trimmed * h);
      lastWrote = box.scrollTop;
    });
    output.observe(box, { childList: true, subtree: true, characterData: true, attributes: true });
    cleanup(() => output.disconnect());

    // Fit the pty to the space the element is given: whole columns and rows
    // of the shared cell, minus the grid's own padding. The box is pinned to
    // a whole number of rows, so the remainder shows the pane background
    // instead of a half row clipped at the top once pinned to the bottom.
    // Every open tab fits, so the last viewer to resize wins.
    let lastFit = null;
    const fit = () => {
      const g = grid();
      if (!g || !host.clientWidth || !host.clientHeight) return; // hidden, e.g. zoomed away
      const cell = measureCell(g);
      if (!cell) return;
      const style = getComputedStyle(g);
      const padX = parseFloat(style.paddingLeft) + parseFloat(style.paddingRight);
      const padY = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
      const cols = Math.max(20, Math.floor((box.clientWidth - padX) / cell.w));
      const rows = Math.max(5, Math.floor((host.clientHeight - padY) / cell.h));
      const height = rows * cell.h + padY + "px";
      if (box.style.height !== height) {
        box.style.flex = "none";
        box.style.height = height;
      }
      // A new size leaves scrollTop short of the new bottom.
      if (stick) toBottom();
      if (!lastFit || lastFit.cols !== cols || lastFit.rows !== rows) {
        lastFit = { cols, rows };
        send({ t: "resize", cols, rows });
      }
    };
    if (!nofit) {
      // Leading and trailing: a one-off change (a split, a zoom) fits at
      // once; a window drag coalesces, since every cell boundary it crosses
      // would cost a resize frame and a forced keyframe.
      let fitTimer = null;
      const resized = new ResizeObserver(() => {
        if (fitTimer === null) fit();
        clearTimeout(fitTimer);
        fitTimer = setTimeout(() => { fitTimer = null; fit(); }, 80);
      });
      resized.observe(host);
      cleanup(() => { resized.disconnect(); clearTimeout(fitTimer); });
      // font-display:swap means the first fit may have measured the
      // fallback font; re-measure once the real face is loaded.
      document.fonts.ready.then(() => { cellCache = null; fit(); });
    }
  },
});
