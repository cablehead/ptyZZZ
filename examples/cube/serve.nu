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
# ptyZZZ is built in this repo; the animations come from sibling projects.
# Override the two external ones by editing these paths.
const PTYZZZ = ($HERE | path join ".." ".." "target" "release" "ptyZZZ" | path expand)
const PLAYSTYLE = ("~/yazelix-screen/target/release/examples/play_style" | path expand)
const AQUA = ("~/asciiquarium-rs/target/release/asciiquarium" | path expand)

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

# Register one pty service per face. Re-append on each boot hot-reloads the
# running service (xs replaces it), so this is restart-safe. Each ptyZZZ renders
# into grid-<n> and each screen frame is appended to pty.screen.<n> (ttl last:1,
# so a fresh /sse replays the current frame of every face on connect). The term
# face is duplex so its <name>.send topic feeds ptyZZZ's stdin.
if ($HTTP_NU.store? | default null) != null and ($HTTP_NU.services? | default false) {
  $FACES | enumerate | each {|it|
    let runcmd = if $it.item.kind == "term" { "nu" } else if $it.item.kind == "aqua" { $AQUA } else { $"PLAYBIN ($it.item.style) 1000000" }
    let duplex = if $it.item.kind == "term" { "true" } else { "false" }
    let cols = ($it.item.cols? | default 78)
    let rows = ($it.item.rows? | default 39)
    let closure = "{
  run: {||
    ^PTYBIN run --cols COLS --rows ROWS --target 'grid-IDX' -- RUNCMD
    | lines | each {|l|
        let e = try { $l | from json } catch { null }
        if $e != null and $e.t == 'screen' {
          $e.html | .append 'pty.screen.IDX' --ttl last:1
        }
      } | ignore
  }
  duplex: DUPLEX
}"
    $closure
      | str replace --all "PTYBIN" $PTYZZZ
      | str replace --all "RUNCMD" $runcmd
      | str replace --all "PLAYBIN" $PLAYSTYLE
      | str replace --all "DUPLEX" $duplex
      | str replace --all "COLS" ($cols | into string)
      | str replace --all "ROWS" ($rows | into string)
      | str replace --all "IDX" ($it.index | into string)
      | .append $"xs.service.pty($it.index).create" --ttl last:1 | ignore
  }
}

# Position class + placeholder label for each face, derived from FACES so the
# layout stays a single source of truth. Fed to the template as `faces`.
const POS = [f-front f-right f-back f-left f-top f-bottom]
def face-views [] {
  $FACES | enumerate | each {|it|
    let label = if $it.item.kind == "term" { "nu terminal . type to me" } else if $it.item.kind == "aqua" { "asciiquarium" } else { $it.item.style }
    {i: $it.index, cls: ($POS | get $it.index), label: $label}
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

    # One stream for all six faces: follow every pty.screen.<n>, morph #grid-<n>.
    # The stored html already carries id=grid-<n>, so datastar targets the right
    # face with no per-face routing.
    (route {method: "GET", path: "/sse"} {|req ctx|
      .cat --follow
      | where topic =~ '^pty\.screen\.[0-5]$'
      | each {|f| .cas $f.hash | to datastar-patch-elements }
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
