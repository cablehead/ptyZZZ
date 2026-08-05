# ptyZZZ experiment: ptys owned by xs *services*, shown over one /sse.
#
# Run:
#   ~/http-nu/target/release/http-nu --dev --datastar --services --store ./store \
#     :5111 ~/ptyZZZ/serve.nu
#
# Panes: PTYZZZ_BINS is a comma-separated list of name=path pairs, one pty
# pane per entry, rendered side by side. Unset, it serves one pane backed by
# this repo's binary. Two engines head to head:
#   PTYZZZ_BINS="wezterm=/a/ptyZZZ,rio=/b/ptyZZZ" http-nu ... serve.nu
#
# Flow, per pane <name>:
#   POST /input?pane=<name> -> append `pty-<name>.send` -> service stdin
#   ptyZZZ stdout -> service closure fans JSONL into frames:
#     screen (keyframe) -> `pty-<name>.screen` (ttl last:1)    the join point
#     diff              -> `pty-<name>.diff`   (ttl ephemeral) changed rows
#
# Diffs carry their payload in frame meta ({body: ...}), not the CAS. An
# ephemeral frame is broadcast and never stored, so writing its content to the
# CAS would be a disk write with no reader benefit -- benched at ~30us/frame
# via CAS vs ~0.4us via meta, plus it saves a CAS read per subscriber per
# frame on /sse. Keyframes stay in the CAS: stored (last:1), large, rare.
#   GET  /sse     -> follow every pane's topics, patch by element id
#
# A joiner replays each pane's stored keyframe and applies live diffs on top;
# any missed diffs or delta bugs heal at the next keyframe. The client routes
# keystrokes to the focused pane (click to focus) and fits each pane's pty to
# its own box via {t:resize}.

use http-nu/datastar *
use http-nu/router *

const PTYZZZ = (path self | path dirname | path join "target" "release" "ptyZZZ")

let panes = (
  $env.PTYZZZ_BINS? | default $"local=($PTYZZZ)"
  | split row ","
  | each {|p|
      let kv = $p | split row "=" | each { str trim }
      {name: $kv.0, bin: $kv.1}
    }
)

# Fail at load with a clear message rather than registering a service whose
# spawn dies silently (the page would just sit at "connecting...").
for p in $panes {
  if not ($p.bin | path exists) {
    error make {msg: $"serve: missing binary for pane ($p.name): ($p.bin)"}
  }
}

# Register each pane's pty service idempotently (needs --store + --services):
# append the create frame only if the stored definition is missing or changed.
# Create frames are kept `forever` -- the runtime keeps the last known-good
# create as its hot-replace fallback (lifecycle I3), and an already-confirmed
# service resumes on every boot on its own (I2), so re-appending an identical
# create each boot would just pile up cruft.
def register-service [topic: string, config: string] {
  let last = (.last $topic)
  let current = if ($last | is-empty) { null } else { .cas $last.hash }
  if $current != $config { $config | .append $topic | ignore }
}

const SVC = '{
  run: {||
    ^PTYBIN run --target TARGET -- nu
    | lines | each {|l|
        let e = try { $l | from json } catch { null }
        if $e == null { return }
        match $e.t {
          "screen" => ( $e.html | .append "PFX.screen" --ttl last:1 )
          "diff"   => ( null | .append "PFX.diff" --ttl ephemeral --meta {body: $l} )
          "exit"   => ( {code: $e.code} | to json | .append "PFX.exit" --ttl last:1 )
          _ => null
        }
      } | ignore
  }
  duplex: true
}'

if ($HTTP_NU.store? | default null) != null and ($HTTP_NU.services? | default false) {
  for p in $panes {
    let closure = $SVC
      | str replace --all "PTYBIN" $p.bin
      | str replace --all "PFX" $"pty-($p.name)"
      | str replace --all "TARGET" $"grid-($p.name)"
    register-service $"xs.service.pty-($p.name).create" $closure
  }
}

let topics = $panes | get name | each {|n| [$"pty-($n).screen" $"pty-($n).diff"] } | flatten
let pane_names = $panes | get name

