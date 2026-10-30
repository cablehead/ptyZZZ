# ptyZZZ's smallest browser example: one terminal that fills the window.
#
# Run:
#   ~/http-nu/target/release/http-nu --dev --datastar --services --store ./store \
#     :5111 ~/ptyZZZ/serve.nu
#
# The pty runs as an xs duplex service, `pty-local`, backed by this repo's
# release binary if present, otherwise ptyZZZ on PATH.
#
#   POST /input -> append `pty-local.send` (ephemeral) -> service stdin
#   ptyZZZ stdout -> the service fans its JSONL into frames:
#     screen (keyframe) -> `pty-local.screen` (ttl last:1)
#     diff              -> `pty-local.diff`   (ttl ephemeral)
#   GET  /sse   -> follow both topics, patch the grid by element id
#
# Diffs carry their payload in frame meta ({body: ...}), not the CAS: an
# ephemeral frame is never stored, so a CAS write would be wasted (~30us a
# frame via CAS vs ~0.4us via meta). Keyframes stay in the CAS.
#
# The browser side is <pty-term> (static/pty-term.js): keys, paste, fitting
# the pty to the window, and pinning to live output.

use http-nu/datastar *
use http-nu/router *

const HERE = (path self | path dirname)
const LOCAL_BIN = ($HERE | path join "target" "release" "ptyZZZ")

def resolve-ptyzzz [] {
  if ($LOCAL_BIN | path exists) { $LOCAL_BIN } else {
    let found = which ptyZZZ
    if ($found | is-empty) {
      error make {msg: "serve: no ptyZZZ binary (cargo build --release, or put ptyZZZ on PATH)"}
    } else {
      $found.0.path
    }
  }
}

# Register the pty service idempotently (needs --store + --services): append
# the create frame only if the stored definition is missing or changed. The
# runtime keeps the last known-good create and resumes a confirmed service on
# every boot, so re-appending an identical create would just pile up.
def register-service [topic: string, config: string] {
  let last = (.last $topic)
  let current = if ($last | is-empty) { null } else { .cas $last.hash }
  if $current != $config { $config | .append $topic | ignore }
}

const SVC = '{
  run: {||
    ^PTYBIN run --die-with-parent --target grid-local -- nu
    | lines | each {|l|
        let e = try { $l | from json } catch { null }
        if $e == null { return }
        match $e.t {
          "screen" => ( $e.html | .append "pty-local.screen" --ttl last:1 --meta {seqno: $e.seqno} )
          "diff"   => ( null | .append "pty-local.diff" --ttl ephemeral --meta {body: $l} )
          "exit"   => ( {code: $e.code} | to json | .append "pty-local.exit" --ttl last:1 )
          _ => null
        }
      } | ignore
  }
  duplex: true
}'

if ($HTTP_NU.store? | default null) != null and ($HTTP_NU.services? | default false) {
  register-service "xs.service.pty-local.create" ($SVC | str replace "PTYBIN" (resolve-ptyzzz))
}

const PAGE = r#'<!doctype html>
<html><head><meta charset=utf-8>
<script type=module src="/static/vendor/datastar-rocket@1.0.4.js"></script>
<script type=module src="/static/pty-term.js"></script>
<link rel=stylesheet href="/static/pty-term.css">
<style>body{margin:0;height:100vh;display:flex;background:var(--term-bg)}</style>
</head>
<body data-init="@get('/sse')">
  <pty-term pane=local><div class=scroll><div id=grid-local>connecting...</div></div></pty-term>
  <script type=module>
    await customElements.whenDefined("pty-term");
    requestAnimationFrame(() => document.querySelector("pty-term").park?.());
    if (location.search.includes("drive")) import("/static/drive.js");
  </script>
</body></html>'#

{|req|
  dispatch $req [
    (route {method: "GET", path: "/"} {|req ctx|
      $PAGE | metadata set --content-type "text/html"
    })

    # The follow starts at the stored keyframe (last:1 keeps one), so a
    # connect replays only what came after it, not the whole store. Once the
    # replay is done (xs.threshold) it asks the pty for a fresh keyframe.
    # Diffs are ephemeral, so the fold keeps the seqno last sent (-1 before
    # the first keyframe) and forwards a diff only when its base matches; a
    # missed one heals at the next keyframe. A diff expands to up to three patches: changed rows + cursor
    # (morph by id), appended rows, trimmed rows.
    (route {method: "GET", path: "/sse"} {|req ctx|
      let seed = .last "pty-local.screen"
      let head = .cat --last 1 | get 0?.id?
      # Piped straight from the if: bound to a variable, the follow would be
      # collected and never end.
      if $seed != null { .cat --follow --from $seed.id } else if $head != null { .cat --follow --after $head } else { .cat --follow }
      | where topic in ["pty-local.screen" "pty-local.diff" "xs.threshold"]
      | generate {|f, sent|
          if $f.topic == "xs.threshold" {
            '{"t":"screen"}' + "\n" | .append "pty-local.send" --ttl ephemeral | ignore
            return {next: $sent}
          }
          if $f.topic == "pty-local.screen" {
            return {out: [(.cas $f.hash | to datastar-patch-elements)], next: ($f.meta.seqno? | default (-1))}
          }
          let d = $f.meta.body | from json
          if $d.base != $sent { return {next: $sent} }
          {
            out: ([
              (if ($d.patch | is-not-empty) { $d.patch | to datastar-patch-elements })
              (if ($d.append | is-not-empty) {
                $d.append | to datastar-patch-elements --mode append --selector $"#($d.target)"
              })
              (if ($d.trim | is-not-empty) {
                "" | to datastar-patch-elements --mode remove --selector ($d.trim | each {|id| $"#($id)"} | str join ",")
              })
            ] | compact)
            next: $d.seqno
          }
        } (-1)
      | flatten
      | to sse
      | metadata set --content-type "text/event-stream"
    })

    # The body is one or more ptyZZZ command frames as NDJSON ({t:key},
    # {t:paste}, {t:input}, {t:resize}), passed through to the service.
    # Ephemeral: the service reads a keystroke once, so storing it only grows
    # the store.
    (route {method: "POST", path: "/input"} {|req ctx|
      let body = $in | into string | str trim --right --char "\n"
      $body + "\n" | .append "pty-local.send" --ttl ephemeral | ignore
      null | metadata set { merge {'http.response': {status: 204}} }
    })

    # <pty-term>, its styles and font, the key test harness, and the vendored
    # Datastar Rocket bundle.
    (route {method: "GET", path-matches: "/static/:file"} {|req ctx|
      .static ($HERE | path join "static") $"/($ctx.file)"
    })
    (route {method: "GET", path-matches: "/static/vendor/:file"} {|req ctx|
      .static ($HERE | path join "static" "vendor") $"/($ctx.file)"
    })
    (route {method: "GET", path-matches: "/static/fonts/:file"} {|req ctx|
      .static ($HERE | path join "static" "fonts") $"/($ctx.file)"
    })

    # Probe helper for bench/keytest.nu: the current keyframe html as text.
    (route {method: "GET", path: "/snap"} {|req ctx|
      let f = .last "pty-local.screen"
      if ($f | is-empty) { "no screen yet" } else { .cas $f.hash } | metadata set --content-type "text/plain"
    })
  ]
}
