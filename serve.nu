# ptyZZZ experiment: a pty owned by an xs *service*, shown over one /sse.
#
# Run:
#   ~/http-nu/target/release/http-nu --dev --datastar --services --store ./store \
#     :5111 ~/ptyZZZ/serve.nu
#
# Flow:
#   POST /input   -> append `pty.send`  (JSONL {t:input,b:..})  -> service stdin -> ptyZZZ
#   ptyZZZ stdout -> service closure fans JSONL into frames:
#     screen (keyframe) -> `pty.screen` (ttl last:1)    full grid; the join point
#     diff              -> `pty.diff`   (ttl ephemeral) changed/appended/trimmed rows
#
# Diffs carry their payload in frame meta ({body: ...}), not the CAS. An
# ephemeral frame is broadcast and never stored, so writing its content to the
# CAS would be a disk write with no reader benefit -- benched at ~30us/frame
# via CAS vs ~0.4us via meta, plus it saves a CAS read per subscriber per
# frame on /sse. Keyframes stay in the CAS: stored (last:1), large, rare.
#   GET  /sse     -> follow both topics, patch #grid
#
# A joiner replays the stored keyframe and applies live diffs on top; any
# missed diffs or delta bugs heal at the next keyframe (ptyZZZ emits one every
# --keyframe-interval seconds while diffs are flowing, and on resize/alt-flip).

use http-nu/datastar *
use http-nu/router *

const PTYZZZ = (path self | path dirname | path join "target" "release" "ptyZZZ")

# Fail at load with a clear message rather than registering a service whose
# spawn dies silently (the page would just sit at "connecting...").
if not ($PTYZZZ | path exists) {
  error make {msg: $"serve: missing binary, build it first: cargo build --release \(expected ($PTYZZZ))"}
}

# Register the pty service idempotently (needs --store + --services): append
# xs.service.pty.create only if the stored definition is missing or changed.
# Create frames are kept `forever` -- the runtime keeps the last known-good create
# as its hot-replace fallback (lifecycle I3), and an already-confirmed service
# resumes on every boot on its own (I2), so re-appending an identical create each
# boot would just pile up cruft. (last:1 is for app data instead -- the pty.screen
# and pty.exit output below.)
def register-service [topic: string, config: string] {
  let last = (.last $topic)
  let current = if ($last | is-empty) { null } else { .cas $last.hash }
  if $current != $config { $config | .append $topic | ignore }
}

if ($HTTP_NU.store? | default null) != null and ($HTTP_NU.services? | default false) {
  let closure = "{
  run: {||
    ^PTYBIN run -- nu
    | lines | each {|l|
        let e = try { $l | from json } catch { null }
        if $e == null { return }
        match $e.t {
          'screen' => ( $e.html | .append 'pty.screen' --ttl last:1 )
          'diff'   => ( null | .append 'pty.diff' --ttl ephemeral --meta {body: $l} )
          'exit'   => ( {code: $e.code} | to json | .append 'pty.exit' --ttl last:1 )
          _ => null
        }
      } | ignore
  }
  duplex: true
}"
  register-service "xs.service.pty.create" ($closure | str replace "PTYBIN" $PTYZZZ)
}

