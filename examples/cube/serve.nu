# examples/cube -- a rotating CSS-3D cube whose six faces are six live ptyZZZ
# screens. Five run animations; the front face is an interactive nu shell.
#
# This file is just the glue: it registers one pty service per face, renders the
# page from a minijinja template, and serves the frontend from static/. The page
# markup/CSS/JS live in templates/cube.html and static/cube.{css,js}.
#
# Run (needs store + services + datastar):
#   http-nu --dev --datastar --services --store ./store 127.0.0.1:5111 examples/cube/serve.nu
#   # add --tls <pem> to serve https
#
# Faces:
#   0 front  nu (interactive)   3 left    asciiquarium
#   1 right  boids_predator     4 top     mandelbrot
#   2 back   mandelbrot         5 bottom  asciiquarium
#
# Face 0 is a real terminal: its service is duplex, so keystrokes POSTed to
# /input (appended to pty0.send) are fed to that ptyZZZ's stdin as JSONL input.

use http-nu/datastar *
use http-nu/router *

# Directory of this script, so template/static/binary paths resolve regardless
# of the working directory http-nu was launched from.
const HERE = (path self | path dirname)

# --- config: binaries the faces run --------------------------------------
# ptyZZZ is built in this repo. The animations are vendored in bin/ as
# prebuilt binaries named <name>-<target triplet>; the one matching the
# platform http-nu runs on is picked at load, and load fails loudly when this
# platform's binaries are missing (see bin/README.md to build and add them).
const PTYZZZ = ($HERE | path join ".." ".." "target" "release" "ptyZZZ" | path expand)

def triplet [] {
  let u = (uname)
  match [$u."kernel-name", $u.machine] {
    ["Linux", "x86_64"] => "x86_64-unknown-linux-gnu"
    ["Linux", "aarch64"] => "aarch64-unknown-linux-gnu"
    ["Darwin", "x86_64"] => "x86_64-apple-darwin"
    ["Darwin", "arm64"] => "aarch64-apple-darwin"
    ["Windows_NT", "x86_64"] => "x86_64-pc-windows-msvc"
    _ => (error make {msg: $"cube: unsupported platform ($u | get kernel-name)/($u.machine)"})
  }
}

def anim-bin [name: string] {
  let bin = ($HERE | path join "bin" $"($name)-(triplet)")
  if not ($bin | path exists) {
    error make {msg: $"cube: missing ($bin) -- build it for this platform, see bin/README.md"}
  }
  $bin
}
let PLAYSTYLE = (anim-bin "play_style")
let AQUA = (anim-bin "asciiquarium")

# Per face, one of:
#   term  interactive nu shell (duplex service, takes keystrokes)
#   anim  a yazelix-screen play_style animation
#   aqua  the asciiquarium-rs aquarium
# cols/rows default to 78x39 (fills the square face at ~2:1 cell ratio). The
# term face is bigger so TUIs fit -- btop needs >=80 cols; 84x42 keeps 2:1.
const FACES = [
  {kind: "term", cols: 84, rows: 42}
  {kind: "anim", style: "boids_predator"}
  {kind: "anim", style: "mandelbrot"}
  {kind: "aqua"}
  {kind: "anim", style: "mandelbrot"}
  {kind: "aqua"}
]

# Fail loudly at load if a face binary is missing, rather than serving a dead
# face with no hint why.
for bin in [$PTYZZZ $PLAYSTYLE $AQUA] {
  if not ($bin | path exists) {
    error make {msg: $"cube: missing binary, build it first: ($bin)"}
  }
}

# Register a service idempotently: append its xs.service.<name>.create only if
# the stored definition is missing or has changed. Two facts make this the right
# shape. Create frames are kept `forever` (the runtime keeps the last known-good
# create as its hot-replace fallback -- lifecycle invariant I3 -- and a pruning
# ttl like last:/time: would delete it). And an already-confirmed service resumes
# on every boot on its own (I2), so re-appending an identical create each boot is
# pure cruft. So: forever + skip-if-unchanged, and no create accumulation across
# restarts. (Pruning ttl is for app *data* streams instead -- see pty.screen.<n>
# below, which uses last:1 so a fresh /sse replays only the current frame.)
def register-service [topic: string, config: string] {
  let last = (.last $topic)
  let current = if ($last | is-empty) { null } else { .cas $last.hash }
  if $current != $config { $config | .append $topic | ignore }
}

