# Proves the model end to end (task 5, xs-replica-panes.md, corrected after
# review -- see the branch history for why the first cut was wrong):
#
#   writes go to the remote: opening or killing a pane is `xs append
#     <host-addr> xs.service.pty-<id>.create/.term`, straight to whichever
#     host's store owns that pane. xs's own service machinery (--services)
#     starts the pty; there is no custom dispatcher anywhere in this example.
#   reads come from the replica: viewer never talks to a host directly for
#     rendering, only through `xs.replica.<name>.create`.
#   one viewer, many hosts: viewer's `/sse` interleaves a replica of every
#     configured host into one screen -- this test uses two, on purpose,
#     since a single host would not exercise that at all.
#
#   examples/panes-replicated/test-viewer.nu
#
# Needs `http-nu` (built against an xs with replica stores), `xs` (the
# CLI -- viewer shells out to `xs append` for the remote writes above), and
# `ptyZZZ`, all on PATH. Spawns three real server processes (`job spawn`,
# three stores) -- replication and remote append are both a client
# connecting to a remote store's socket, so there is no in-process shortcut
# for either the way test.nu's `do $c $req` is for single-store route logic.

use std/assert

const script_dir = path self | path dirname
const ROOT = ($script_dir | path join ".." "..")
const PTYZZZ = ($ROOT | path join "target" "release" "ptyZZZ" | path expand)

if not ($PTYZZZ | path exists) {
  error make {msg: $"panes: missing ($PTYZZZ) -- cargo build --release"}
}

let host_a_store = (mktemp -d)
let host_b_store = (mktemp -d)
let viewer_store = (mktemp -d)
let host_a_port = 39431
let host_b_port = 39432
let viewer_port = 39433

def wait-http [url: string] {
  mut ready = false
  for _ in 1..100 {
    if (do { curl -sf -o /dev/null $url } | complete | get exit_code) == 0 {
      $ready = true
      break
    }
    sleep 100ms
  }
  $ready
}

def sse-snapshot [port: int] {
  do { curl -sN --max-time 1 $"http://127.0.0.1:($port)/sse" } | complete | get stdout
}

def wait-sse-contains [port: int, marker: string] {
  mut seen = false
  for _ in 1..100 {
    if (sse-snapshot $port | str contains $marker) { $seen = true; break }
    sleep 100ms
  }
  $seen
}

let host_a_job = (job spawn --description "panes-test-host-a" {
  http-nu --dev --services --store $host_a_store $"127.0.0.1:($host_a_port)" (
    $script_dir | path join host.nu
  )
})
assert (wait-http $"http://127.0.0.1:($host_a_port)/") "host-a did not come up"

let host_b_job = (job spawn --description "panes-test-host-b" {
  http-nu --dev --services --store $host_b_store $"127.0.0.1:($host_b_port)" (
    $script_dir | path join host.nu
  )
})
assert (wait-http $"http://127.0.0.1:($host_b_port)/") "host-b did not come up"

let viewer_job = (job spawn --description "panes-test-viewer" {
  with-env {PANES_HOSTS: $"host-a=($host_a_store),host-b=($host_b_store)"} {
    http-nu --dev --datastar --store $viewer_store $"127.0.0.1:($viewer_port)" (
      $script_dir | path join viewer.nu
    )
  }
})
assert (wait-http $"http://127.0.0.1:($viewer_port)/") "viewer did not come up"

# ensure-workspace's remote append lands on host-a (the first configured
# host); the result comes back only via viewer's replica of host-a.
assert (wait-sse-contains $viewer_port "grid-p1") "viewer /sse shows p1's keyframe: remote append -> host-a's own service machinery -> replica -> render"

# A pane explicitly placed on host-b, through viewer's own route -- proves
# placement, not just the default-host path above.
let n1 = (curl -s -X POST $"http://127.0.0.1:($viewer_port)/pane/new-column?host=host-b" | from json)
assert ($n1.id == "p2") $"new-column id p2, got ($n1.id)"
assert (wait-sse-contains $viewer_port "grid-p2") "viewer /sse shows p2's keyframe from host-b: one interleave, two hosts, one screen"

# Input on a host-a pane and a host-b pane both round-trip -- proves /input
# looks up each pane's own host rather than assuming a single one.
let body_a = ({t: "input", b: "echo panes-ok-a\n"} | to json --raw)
curl -s -X POST -d $body_a $"http://127.0.0.1:($viewer_port)/input?pane=p1" | ignore
assert (wait-sse-contains $viewer_port "panes-ok-a") "viewer /sse shows echo from p1 on host-a after /input"

let body_b = ({t: "input", b: "echo panes-ok-b\n"} | to json --raw)
curl -s -X POST -d $body_b $"http://127.0.0.1:($viewer_port)/input?pane=p2" | ignore
assert (wait-sse-contains $viewer_port "panes-ok-b") "viewer /sse shows echo from p2 on host-b after /input"

job kill $host_a_job
job kill $host_b_job
job kill $viewer_job

print "test-viewer.nu: all assertions passed"
