use std/assert

# Endpoint tests: `source serve.nu; do $c $req`.
#   http-nu eval --datastar --store /tmp/through-test --services examples/through/test.nu

const script_dir = path self | path dirname
let c = source ($script_dir | path join serve.nu)

let page = (do $c {method: "GET", path: "/", headers: {}, query: {}} | into string)
assert ($page | str contains "<!doctype html") "GET / is html"
assert ($page | str contains "grid-p1") "GET / seeds grid-p1"
assert ($page | str contains "id=\"toggle\"") "GET / has the rotate toggle"
assert ($page | str contains "/ping") "GET / wires the rtt ping"
assert ($page | str contains "/static/through.js") "GET / loads static js"
assert ($page | str contains "/datastar@1.0.2.js") "GET / loads datastar"
assert ($page | str contains "wezterm") "GET / has the wezterm box"
assert ($page | str contains 'href="/shots"') "GET / links to the local shots gallery"
assert (not ($page | str contains "actions-panel")) "no panes command panel"
assert (not ($page | str contains "/pane/new-column")) "no new-column"
assert (not ($page | str contains "/pane/split")) "no split"

mut ready = false
for _ in 1..50 {
  sleep 100ms
  if (.last "pty-p1.screen" | is-not-empty) { $ready = true; break }
}
assert $ready "pty-p1 emitted a screen keyframe"
(({t: "input", b: "echo through-ok\n"} | to json --raw) + "\n") | do $c {method: "POST", path: "/input", headers: {}, query: {pane: "p1"}} | ignore
mut seen = false
for _ in 1..50 {
  sleep 100ms
  let f = .last "pty-p1.screen"
  if $f == null { continue }
  let html = .cas $f.hash
  if ($html | str contains "through-ok") { $seen = true; break }
}
assert $seen "pty-p1 screen shows echo through-ok after /input"

# A join must not wait out the healing interval. Once the replay is done the
# fold asks for a keyframe, and that keyframe lands on the same stream. So
# with no input in between, one join sees two grid-p1 morphs: the stored seed,
# then the fresh one. Two sse events are six lines; if the second never comes
# this blocks, which is how the failure shows.
let join = (null | do $c {method: "GET", path: "/sse", headers: {}, query: {}} | lines | first 6 | str join "\n")
assert (($join | split row 'id="grid-p1"' | length) == 3) "a join replays the seed and then gets a requested keyframe"

let gallery = (do $c {method: "GET", path: "/shots", headers: {}, query: {}} | into string)
assert ($gallery | str contains "through captures") "GET /shots is the local gallery"