# One pty service per face. The term face is duplex, so its pty0.send topic feeds
# that ptyZZZ's stdin; each face renders into grid-<n>. Keyframes land on
# pty.screen.<n> (last:1, the join point); diffs on pty.diff.<n> (ephemeral).
# The term face keeps full scrollback -- its .fit is a scroll viewport, so the
# cube's front face is the standing single-terminal example (history + follow).
# Animation faces run --scrollback 0: they repaint in place and never scroll,
# so this is just insurance against stray scrolling output.
if ($HTTP_NU.store? | default null) != null and ($HTTP_NU.services? | default false) {
  $FACES | enumerate | each {|it|
    let runcmd = if $it.item.kind == "term" { "nu" } else if $it.item.kind == "aqua" { $AQUA } else { $"PLAYBIN ($it.item.style) 1000000" }
    let duplex = if $it.item.kind == "term" { "true" } else { "false" }
    let scrollback = if $it.item.kind == "term" { "3000" } else { "0" }
    let cols = ($it.item.cols? | default 78)
    let rows = ($it.item.rows? | default 39)
    let closure = "{
  run: {||
    ^PTYBIN run --cols COLS --rows ROWS --scrollback SCROLLBACK --target 'grid-IDX' -- RUNCMD
    | lines | each {|l|
        let e = try { $l | from json } catch { null }
        if $e == null { return }
        match $e.t {
          'screen' => ( $e.html | .append 'pty.screen.IDX' --ttl last:1 )
          'diff'   => ( $l | .append 'pty.diff.IDX' --ttl ephemeral )
          _ => null
        }
      } | ignore
  }
  duplex: DUPLEX
}"
    let config = ($closure
      | str replace --all "PTYBIN" $PTYZZZ
      | str replace --all "RUNCMD" $runcmd
      | str replace --all "PLAYBIN" $PLAYSTYLE
      | str replace --all "DUPLEX" $duplex
      | str replace --all "SCROLLBACK" $scrollback
      | str replace --all "COLS" ($cols | into string)
      | str replace --all "ROWS" ($rows | into string)
      | str replace --all "IDX" ($it.index | into string))
    register-service $"xs.service.pty($it.index).create" $config
  }
}

# Position class + placeholder label for each face, derived from FACES so the
# layout stays a single source of truth. Fed to the template as `faces`.
# `scroll` marks the term face: its .fit becomes a scrollback viewport.
const POS = [f-front f-right f-back f-left f-top f-bottom]
def face-views [] {
  $FACES | enumerate | each {|it|
    let label = if $it.item.kind == "term" { "nu terminal . type to me" } else if $it.item.kind == "aqua" { "asciiquarium" } else { $it.item.style }
    {i: $it.index, cls: ($POS | get $it.index), label: $label, scroll: ($it.item.kind == "term")}
  }
}

{|req|
  dispatch $req [
    # The page: rendered from templates/cube.html with the datastar bundle path
    # and the face list as context.
    (route {method: "GET", path: "/"} {|req ctx|
      {datastar: $DATASTAR_JS_PATH, faces: (face-views)}
      | .mj ($HERE | path join "templates" "cube.html")
      | metadata set --content-type "text/html"
    })

    # Frontend assets (css/js) from static/. .static sets the content-type.
    (route {method: "GET", path-matches: "/static/:file"} {|req ctx|
      .static ($HERE | path join "static") $"/($ctx.file)"
    })

    # One stream for all six faces: follow every pty.screen.<n> and
    # pty.diff.<n>. Stored html carries id=grid-<n>, so datastar targets the
    # right face with no per-face routing; a diff expands to up to three patch
    # events (changed rows + cursor morph, appended rows, trimmed rows).
    (route {method: "GET", path: "/sse"} {|req ctx|
      .cat --follow
      | where topic =~ '^pty\.(screen|diff)\.[0-5]$'
      | each {|f|
          let body = .cas $f.hash
          if ($f.topic | str starts-with "pty.screen.") {
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

    # Keystrokes for the interactive face: wrap the POST body as a ptyZZZ input
    # command and append it to pty0.send (the duplex service's stdin feed).
    (route {method: "POST", path: "/input"} {|req ctx|
      let body = $in | into string
      ({t: "input", b: $body} | to json --raw) + "\n" | .append "pty0.send" | ignore
      null | metadata set { merge {'http.response': {status: 204}} }
    })

    # Probe: which faces have produced a frame yet.
    (route {method: "GET", path: "/faces"} {|req ctx|
      0..5 | each {|n|
        let f = (.last $"pty.screen.($n)")
        {face: $n, live: (($f | is-empty) == false)}
      } | to json | metadata set --content-type "application/json"
    })
  ]
}
