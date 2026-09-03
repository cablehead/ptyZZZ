# examples/panes-replicated -- the browser-facing surface, and the only
# writer of its own layout (panes.layout/panes.patch/panes.seq -- how *this*
# viewer arranges panes on screen, nothing any host needs to know).
#
# Opening or killing a pane is a remote append: `xs.service.pty-<id>.create`/
# `.term` land directly on the owning host's store (`remote-append`, below),
# an RPC to the store that mints every id and orders every frame there --
# one writer per log, with two clients, not two writers. That is what
# starts and stops the pty: xs's own service machinery, watching that
# host's own store, needs nothing from this file once the frame lands.
# Reads never touch a host directly either: viewer replicates each one
# (`xs.replica.<name>.create`) and folds all of them with `interleave`
# (common.nu's `sse-response`), which is the point of doing it this way --
# adding a host is adding a replica to fold, not touching this file's shape.
#
# Never runs --services, on purpose: this store owns no pty-spawning
# closures (those are remote-appended to a host, never written here), so
# there is nothing on it `--services` could act on even by mistake.
#
# Run (needs one or more hosts already serving a store -- see host.nu --
# named in PANES_HOSTS as `name=addr` pairs, comma-separated):
#   PANES_HOSTS=host-a=./host-a-store \
#     http-nu --dev --datastar --store ./viewer-store 127.0.0.1:5112 examples/panes-replicated/viewer.nu

use http-nu/datastar *
use http-nu/router *
use ./common.nu *

def parse-hosts [] {
  let raw = ($env.PANES_HOSTS? | default "")
  if $raw == "" {
    error make {msg: "panes viewer: set PANES_HOSTS to a comma-separated name=addr list, e.g. host-a=./host-a-store"}
  }
  $raw | split row "," | each {|pair|
    let parts = ($pair | split row "=")
    {name: ($parts | get 0), addr: ($parts | get 1)}
  }
}

let HOSTS = (parse-hosts)
let HOST_NAMES = ($HOSTS | get name)

def host-addr [name: string] {
  ($HOSTS | where name == $name | get 0).addr
}

def register-replicas [] {
  for h in $HOSTS {
    let topic = $"xs.replica.($h.name).create"
    let last = (.last $topic)
    let current = if $last == null { null } else { $last.meta.addr? | default null }
    if $current != $h.addr {
      null | .append $topic --meta {addr: $h.addr} | ignore
    }
  }
}

const PTYZZZ = ($HERE | path join ".." ".." "target" "release" "ptyZZZ" | path expand)

