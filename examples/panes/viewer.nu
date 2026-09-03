# examples/panes -- the browser-facing surface. Renders from a replica of
# host's stream and owns the layout (panes.layout/panes.patch/panes.seq),
# which it writes to its own stream. Runs WITHOUT --services and never calls
# spawn-pane/kill-pane against a real xs.service: those just append an
# intent (panes.spawn.<id>/panes.kill.<id>) here for host's dispatcher to
# act on. See host.nu and examples/panes/README.md for why: `xs.service.pty-
# <id>.create` carries the closure that spawns a ptyZZZ process, and a
# viewer that replicated those frames *and* ran --services would spawn its
# own pty -- a second, divergent terminal instead of showing the remote one.
# This design keeps that gate structural: viewer's spawn-pane never touches
# xs.service.* at all, so there is nothing here `--services` could act on.
#
# Run (needs the host already serving a store at $PANES_HOST_ADDR):
#   PANES_HOST_ADDR=./host-store \
#     http-nu --dev --datastar --store ./viewer-store 127.0.0.1:5112 examples/panes/viewer.nu

use http-nu/datastar *
use http-nu/router *
use ./common.nu *

# Name of the core this store opens for host's replicated frames --
# `xs.replica.(HOST_CORE).create`, read everywhere below via `--core`/`.cat
# (HOST_CORE)`.
const HOST_CORE = "host"

def register-replica [] {
  let addr = ($env.PANES_HOST_ADDR? | default "")
  if $addr == "" {
    error make {msg: "panes viewer: set PANES_HOST_ADDR to the host's --store path"}
  }
  let topic = $"xs.replica.($HOST_CORE).create"
  let last = (.last $topic)
  let current = if $last == null { null } else { $last.meta.addr? | default null }
  if $current != $addr {
    null | .append $topic --meta {addr: $addr} | ignore
  }
}

# Intents for host's dispatcher (host.nu) -- not a real xs.service, on
# purpose: this store never runs --services, so xs.service.* here would just
# be inert data even if we wrote it directly. Host is the one that turns a
# pane id into an actual pty.
def spawn-pane [id: string] {
  null | .append $"panes.spawn.($id)" | ignore
}

def kill-pane [id: string] {
  null | .append $"panes.kill.($id)" | ignore
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
    $c.item.panes | enumerate | where {|p| $p.item == $pid} | each {|p|
      {ci: $c.index, pi: $p.index, col_id: $c.item.id}
    }
  } | flatten
  if ($hits | is-empty) { null } else { $hits | first }
}

def emit-patch [mode: string, selector: string, html: string] {
  null | .append "panes.patch" --ttl ephemeral --meta {mode: $mode, selector: $selector, html: $html} | ignore
}

let page_tpl = .mj compile ($TPL | path join "page.html")
let pane_tpl = .mj compile ($TPL | path join "pane.html")
let column_tpl = .mj compile ($TPL | path join "column.html")

def html-pane [id: string] {
  {pane: $id} | .mj render $pane_tpl
}

def html-column [col: record] {
  {col: $col} | .mj render $column_tpl
}

# Only the empty-workspace bootstrap: an already-populated layout is durable
# in viewer's own stream, and host's dispatcher replays every spawn intent
# viewer ever wrote on its own restart (see host.nu) -- so there is no
# existing-panes loop to redo here the way host.nu's old ensure-panes had.
# Do not gate on $HTTP_NU.services: `http-nu eval --services` starts
# dispatchers but leaves that const false, so a services check would skip
# the seed and tests would see an empty page.
def ensure-workspace [] {
  mut l = layout
  if ($l.columns | is-empty) {
    let n = (next-n)
    let pid = $"p($n)"
    let cid = $"c($n)"
    spawn-pane $pid
    $l = {columns: [{id: $cid, panes: [$pid]}]}
    save-layout $l
  }
  $l
}

if ($HTTP_NU.store? | default null) != null {
  register-replica
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

    (route {method: "GET", path: "/sse"} {|req ctx| sse-response $HOST_CORE })

    (route {method: "POST", path: "/input"} {|req ctx|
      let body = $in | into string | str trim --right --char "\n"
      let pane = $req.query?.pane? | default ""
      if $pane == "" {
        "missing pane" | metadata set { merge {'http.response': {status: 400}} }
      } else {
        # ephemeral: a keystroke is relayed by host's dispatcher the moment
        # it is written and never read again, so persisting it only grows
        # the journal every replica replays.
        $body + "\n" | .append $"pty-($pane).send" --ttl ephemeral | ignore
        null | metadata set { merge {'http.response': {status: 204}} }
      }
    })

    (route {method: "POST", path: "/pane/new-column"} {|req ctx|
      let after = $req.query?.after? | default ""
      let l = layout
      let n = (next-n)
      let pid = $"p($n)"
      let cid = $"c($n)"
      let col = {id: $cid, panes: [$pid]}
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
      # dropped as PatchElementsNoTargetsFound). Both still go out ordered:
      # the patch is local (this store), the keyframe arrives later via
      # host's own pty spawn plus the replica, and interleave preserves
      # per-stream order even though it makes no promise across streams --
      # which is fine here, since nothing downstream of the patch depends on
      # a cross-stream ordering, only on the patch existing before the
      # keyframe eventually shows up.
      spawn-pane $pid
      {id: $pid, col: $cid} | to json | metadata set --content-type "application/json"
    })

    (route {method: "POST", path: "/pane/split"} {|req ctx|
      let pane = $req.query?.pane? | default ""
      let l = layout
      let loc = locate $l $pane
      if $loc == null {
        "unknown pane" | metadata set { merge {'http.response': {status: 400}} }
      } else {
        let pid = $"p(next-n)"
        let cols = $l.columns | enumerate | each {|c|
          if $c.index == $loc.ci {
            $c.item | update panes ($c.item.panes | append $pid)
          } else { $c.item }
        }
        save-layout {columns: $cols}
        emit-patch "append" $"#col-($loc.col_id)" (html-pane $pid)
        spawn-pane $pid  # after the patch -- see /pane/new-column
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
        kill-pane $pane
        let col = $l.columns | get $loc.ci
        let leftover = $col.panes | where {|p| $p != $pane }
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
          $leftover | get $i
        } else if ($cols | is-empty) {
          null
        } else {
          let i = if $loc.ci < ($cols | length) { $loc.ci } else { ($cols | length) - 1 }
          $cols | get $i | get panes | last
        }
        {id: $pane, next: $next} | to json | metadata set --content-type "application/json"
      }
    })

    (route {method: "GET", path-matches: "/static/:file"} {|req ctx|
      .static ($HERE | path join "static") $"/($ctx.file)"
    })

    (route {method: "GET", path-matches: "/fonts/:file"} {|req ctx|
      .static $FONTS $"/($ctx.file)"
    })
  ]
}
