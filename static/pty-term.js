// <pty-term pane="name">: one ptyZZZ terminal pane in the browser.
//
// The element renders the grid box that the page's /sse patches land in
// (by element id, grid-<pane>), and owns everything else one pane needs:
// keys and composed text, paste, the send queue to /input, fitting the pty
// to its box, and pinning the view to the live output. The page owns the
// stream and the layout. Keys reach a pane through the browser's focus: the
// pane listens on itself, so whichever pane holds focus gets what is typed.
//
// Props:
//   pane   the pane name; the server's /input?pane= and grid-<pane> id
//   nofit  observe without resizing the pty (screenshots, a second viewer)

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

// Rows are space-padded to full width; strip trailing whitespace per line so
// a copy from any pane matches what a terminal emulator would yield. One
// listener for the page, registered when the module loads.
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

rocket("pty-term", {
  mode: "light",
  props: ({ string, bool }) => ({
    pane: string.trim,
    nofit: bool,
  }),
  // The grid's contents arrive from the page's stream; a rerender would
  // morph them away.
  renderOnPropChange: false,
  // The composition surface (lifted from stacks2099 key-buffer.js): dead
  // keys and IME need the OS composing into a real editable element, or an
  // accent collapses to its base letter. Offscreen via opacity, not
  // display:none, which cannot hold focus.
  render: ({ html, props: { pane } }) => html`
    <div class="scroll" data-ref:box><div id="grid-${pane}">connecting...</div></div>
    <textarea class="compose" data-ref:compose aria-hidden="true" tabindex="-1"
      autocapitalize="off" autocomplete="off" spellcheck="false"></textarea>
  `,
  onFirstRender: ({ host, refs, props, cleanup, defineHostProp }) => {
    const box = refs.box;
    const ta = refs.compose;

    // Awaited send queue: at most one POST in flight, so frames reach the
    // server in press order by construction. While one is in flight the
    // backlog accumulates and drains as a single NDJSON batch, capping the
    // added delay at one round trip.
    const queue = [];
    let sending = false;
    const send = (frame) => {
      queue.push(JSON.stringify(frame));
      drain();
    };
    const drain = async () => {
      if (sending) return;
      sending = true;
      while (queue.length) {
        const batch = queue.splice(0).join("\n") + "\n";
        try {
          await fetch("/input?pane=" + encodeURIComponent(props.pane), { method: "POST", body: batch });
        } catch {}
      }
      sending = false;
    };

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
    // finished selection is visible to park's guard. A plain click must
    // still park, or a dead key (null in keyEvent) would not refocus the
    // surface before composing.
    host.addEventListener("click", park);

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
      // A key we forward means the user is typing, not selecting: re-park
      // focus so the next dead key composes, and drop any stray char the
      // keypress may have placed in the surface.
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

    // Fit the pty to the box: measure one rendered line from a probe row
    // (it inherits the pane font and the --cell-h row pitch), derive
    // cols/rows from the box, and send {t:resize} when they change. Every
    // open tab does this, so the last viewer to resize wins.
    let lastFit = null;
    const fit = () => {
      const probe = document.createElement("div");
      probe.textContent = "M".repeat(40);
      probe.style.cssText = "position:absolute;visibility:hidden;white-space:pre";
      box.appendChild(probe);
      const r = probe.getBoundingClientRect();
      box.removeChild(probe);
      if (r.width === 0 || r.height === 0) return;
      const cols = Math.max(20, Math.floor((box.clientWidth - 16) / (r.width / 40)));
      const rows = Math.max(5, Math.floor((box.clientHeight - 16) / r.height));
      if (!lastFit || lastFit.cols !== cols || lastFit.rows !== rows) {
        lastFit = { cols, rows };
        send({ t: "resize", cols, rows });
      }
    };
    if (!props.nofit) {
      let fitTimer;
      const resized = new ResizeObserver(() => {
        clearTimeout(fitTimer);
        fitTimer = setTimeout(fit, 200);
      });
      resized.observe(box);
      cleanup(() => { resized.disconnect(); clearTimeout(fitTimer); });
      fit();
      // font-display:swap means the first fit may have measured the
      // fallback font; re-measure once the real face is loaded.
      document.fonts.ready.then(fit);
    }

    // Pin the view to the live output (from examples/panes, "Auto-stick to
    // the bottom"). Every scroll is either ours or the user's: if scrollTop
    // still reads what we last wrote, it was ours. Scrolling away unpins;
    // scrolling back to the bottom pins again. The target is the last
    // non-blank row or the cursor, whichever is lower, not the end of the
    // scroll range: after a resize the grid can end in blank rows.
    let stick = true;
    let lastWrote = -1;
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
    const toBottom = () => {
      const top = bottomTarget();
      if (box.scrollTop !== top) box.scrollTop = top;
      lastWrote = box.scrollTop;
    };
    box.addEventListener("scroll", () => {
      if (box.scrollTop === lastWrote) return;
      stick = box.scrollTop >= bottomTarget() - 8;
    }, { passive: true });
    const output = new MutationObserver(() => { if (stick) toBottom(); });
    output.observe(box, { childList: true, subtree: true, characterData: true, attributes: true });
    cleanup(() => output.disconnect());
  },
});