if not ($PTYZZZ | path exists) {
  error make {msg: $"panes: missing ($PTYZZZ) -- cargo build --release"}
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

# The RPC: appends piped content to topic on a *remote* store, via the `xs`
# CLI rather than the `.append` builtin (which only ever targets this
# engine's own store). Same-VM only for now, same as the rest of this
# example -- a remote host reachable only over pai-sho needs `xs`'s own
# transport story, not this.
#
# A host being briefly unreachable is a normal, expected condition in a
# distributed system, not a bug -- `complete` swallows a nonzero exit
# instead of raising, so a route (e.g. opening a column) still returns
# successfully with the layout change recorded even if the remote spawn it
# also asked for did not land. The alternative -- raising and 500ing the
# whole route -- would make one flaky host bring down layout edits for
# panes that live on every other host too.
def remote-append [addr: string, topic: string, --ttl: string] {
  let ttl_args = if $ttl != null { ["--ttl" $ttl] } else { [] }
  let result = ($in | ^xs append $addr $topic ...$ttl_args | complete)
  if $result.exit_code != 0 {
    print --stderr $"panes viewer: remote append to ($addr) ($topic) failed: ($result.stderr | str trim)"
  }
}

# Idempotent the same way the old same-process register-service was, just
# sourced from the replica instead of a local read: re-appending an
# unchanged create is a harmless no-op downstream (xs's service dispatcher
# already ignores a duplicate/replayed create for a running name), but
# skipping it avoids minting a new frame id for literally the same config
# on every restart.
def register-remote-service [host: string, topic: string, config: string] {
  let last = (last-core $topic $host)
  let current = if ($last | is-empty) { null } else { cas-core $last.hash $host }
  if $current != $config {
    $config | remote-append (host-addr $host) $topic
  }
}

def spawn-pane [host: string, id: string] {
  let closure = $SVC
    | str replace --all "PTYBIN" $PTYZZZ
    | str replace --all "PFX" $"pty-($id)"
    | str replace --all "TARGET" $"grid-($id)"
  register-remote-service $host $"xs.service.pty-($id).create" $closure
}

def kill-pane [host: string, id: string] {
  "" | remote-append (host-addr $host) $"xs.service.pty-($id).term"
}

def send-input [host: string, id: string, body: string] {
  $body | remote-append (host-addr $host) $"pty-($id).send" --ttl ephemeral
}

def save-layout [l: record] {
  null | .append "panes.layout" --ttl last:1 --meta $l | ignore
}

def next-n [] {
  let f = .last "panes.seq"
  let n = (if $f == null { 0 } else { $f.meta.n }) + 1
  null | .append "panes.seq" --ttl last:1 --meta {n: $n} | ignore
  $n
}

def locate [l: record, pid: string] {
  let hits = $l.columns | enumerate | each {|c|
    $c.item.panes | enumerate | where {|p| $p.item.id == $pid} | each {|p|
      {ci: $c.index, pi: $p.index, col_id: $c.item.id, host: $p.item.host}
    }
  } | flatten
  if ($hits | is-empty) { null } else { $hits | first }
}

def emit-patch [mode: string, selector: string, html: string] {
  null | .append "panes.patch" --ttl ephemeral --meta {mode: $mode, selector: $selector, html: $html} | ignore
}

# Signals ride the same topic as elements: both are "a datastar event the fold
# should forward", so neither the topic list nor the fold grows a second shape.
def emit-signals [signals: record] {
  null | .append "panes.patch" --ttl ephemeral --meta {signals: ($signals | to json --raw)} | ignore
}

let page_tpl = .mj compile ($TPL | path join "page.html")
let pane_tpl = .mj compile ($TPL | path join "pane.html")
let column_tpl = .mj compile ($TPL | path join "column.html")

def html-pane [id: string, host: string] {
  {pane: {id: $id, host: $host}} | .mj render $pane_tpl
}

def html-column [col: record] {
  {col: $col} | .mj render $column_tpl
}

# Only the empty-workspace bootstrap: an already-populated layout is durable
# in viewer's own stream, so there is no existing-panes loop to redo here --
# unlike the pre-split ensure-panes, this never needs to re-spawn anything,
# because the ptys it spawned live in a host's store, not this process, and
# outlive a viewer restart on their own.
# Do not gate on $HTTP_NU.services: `http-nu eval --services` starts
# dispatchers but leaves that const false, so a services check would skip
# the seed and tests would see an empty page.
def ensure-workspace [] {
  mut l = layout
  if ($l.columns | is-empty) {
    let host = ($HOSTS | get 0).name
    let n = (next-n)
    let pid = $"p($n)"
    let cid = $"c($n)"
    spawn-pane $host $pid
    $l = {columns: [{id: $cid, panes: [{id: $pid, host: $host}]}]}
    save-layout $l
  }
  $l
}

if ($HTTP_NU.store? | default null) != null {
  register-replicas
  ensure-workspace | ignore
}

{|req|
  dispatch $req [
    (route {method: "GET", path: "/"} {|req ctx|
      let l = try { layout } catch { {columns: []} }
      {datastar: $DATASTAR_JS_PATH, columns: $l.columns}
      | .mj render $page_tpl
      | metadata set --content-type "text/html"
    })

    (route {method: "GET", path: "/sse"} {|req ctx| sse-response $HOST_NAMES })

    # A pong is just an element patch, so it rides the pathway panes.patch
    # already owns: no new frame type, no new branch in the /sse fold. Echoing
    # the id back rather than a timestamp keeps clock skew out of it -- the
    # client knows when it sent. This matters more for viewer than it did for
    # a single unsplit process: viewer renders through a replica now, one more
    # hop past viewer's own store for a stall to hide in, so knowing the
    # round trip to *viewer* (not to a host) is what a stalled-stream signal
    # should mean.
    (route {method: "POST", path: "/ping"} {|req ctx|
      # Datastar posts every signal as the body. Echo the client's own send
      # time back over the SSE stream rather than in this response: the round
      # trip we care about is the stream's, and the write keeps it warm.
      let signals = (try { $in | into string | from json } catch { {} })
      emit-signals {pong: ($signals.ping? | default 0)}
      null | metadata set { merge {'http.response': {status: 204}} }
    })

    (route {method: "POST", path: "/input"} {|req ctx|
      let body = $in | into string | str trim --right --char "\n"
      let pane = $req.query?.pane? | default ""
      if $pane == "" {
        "missing pane" | metadata set { merge {'http.response': {status: 400}} }
      } else {
        let loc = locate (layout) $pane
        if $loc == null {
          "unknown pane" | metadata set { merge {'http.response': {status: 400}} }
        } else {
          send-input $loc.host $pane ($body + "\n")
          null | metadata set { merge {'http.response': {status: 204}} }
        }
      }
    })

    (route {method: "POST", path: "/pane/new-column"} {|req ctx|
      let after = $req.query?.after? | default ""
      # Which host gets the new pane. Defaults to the first configured host
      # so a single-host setup needs no query param; a multi-host viewer
      # picks with ?host=name (no placement UI yet -- see README).
      let host = $req.query?.host? | default (($HOSTS | get 0).name)
      let l = layout
      let n = (next-n)
      let pid = $"p($n)"
      let cid = $"c($n)"
      let col = {id: $cid, panes: [{id: $pid, host: $host}]}
      let loc = if $after == "" { null } else { locate $l $after }
      let cols = if $loc == null {
        $l.columns | append $col
      } else {
        let i = $loc.ci + 1
        ($l.columns | take $i) | append $col | append ($l.columns | skip $i)
      }
      save-layout {columns: $cols}
      if ($l.columns | is-empty) {
        emit-patch "append" "#strip" (html-column $col)
      } else if $loc == null {
        emit-patch "append" "#strip" (html-column $col)
      } else {
        emit-patch "after" $"#col-($loc.col_id)" (html-column $col)
      }
      # After the patch, never before -- see the original serve.nu history
      # for why (a keyframe racing ahead of the element it targets gets
      # dropped as PatchElementsNoTargetsFound). The patch is local (this
      # store); the keyframe comes later from the host's own pty plus its
      # replica -- interleave preserves per-stream order, which is all this
      # needs, since nothing here depends on an ordering across streams.
      spawn-pane $host $pid
      {id: $pid, col: $cid} | to json | metadata set --content-type "application/json"
    })

    (route {method: "POST", path: "/pane/split"} {|req ctx|
      let pane = $req.query?.pane? | default ""
      let l = layout
      let loc = locate $l $pane
      if $loc == null {
        "unknown pane" | metadata set { merge {'http.response': {status: 400}} }
      } else {
        # Same host as the pane being split -- a column stays co-located on
        # one host by default.
        let host = $loc.host
        let pid = $"p(next-n)"
        let cols = $l.columns | enumerate | each {|c|
          if $c.index == $loc.ci {
            $c.item | update panes ($c.item.panes | append {id: $pid, host: $host})
          } else { $c.item }
        }
        save-layout {columns: $cols}
        emit-patch "append" $"#col-($loc.col_id)" (html-pane $pid $host)
        spawn-pane $host $pid  # after the patch -- see /pane/new-column
        {id: $pid, col: $loc.col_id} | to json | metadata set --content-type "application/json"
      }
    })

    (route {method: "POST", path: "/pane/close"} {|req ctx|
      let pane = $req.query?.pane? | default ""
      let l = layout
      let loc = locate $l $pane
      if $loc == null {
        "unknown pane" | metadata set { merge {'http.response': {status: 400}} }
      } else {
        kill-pane $loc.host $pane
        let col = $l.columns | get $loc.ci
        let leftover = $col.panes | where {|p| $p.id != $pane }
        let cols = if ($leftover | is-empty) {
          $l.columns | where {|c| $c.id != $col.id }
        } else {
          $l.columns | enumerate | each {|c|
            if $c.index == $loc.ci { $c.item | update panes $leftover } else { $c.item }
          }
        }
        save-layout {columns: $cols}
        if ($leftover | is-empty) {
          emit-patch "remove" $"#col-($col.id)" ""
        } else {
          emit-patch "remove" $"#pane-($pane)" ""
        }
        let next = if not ($leftover | is-empty) {
          let i = if $loc.pi < ($leftover | length) { $loc.pi } else { ($leftover | length) - 1 }
          ($leftover | get $i).id
        } else if ($cols | is-empty) {
          null
        } else {
          let i = if $loc.ci < ($cols | length) { $loc.ci } else { ($cols | length) - 1 }
          ($cols | get $i | get panes | last).id
        }
        {id: $pane, next: $next} | to json | metadata set --content-type "application/json"
      }
    })

    (route {method: "GET", path-matches: "/static/:file"} {|req ctx|
      .static $STATIC $"/($ctx.file)"
    })

    (route {method: "GET", path-matches: "/fonts/:file"} {|req ctx|
      .static $FONTS $"/($ctx.file)"
    })
  ]
}
