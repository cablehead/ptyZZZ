# examples/through -- one full-screen ptyZZZ pane that can yaw to show the
# headless wezterm behind it, projecting HTML onto the page.
#
# Glue only: one duplex pty service, routes, no splits. Markup is minijinja
# under templates/; CSS/JS under static/.
#
# Run (needs store + services + datastar):
#   http-nu --dev --datastar --services --store ./through-store 127.0.0.1:5111 examples/through/serve.nu
#   # add --tls <pem> to serve https, so browsers ask for brotli on /sse

use http-nu/datastar *
use http-nu/router *

const HERE = (path self | path dirname)
const TPL = ($HERE | path join "templates")
const PTYZZZ = ($HERE | path join ".." ".." "target" "release" "ptyZZZ" | path expand)
const FONTS = ($HERE | path join ".." ".." "static" "fonts")
const PANE = "p1"

if not ($PTYZZZ | path exists) {
  error make {msg: $"through: missing ($PTYZZZ) -- cargo build --release"}
}

def register-service [topic: string, config: string] {
  let last = (.last $topic)
  let current = if ($last | is-empty) { null } else { .cas $last.hash }
  if $current != $config { $config | .append $topic | ignore }
}

const SVC = '{
  run: {||
    ^PTYBIN run --die-with-parent --target TARGET -- nu
    | lines | each {|l|
        let e = try { $l | from json } catch { null }
        if $e == null { return }
        match $e.t {
          "screen" => ( $e.html | .append "PFX.screen" --ttl last:1 --meta {seqno: $e.seqno} )
          "diff"   => ( null | .append "PFX.diff" --ttl ephemeral --meta {body: $l} )
          "exit"   => ( {code: $e.code} | to json | .append "PFX.exit" --ttl last:1 )
          _ => null
        }
      } | ignore
  }
  duplex: true
}'

def spawn-pane [] {
  let closure = $SVC
    | str replace --all "PTYBIN" $PTYZZZ
    | str replace --all "PFX" $"pty-($PANE)"
    | str replace --all "TARGET" $"grid-($PANE)"
  register-service $"xs.service.pty-($PANE).create" $closure
}

def emit-signals [signals: record] {
  null | .append "through.patch" --ttl ephemeral --meta {signals: ($signals | to json --raw)} | ignore
}

# Seed whenever a store is open. Do not gate on $HTTP_NU.services: `http-nu
# eval --services` starts dispatchers but leaves that const false, so a
# services check would skip the seed and tests would see no pty.
if ($HTTP_NU.store? | default null) != null {
  spawn-pane
  # Anchor for /sse when the first keyframe has not landed yet.
  null | .append "through.ready" --ttl last:1 | ignore
}

{|req|
  dispatch $req [
    (route {method: "GET", path: "/"} {|req ctx|
      {datastar: $DATASTAR_JS_PATH}
      | .mj ($TPL | path join "page.html")
      | metadata set --content-type "text/html"
    })

    # One read: start at the retained keyframe (or the ready frame) and follow.
    # `--from` is inclusive. Diffs are ephemeral, so a missed one is repaired
    # by the healing keyframe. After replay, xs.threshold fires and we ask
    # for a fresh screen so a join does not wait out --keyframe-interval.
    (route {method: "GET", path: "/sse"} {|req ctx|
      let seed = (try { .last $"pty-($PANE).screen" } catch { null })
      let from = (if $seed == null {
        (.last "through.ready" | get id)
      } else {
        $seed.id
      })

      .cat --follow --from $from
      | generate {|f, s|
          if $f.topic == "xs.threshold" {
            '{"t":"screen"}' + "\n" | .append $"pty-($PANE).send" --ttl ephemeral | ignore
            return {next: $s}
          }

          if ($f.topic | str starts-with "pty-") {
            let kind = ($f.topic | split row "." | get 1)

            if $kind == "screen" {
              return {
                out: [(.cas $f.hash | to datastar-patch-elements)]
                next: ($s | upsert sent ($f.meta.seqno))
              }
            }

            if $kind == "diff" {
              let d = ($f.meta.body | from json)
              if $s.sent == null or $d.base != $s.sent { return {next: $s} }
              let o = ([
                (if ($d.patch | is-not-empty) { $d.patch | to datastar-patch-elements })
                (if ($d.append | is-not-empty) {
                  $d.append | to datastar-patch-elements --mode append --selector $"#($d.target)"
                })
                (if ($d.trim | is-not-empty) {
                  "" | to datastar-patch-elements --mode remove --selector ($d.trim | each {|t| $"#($t)"} | str join ",")
                })
              ] | compact)
              return {
                out: $o
                next: ($s | upsert sent $d.seqno)
              }
            }
            return {next: $s}
          }

          if $f.topic == "through.patch" {
            let p = $f.meta
            if "signals" in ($p | columns) {
              return {out: [($p.signals | to datastar-patch-signals)], next: $s}
            }
            return {next: $s}
          }

          {next: $s}
        } {sent: null}
      | flatten
      | to sse
      | metadata set --content-type "text/event-stream"
    })

    (route {method: "POST", path: "/ping"} {|req ctx|
      let signals = (try { $in | into string | from json } catch { {} })
      emit-signals {pong: ($signals.ping? | default 0)}
      null | metadata set { merge {'http.response': {status: 204}} }
    })

    (route {method: "POST", path: "/input"} {|req ctx|
      let body = $in | into string | str trim --right --char "\n"
      $body + "\n" | .append $"pty-($PANE).send" --ttl ephemeral | ignore
      null | metadata set { merge {'http.response': {status: 204}} }
    })

    (route {method: "GET", path: "/shots"} {|req ctx|
      let dir = $HERE | path join "shots"
      let files = if ($dir | path exists) {
        ls $dir | where {|x| $x.type == "file" or $x.type == "image"} | each {|x|
          let name = $x.name | path basename
          let ext = ($name | path parse | get extension | default "" | str downcase)
          let kind = if $ext in ["png" "jpg" "jpeg" "webp" "gif"] { "img" } else if $ext in ["webm" "mp4"] { "video" } else { "skip" }
          {name: $name, kind: $kind}
        } | where kind != "skip" | sort-by name
      } else { [] }
      {files: $files} | .mj ($TPL | path join "shots.html")
      | metadata set --content-type "text/html"
    })

    (route {method: "GET", path-matches: "/shots/:file"} {|req ctx|
      .static ($HERE | path join "shots") $"/($ctx.file)"
    })

    (route {method: "GET", path-matches: "/static/:file"} {|req ctx|
      .static ($HERE | path join "static") $"/($ctx.file)"
    })

    (route {method: "GET", path-matches: "/fonts/:file"} {|req ctx|
      .static $FONTS $"/($ctx.file)"
    })
  ]
}