const PAGE = r#'<!doctype html>
<html><head><meta charset=utf-8>
<script type=module src=DATASTAR></script>
<style>
  :root{--term-bg:#111;--term-fg:#ddd;
    --c0:#000;--c1:#cd0000;--c2:#00cd00;--c3:#cdcd00;--c4:#1e90ff;--c5:#cd00cd;
    --c6:#00cdcd;--c7:#e5e5e5;--c8:#4d4d4d;--c9:#ff5454;--c10:#54ff54;--c11:#ffff54;
    --c12:#5454ff;--c13:#ff54ff;--c14:#54ffff;--c15:#fff;}
  body{background:#000;color:var(--term-fg);margin:0;font:14px/1.2 monospace}
  #grid{white-space:pre;padding:8px;background:var(--term-bg);position:relative;min-height:100vh;box-sizing:border-box}
  .row{min-height:1.2em}
  #grid a{color:inherit;text-decoration:underline}
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
  <div id=grid>connecting...</div>
  <script type=module>
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
    // Awaited send queue: at most one POST in flight, so frames reach the
    // server in press order by construction. While one is in flight the
    // backlog accumulates and drains as a single NDJSON batch, capping the
    // added delay at one round trip regardless of typing speed.
    let queue = [], sending = false;
    function send(frame) {
      queue.push(JSON.stringify(frame));
      drain();
    }
    async function drain() {
      if (sending) return;
      sending = true;
      while (queue.length) {
        const batch = queue.splice(0).join("\n") + "\n";
        try { await fetch("/input", {method:"POST", body: batch}); } catch {}
      }
      sending = false;
    }
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
      send(frame);
    });
    addEventListener("paste", ev => {
      const text = ev.clipboardData?.getData("text");
      if (!text) return;
      ev.preventDefault();
      send({t:"paste", s:text});
    });
    // Fit the pty to the window: measure one rendered line box from a
    // probe row (inherits the grid font and 1.2 line-height), derive
    // cols/rows from the viewport minus the grid padding, and send
    // {t:resize} when the geometry changes. Note every open tab does
    // this, so the last viewer to resize wins, like tmux.
    function fit() {
      const grid = document.getElementById("grid");
      const probe = document.createElement("div");
      probe.textContent = "M".repeat(40);
      probe.style.cssText = "position:absolute;visibility:hidden;white-space:pre";
      grid.appendChild(probe);
      const r = probe.getBoundingClientRect();
      grid.removeChild(probe);
      if (r.width === 0 || r.height === 0) return;
      const cols = Math.max(20, Math.floor((grid.clientWidth - 16) / (r.width / 40)));
      const rows = Math.max(5, Math.floor((innerHeight - 16) / r.height));
      if (cols !== fit.cols || rows !== fit.rows) {
        fit.cols = cols; fit.rows = rows;
        send({t:"resize", cols, rows});
      }
    }
    let fitTimer;
    addEventListener("resize", () => { clearTimeout(fitTimer); fitTimer = setTimeout(fit, 200); });
    fit();
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
    // follow the tail like a terminal, but let the user scroll back through
    // history undisturbed; resume following when they return to the bottom
    let follow = true;
    addEventListener("scroll", () => {
      follow = innerHeight + scrollY >= document.body.scrollHeight - 48;
    });
    new MutationObserver(() => {
      if (follow) scrollTo(0, document.body.scrollHeight);
    }).observe(document.body, {childList: true, subtree: true});
  </script>
</body></html>'#

{|req|
  dispatch $req [
    (route {method: "GET", path: "/"} {|req ctx|
      $PAGE | str replace "DATASTAR" $DATASTAR_JS_PATH | metadata set --content-type "text/html"
    })

    # One stream: the stored keyframe replays first (last:1), then live frames.
    # A keyframe is one morph of #grid. A diff expands to up to three patch
    # events: changed rows + cursor (morph by id), appended rows (append into
    # the grid), trimmed rows (remove by id).
    (route {method: "GET", path: "/sse"} {|req ctx|
      .cat --follow
      | where topic in ["pty.screen" "pty.diff"]
      | each {|f|
          if $f.topic == "pty.screen" {
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
    # {t:paste}, {t:input}, {t:resize}), passed through verbatim; the client
    # batches queued frames into one POST. See PROTOCOL.md.
    (route {method: "POST", path: "/input"} {|req ctx|
      let body = $in | into string | str trim --right --char "\n"
      $body + "\n" | .append "pty.send" | ignore
      null | metadata set { merge {'http.response': {status: 204}} }
    })

    # Probe helper: current keyframe html as text/plain.
    (route {method: "GET", path: "/snap"} {|req ctx|
      let f = .last "pty.screen"
      if ($f | is-empty) { "no screen yet" } else { .cas $f.hash } | metadata set --content-type "text/plain"
    })
  ]
}
