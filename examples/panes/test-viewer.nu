# Proves phase A of the host/viewer split: a pane spawned on host shows up,
# live, in a read-only viewer that only replicates host's stream.
#
#   examples/panes/test-viewer.nu
#
# Needs `http-nu` on PATH (built against an xs with replica stores) and
# `ptyZZZ` built in this repo. Unlike test.nu (in-process `do $c $req`
# against one store), this spawns two real server processes -- replication
# is a client connecting to a remote store's socket, so there is no
# in-process shortcut for it.

use std/assert

const script_dir = path self | path dirname
const ROOT = ($script_dir | path join ".." "..")
const PTYZZZ = ($ROOT | path join "target" "release" "ptyZZZ" | path expand)

if not ($PTYZZZ | path exists) {
  error make {msg: $"panes: missing ($PTYZZZ) -- cargo build --release"}
}

let host_store = (mktemp -d)
let viewer_store = (mktemp -d)
let host_port = 39411
let viewer_port = 39412

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
  http-nu --dev --datastar --services --store $host_store $"127.0.0.1:($host_port)" (
    $script_dir | path join host.nu
  )
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

# host's ensure-panes spawns p1 at startup -- the viewer never called it, so
# seeing it at all means the replica caught host's own xs.service.*/screen
# frames, not local state.
assert (wait-sse-contains $viewer_port "grid-p1") "viewer /sse shows host's p1 keyframe via its replica"

# A frame appended to host's stream *after* the viewer already came up:
# open a second column on host, and confirm it reaches the viewer too.
curl -s -X POST $"http://127.0.0.1:($host_port)/pane/new-column" | ignore
assert (wait-sse-contains $viewer_port "grid-p2") "viewer /sse shows a pane opened on host after both were up"

job kill $host_job
job kill $viewer_job

print "test-viewer.nu: all assertions passed"
