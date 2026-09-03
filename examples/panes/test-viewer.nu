# Proves the host/viewer split end to end (task 5, xs-replica-panes.md):
#
#   phase A -- a pane spawned on host shows up, live, in a read-only viewer
#              that only replicates host's stream.
#   phase B -- viewer's own routes (/pane/new-column, /input) are the only
#              way to drive it: viewer never touches xs.service.* itself, it
#              appends an intent to its own stream, host's dispatcher
#              (interleaving its own store with a replica of viewer's) spawns
#              the real pty and relays input to it, and the result comes back
#              to viewer only via its replica of host.
#
#   examples/panes/test-viewer.nu
#
# Needs `http-nu` on PATH (built against an xs with replica stores) and
# `ptyZZZ` built in this repo. Spawns two real server processes (`job
# spawn`, two stores) -- replication is a client connecting to a remote
# store's socket, so there is no in-process shortcut for it the way
# test.nu's `do $c $req` is for single-store route logic.

use std/assert

const script_dir = path self | path dirname
const ROOT = ($script_dir | path join ".." "..")
const PTYZZZ = ($ROOT | path join "target" "release" "ptyZZZ" | path expand)

if not ($PTYZZZ | path exists) {
  error make {msg: $"panes: missing ($PTYZZZ) -- cargo build --release"}
}

let host_store = (mktemp -d)
let viewer_store = (mktemp -d)
let host_port = 39421
let viewer_port = 39422

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

let host_job = (job spawn --description "panes-test-host" {
  with-env {PANES_VIEWER_ADDR: $viewer_store} {
    http-nu --dev --services --store $host_store $"127.0.0.1:($host_port)" (
      $script_dir | path join host.nu
    )
  }
})
assert (wait-http $"http://127.0.0.1:($host_port)/") "host did not come up"

let viewer_job = (job spawn --description "panes-test-viewer" {
  with-env {PANES_HOST_ADDR: $host_store} {
    http-nu --dev --datastar --store $viewer_store $"127.0.0.1:($viewer_port)" (
      $script_dir | path join viewer.nu
    )
  }
})
assert (wait-http $"http://127.0.0.1:($viewer_port)/") "viewer did not come up"

# --- phase A: viewer's ensure-workspace intent (panes.spawn.p1) reaches
# host's dispatcher, which spawns the real pty; the result comes back only
# via viewer's replica of host.
assert (wait-sse-contains $viewer_port "grid-p1") "viewer /sse shows p1's keyframe: intent -> host dispatch -> spawn -> replica -> render"

# --- phase B: drive a new pane through viewer's own route, not host's --
# host has no /pane/* route anymore (see host.nu).
let n1 = (curl -s -X POST $"http://127.0.0.1:($viewer_port)/pane/new-column" | from json)
assert ($n1.id == "p2") $"new-column id p2, got ($n1.id)"
assert (wait-sse-contains $viewer_port "grid-p2") "viewer /sse shows p2's keyframe after driving new-column through viewer"

# --- phase B: input through viewer's own route reaches the real pty on
# host and its echo comes back through the replica.
let body = ({t: "input", b: "echo panes-ok\n"} | to json --raw)
curl -s -X POST -d $body $"http://127.0.0.1:($viewer_port)/input?pane=p1" | ignore
assert (wait-sse-contains $viewer_port "panes-ok") "viewer /sse shows echo panes-ok after /input on viewer: intent -> host relay -> pty stdin -> replica"

job kill $host_job
job kill $viewer_job

print "test-viewer.nu: all assertions passed"
