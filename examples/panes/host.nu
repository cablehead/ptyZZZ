# examples/panes -- owns the ptys. No browser routes: viewer.nu is the only
# rendering surface, driven by a replica of this stream (see viewer.nu and
# examples/panes/README.md for the split).
#
# Run (needs store + services; PANES_VIEWER_ADDR points at viewer's store so
# this can replicate it back and react to its intents):
#   PANES_VIEWER_ADDR=./viewer-store \
#     http-nu --dev --services --store ./host-store 127.0.0.1:5111 examples/panes/host.nu

use http-nu/router *
use ./common.nu *

const PTYZZZ = ($HERE | path join ".." ".." "target" "release" "ptyZZZ" | path expand)

if not ($PTYZZZ | path exists) {
  error make {msg: $"panes: missing ($PTYZZZ) -- cargo build --release"}
}

# Name of the core this store opens for viewer's replicated frames --
# `xs.replica.(VIEWER_CORE).create`, read by the dispatcher below via `.cat
# (VIEWER_CORE)`.
const VIEWER_CORE = "viewer"

def register-replica [] {
  let addr = ($env.PANES_VIEWER_ADDR? | default "")
  if $addr == "" {
    error make {msg: "panes host: set PANES_VIEWER_ADDR to the viewer's --store path"}
  }
  let topic = $"xs.replica.($VIEWER_CORE).create"
  let last = (.last $topic)
  let current = if $last == null { null } else { $last.meta.addr? | default null }
  if $current != $addr {
    null | .append $topic --meta {addr: $addr} | ignore
  }
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

def spawn-pane [id: string] {
  let closure = $SVC
    | str replace --all "PTYBIN" $PTYZZZ
    | str replace --all "PFX" $"pty-($id)"
    | str replace --all "TARGET" $"grid-($id)"
  register-service $"xs.service.pty-($id).create" $closure
}

def kill-pane [id: string] {
  null | .append $"xs.service.pty-($id).term" | ignore
}

# Single writer per stream (ADR 0008): viewer owns panes.layout/panes.patch
# and writes its intents -- panes.spawn.<id>, panes.kill.<id>, pty-<id>.send
# -- to its own stream. Host never writes to viewer's stream and never reads
# viewer's layout; it only reacts to the intents, which is why this dispatcher
# only knows pane ids, not columns.
#
# Full history, not `--new`: on a host restart this replays every intent
# viewer ever wrote, in order. spawn-pane/kill-pane are already idempotent
# (register-service no-ops on an unchanged config; a term for an untracked
# name no-ops in the service dispatcher), so replaying is redundant-but-safe
# rather than wrong, and it's what gets a restarted host back to viewer's
# current layout without host needing its own copy of that layout.
# `job spawn`'s closure runs as a background thread in this same session, so
# it captures $VIEWER_CORE/spawn-pane/kill-pane/cas-core directly -- unlike
# $SVC above, which is a string re-parsed in an isolated per-service engine
# and so has to get its config baked in by string substitution instead.
job spawn --description "panes-dispatch" {
  interleave { .cat --follow } { .cat $VIEWER_CORE --follow } | each {|f|
    if ($f.topic | str starts-with "panes.spawn.") {
      spawn-pane ($f.topic | str replace "panes.spawn." "")
    } else if ($f.topic | str starts-with "panes.kill.") {
      kill-pane ($f.topic | str replace "panes.kill." "")
    } else if ($f.topic | str starts-with "pty-") and ($f.topic | str ends-with ".send") {
      # Content lives in viewer's CAS (pulled on demand from viewer's own
      # store, ADR 0008); relay it into ours so the pty's own duplex service
      # -- which only watches this store -- forwards it to the child's stdin.
      cas-core $f.hash $VIEWER_CORE | .append $f.topic --ttl ephemeral | ignore
    }
  } | ignore
} | ignore

if ($HTTP_NU.store? | default null) != null {
  register-replica
}

{|req|
  dispatch $req [
    (route {method: "GET", path: "/"} {|req ctx|
      "panes host: no browser routes here, see viewer.nu" | metadata set --content-type "text/plain"
    })
  ]
}