const TMPL = r#'<!doctype html>
<html><head><meta charset=utf-8>
<script type=module src=DATASTAR></script>
<style>
  :root{--term-bg:#111;--term-fg:#ddd;
    --c0:#000;--c1:#cd0000;--c2:#00cd00;--c3:#cdcd00;--c4:#1e90ff;--c5:#cd00cd;
    --c6:#00cdcd;--c7:#e5e5e5;--c8:#4d4d4d;--c9:#ff5454;--c10:#54ff54;--c11:#ffff54;
    --c12:#5454ff;--c13:#ff54ff;--c14:#54ffff;--c15:#fff;}
  body{background:#000;color:var(--term-fg);margin:0;font:14px/1.2 monospace;overflow:hidden}
  #panes{display:flex;height:100vh}
  .pane{flex:1;display:flex;flex-direction:column;background:var(--term-bg)}
  .pane+.pane{border-left:1px solid #444}
  .pane.focused .label{color:#fff;background:#333}
  .label{flex:none;font-size:11px;color:#888;background:#1a1a1a;padding:2px 8px}
  .scroll{flex:1;overflow-y:auto;position:relative}
  .scroll>[id^="grid-"]{white-space:pre;padding:8px;position:relative;box-sizing:border-box}
  .row{min-height:1.2em}
  .pane a{color:inherit;text-decoration:underline}
  .cursor{position:absolute;top:calc(8px + var(--cursor-row)*1.2em);left:calc(8px + var(--cursor-col)*1ch);
    width:1ch;height:1.2em;background:var(--term-fg);opacity:.4;pointer-events:none}
  .wc{display:inline-block;width:calc(var(--w)*1ch)}
  .sb{font-weight:bold}.si{font-style:italic}.su{text-decoration:underline}
  .sx{visibility:hidden}.ss{text-decoration:line-through}
  .f1{color:var(--c1)}.f2{color:var(--c2)}.f3{color:var(--c3)}.f4{color:var(--c4)}
  .f5{color:var(--c5)}.f6{color:var(--c6)}.f7{color:var(--c7)}
  .b1{background:var(--c1)}.b2{background:var(--c2)}.b3{background:var(--c3)}.b4{background:var(--c4)}
  .b5{background:var(--c5)}.b6{background:var(--c6)}.b7{background:var(--c7)}
</style></head>
<body data-init="@get('/sse')">
  <div id=panes>__PANES_HTML__</div>
  <script type=module>
    const PANES = __PANES_JS__;
    // The client is byte-blind: it ships semantic key events and the
    // emulator encodes them against its live input modes (application
    // cursor keys, bracketed paste, ...). See PROTOCOL.md {t:key}/{t:paste}.
    const NAMED = ["ArrowUp","ArrowDown","ArrowLeft","ArrowRight","Home","End",
      "PageUp","PageDown","Insert","Delete","Enter","Tab","Backspace","Escape"];
    function keyEvent(ev) {
      if (["Shift","Control","Alt","Meta","CapsLock"].includes(ev.key)) return null;
      if (ev.metaKey) return null; // Cmd+x belongs to the browser/OS
      const mods = (ev.shiftKey?1:0)|(ev.altKey?2:0)|(ev.ctrlKey?4:0);
      if (ev.key.length === 1) {
        // Option as compose (non-US layouts) delivers a finished glyph in
        // ev.key; drop the alt bit so it isn't re-encoded as a Meta chord.
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
    // Awaited send queue, one per pane: at most one POST in flight, so
    // frames reach the server in press order by construction. While one is
    // in flight the backlog accumulates and drains as a single NDJSON
    // batch, capping the added delay at one round trip.
    const queues = {};
    function send(pane, frame) {
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
    // Focus: keystrokes go to the clicked pane.
    let focused = PANES[0];
    function setFocus(name) {
      focused = name;
      document.querySelectorAll(".pane").forEach(p =>
        p.classList.toggle("focused", p.dataset.pane === name));
    }
    document.querySelectorAll(".pane").forEach(p =>
      p.addEventListener("mousedown", () => setFocus(p.dataset.pane)));
    setFocus(focused);
    // ?drive replays a key/paste script from the URL hash (base64 JSON, the
    // bench/keyprobe.json shape) as synthesized events, exercising the real
    // keydown/paste listeners end to end. Test harness affordance, like
    // ?nofit; see bench/keytest.nu.
    if (new URLSearchParams(location.search).has("drive")) (async () => {
      const spec = JSON.parse(atob(location.hash.slice(1)));
      if (spec.pane) setFocus(spec.pane);
      await new Promise(r => setTimeout(r, spec.startDelay ?? 2000));
      for (const s of spec.steps) {
        if (s.pause) await new Promise(r => setTimeout(r, s.pause));
        if (s.key !== undefined) {
          dispatchEvent(new KeyboardEvent("keydown", {
            key: s.key, code: s.code ?? "",
            shiftKey: !!(s.mods & 1), altKey: !!(s.mods & 2), ctrlKey: !!(s.mods & 4),
            bubbles: true, cancelable: true,
          }));
        }
        if (s.paste !== undefined) {
          const dt = new DataTransfer();
          dt.setData("text/plain", s.paste);
          dispatchEvent(new ClipboardEvent("paste", {clipboardData: dt, bubbles: true, cancelable: true}));
        }
        await new Promise(r => setTimeout(r, s.gap ?? 60));
      }
    })();
    addEventListener("keydown", ev => {
      // Composing keydowns (dead keys, IME) are provisional; skip them.
      if (ev.isComposing || ev.keyCode === 229) return;
      // Leave keys aimed at real form fields alone.
      const t = ev.target, tag = t && t.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || (t && t.isContentEditable)) return;
      // Selection beats SIGINT: Ctrl+C with a selection copies.
      if (ev.ctrlKey && !ev.altKey && !ev.metaKey && ev.key === "c") {
        const sel = getSelection().toString();
        if (sel) { navigator.clipboard.writeText(sel).catch(()=>{}); ev.preventDefault(); return; }
      }
      const frame = keyEvent(ev);
      if (frame === null) return;
      ev.preventDefault();
      send(focused, frame);
    });
    addEventListener("paste", ev => {
      const text = ev.clipboardData?.getData("text");
      if (!text) return;
      ev.preventDefault();
      send(focused, {t:"paste", s:text});
    });
    document.addEventListener("copy", ev => {
      // Rows are space-padded to full width; strip trailing whitespace
      // per line so copies match what a terminal emulator would yield.
      const sel = getSelection(); if (!sel) return;
      const text = sel.toString(); if (!text) return;
      const trimmed = text.split("\n").map(l => l.replace(/[ \t]+$/,"")).join("\n");
      if (trimmed === text) return;
      ev.clipboardData?.setData("text/plain", trimmed);
      ev.preventDefault();
    });
    // Fit each pty to its pane: measure one rendered line box from a probe
    // row (inherits the pane font and 1.2 line-height), derive cols/rows
    // from the pane box, and send {t:resize} when the geometry changes.
    // Every open tab does this, so the last viewer to resize wins.
    const lastFit = {};
    function fit() {
      for (const name of PANES) {
        const box = document.querySelector(`.pane[data-pane=${name}] .scroll`);
        if (!box) continue;
        const probe = document.createElement("div");
        probe.textContent = "M".repeat(40);
        probe.style.cssText = "position:absolute;visibility:hidden;white-space:pre";
        box.appendChild(probe);
        const r = probe.getBoundingClientRect();
        box.removeChild(probe);
        if (r.width === 0 || r.height === 0) continue;
        const cols = Math.max(20, Math.floor((box.clientWidth - 16) / (r.width / 40)));
        const rows = Math.max(5, Math.floor((box.clientHeight - 16) / r.height));
        const prev = lastFit[name];
        if (!prev || prev.cols !== cols || prev.rows !== rows) {
          lastFit[name] = {cols, rows};
          send(name, {t:"resize", cols, rows});
        }
      }
    }
    // ?nofit observes without resizing the ptys -- for screenshots and
    // debugging; a second viewer's fit would otherwise fight this one's.
    if (!new URLSearchParams(location.search).has("nofit")) {
      let fitTimer;
      addEventListener("resize", () => { clearTimeout(fitTimer); fitTimer = setTimeout(fit, 200); });
      fit();
    }
    // Follow the cursor like a terminal, not the last grid row: after a
    // clear the prompt sits at the top of the screen region with blank
    // rows below it, and bottom-pinning would hide it. Scrolling the
    // cursor out of view pauses following; scrolling it back resumes --
    // our own follow scrolls keep it visible, so no self-scroll flag is
    // needed.
    const follow = {};
    function cursorInView(name) {
      const box = document.querySelector(`.pane[data-pane=${name}] .scroll`);
      const cur = document.getElementById(`grid-${name}-cursor`);
      if (!box || !cur) return true;
      const b = box.getBoundingClientRect(), c = cur.getBoundingClientRect();
      return c.bottom > b.top && c.top < b.bottom;
    }
    document.querySelectorAll(".pane").forEach(p => {
      const name = p.dataset.pane;
      follow[name] = true;
      p.querySelector(".scroll").addEventListener("scroll", () => {
        follow[name] = cursorInView(name);
      });
    });
    new MutationObserver(() => {
      for (const name of PANES) {
        if (follow[name]) {
          document.getElementById(`grid-${name}-cursor`)?.scrollIntoView({block: "nearest"});
        }
      }
    }).observe(document.body, {childList: true, subtree: true});
  </script>
</body></html>'#

let PAGE = (
  $TMPL
  | str replace "__PANES_HTML__" (
      $panes | each {|p|
        $"<div class=pane data-pane=($p.name)><div class=label>($p.name)</div><div class=scroll><div id=grid-($p.name)>connecting...</div></div></div>"
      } | str join ""
    )
  | str replace "__PANES_JS__" ($pane_names | to json --raw)
)

{|req|
  dispatch $req [
    (route {method: "GET", path: "/"} {|req ctx|
      $PAGE | str replace "DATASTAR" $DATASTAR_JS_PATH | metadata set --content-type "text/html"
    })

    # One stream for every pane: each stored keyframe replays first
    # (last:1), then live frames. A keyframe is one morph of its pane's
    # grid div. A diff expands to up to three patch events: changed rows +
    # cursor (morph by id), appended rows (append into the pane's grid),
    # trimmed rows (remove by id).
    (route {method: "GET", path: "/sse"} {|req ctx|
      .cat --follow
      | where topic in $topics
      | each {|f|
          if ($f.topic | str ends-with ".screen") {
            [(.cas $f.hash | to datastar-patch-elements)]
          } else {
            let d = $f.meta.body | from json
            [
              (if ($d.patch | is-not-empty) { $d.patch | to datastar-patch-elements })
              (if ($d.append | is-not-empty) {
                $d.append | to datastar-patch-elements --mode append --selector $"#($d.target)"
              })
              (if ($d.trim | is-not-empty) {
                "" | to datastar-patch-elements --mode remove --selector ($d.trim | each {|id| $"#($id)"} | str join ",")
              })
            ] | compact
          }
        }
      | flatten
      | to sse
      | metadata set --content-type "text/event-stream"
    })

    # The body is one or more ptyZZZ command frames as NDJSON ({t:key},
    # {t:paste}, {t:input}, {t:resize}), passed through verbatim to the
    # pane's service; the client batches queued frames into one POST.
    (route {method: "POST", path: "/input"} {|req ctx|
      let body = $in | into string | str trim --right --char "\n"
      let pane = $req.query?.pane? | default $pane_names.0
      if $pane in $pane_names {
        $body + "\n" | .append $"pty-($pane).send" | ignore
        null | metadata set { merge {'http.response': {status: 204}} }
      } else {
        "unknown pane" | metadata set { merge {'http.response': {status: 400}} }
      }
    })

    # Probe helper: a pane's current keyframe html as text/plain.
    (route {method: "GET", path: "/snap"} {|req ctx|
      let pane = $req.query?.pane? | default $pane_names.0
      let f = .last $"pty-($pane).screen"
      if ($f | is-empty) { "no screen yet" } else { .cas $f.hash } | metadata set --content-type "text/plain"
    })
  ]
}
