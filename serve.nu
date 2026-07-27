# ptyZZZ experiment: a pty owned by an xs *service*, shown over one /sse.
#
# Run:
#   ~/http-nu/target/debug/http-nu --dev --datastar --services --store ./store \
#     :5111 ~/ptyZZZ/serve.nu
#
# Flow:
#   POST /input   -> append `pty.send`  (JSONL {t:input,b:..})  -> service stdin -> ptyZZZ
#   ptyZZZ stdout -> service closure fans JSONL into frames:
#     screen (keyframe) -> `pty.screen` (ttl last:1)    full grid; the join point
#     diff              -> `pty.diff`   (ttl ephemeral) changed/appended/trimmed rows
#   GET  /sse     -> follow both topics, patch #grid
#
# A joiner replays the stored keyframe and applies live diffs on top; any
# missed diffs or delta bugs heal at the next keyframe (ptyZZZ emits one every
# --keyframe-interval seconds while diffs are flowing, and on resize/alt-flip).

use http-nu/datastar *
use http-nu/router *

const PTYZZZ = (path self | path dirname | path join "target" "release" "ptyZZZ")

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
          'diff'   => ( $l | .append 'pty.diff' --ttl ephemeral )
          'exit'   => ( {code: $e.code} | to json | .append 'pty.exit' --ttl last:1 )
          _ => null
        }
      } | ignore
  }
  duplex: true
}"
  register-service "xs.service.pty.create" ($closure | str replace "PTYBIN" $PTYZZZ)
}

const PAGE = "<!doctype html>
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
<body data-init=\"@get('/sse')\">
  <div id=grid>connecting...</div>
  <script type=module>
    addEventListener('keydown', e => {
      if (e.metaKey||e.ctrlKey&&e.key.length>1) return;
      let b = e.key;
      if (b==='Enter') b='\\n'; else if (b==='Backspace') b='\\x7f';
      else if (b==='Tab') b='\\t'; else if (b==='Escape') b='\\x1b';
      else if (b.length!==1) return;
      e.preventDefault();
      fetch('/input',{method:'POST',body:b});
    });
    // follow the tail like a terminal, but let the user scroll back through
    // history undisturbed; resume following when they return to the bottom
    let follow = true;
    addEventListener('scroll', () => {
      follow = innerHeight + scrollY >= document.body.scrollHeight - 48;
    });
    new MutationObserver(() => {
      if (follow) scrollTo(0, document.body.scrollHeight);
    }).observe(document.body, {childList: true, subtree: true});
  </script>
</body></html>"

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
          let body = .cas $f.hash
          if $f.topic == "pty.screen" {
            [($body | to datastar-patch-elements)]
          } else {
            let d = $body | from json
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

    (route {method: "POST", path: "/input"} {|req ctx|
      let body = $in | into string
      ({t: "input", b: $body} | to json --raw) + "\n" | .append "pty.send" | ignore
      null | metadata set { merge {'http.response': {status: 204}} }
    })

    # Probe helper: current keyframe html as text/plain.
    (route {method: "GET", path: "/snap"} {|req ctx|
      let f = .last "pty.screen"
      if ($f | is-empty) { "no screen yet" } else { .cas $f.hash } | metadata set --content-type "text/plain"
    })
  ]
}
