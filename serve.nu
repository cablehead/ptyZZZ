# ptyZZZ experiment: a pty owned by an xs *service*, shown over one /sse.
#
# Run:
#   ~/http-nu/target/debug/http-nu --dev --datastar --services --store ./store \
#     :5111 ~/ptyZZZ/serve.nu
#
# Flow:
#   POST /input   -> append `pty.send`  (JSONL {t:input,b:..})  -> service stdin -> ptyZZZ
#   ptyZZZ stdout -> service closure fans JSONL into `pty.screen` frames (ttl last:1)
#   GET  /sse     -> follow `pty.screen`, morph #grid

use http-nu/datastar *
use http-nu/router *

const PTYZZZ = (path self | path dirname | path join "target" "debug" "ptyZZZ")

# Register the pty service once (needs --store + --services). Re-append on each
# boot replaces the running service (xs hot-reload), so this is restart-safe.
if ($HTTP_NU.store? | default null) != null and ($HTTP_NU.services? | default false) {
  let closure = "{
  run: {||
    ^PTYBIN run -- bash
    | lines | each {|l|
        let e = try { $l | from json } catch { null }
        if $e == null { return }
        match $e.t {
          'screen' => ( $e.html | .append 'pty.screen' --ttl last:1 )
          'exit'   => ( {code: $e.code} | to json | .append 'pty.exit' --ttl last:1 )
          _ => null
        }
      } | ignore
  }
  duplex: true
}"
  $closure | str replace "PTYBIN" $PTYZZZ | .append "xs.service.pty.create" --ttl last:1 | ignore
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
  #grid{white-space:pre;padding:8px;background:var(--term-bg)}
  .row{min-height:1.2em}
  .wc{display:inline-block;width:calc(var(--w)*1ch)}
  .sb{font-weight:bold}.si{font-style:italic}.su{text-decoration:underline}
  .f1{color:var(--c1)}.f2{color:var(--c2)}.f3{color:var(--c3)}.f4{color:var(--c4)}
  .f5{color:var(--c5)}.f6{color:var(--c6)}.f7{color:var(--c7)}
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
  </script>
</body></html>"

{|req|
  dispatch $req [
    (route {method: "GET", path: "/"} {|req ctx|
      $PAGE | str replace "DATASTAR" $DATASTAR_JS_PATH | metadata set --content-type "text/html"
    })

    (route {method: "GET", path: "/sse"} {|req ctx|
      .cat --follow --topic "pty.screen"
      | where topic == "pty.screen"
      | each {|f| .cas $f.hash | to datastar-patch-elements }
      | to sse
      | metadata set --content-type "text/event-stream"
    })

    (route {method: "POST", path: "/input"} {|req ctx|
      let body = $in | into string
      ({t: "input", b: $body} | to json --raw) + "\n" | .append "pty.send" | ignore
      null | metadata set { merge {'http.response': {status: 204}} }
    })

    # Probe helper: current screen html as text/plain.
    (route {method: "GET", path: "/snap"} {|req ctx|
      let f = .last "pty.screen"
      if ($f | is-empty) { "no screen yet" } else { .cas $f.hash } | metadata set --content-type "text/plain"
    })
  ]
}
