# examples/panes -- niri-style ptyZZZ columns you can open, split, and close.
#
# Glue only: layout in the store, one duplex pty service per pane, routes.
# Markup is minijinja under templates/; CSS/JS under static/.
#
# Run (needs store + services + datastar):
#   http-nu --dev --datastar --services --store ./store 127.0.0.1:5111 examples/panes/serve.nu
#   # add --tls <pem> to serve https, so browsers ask for brotli on /sse
#
# Layout: horizontal strip of 100ch columns. A column can stack several panes.
# Close kills the pty. A restart respawns nu in each surviving slot.

use http-nu/datastar *
use http-nu/router *

const HERE = (path self | path dirname)
const TPL = ($HERE | path join "templates")
const PTYZZZ = ($HERE | path join ".." ".." "target" "release" "ptyZZZ" | path expand)
const FONTS = ($HERE | path join ".." ".." "static" "fonts")

if not ($PTYZZZ | path exists) {
  error make {msg: $"panes: missing ($PTYZZZ) -- cargo build --release"}
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
          "screen" => ( $e.html | .append "PFX.screen" --ttl last:1 )
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

def layout [] {
  let f = .last "panes.layout"
  if $f == null { {columns: []} } else { $f.meta }
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

# Seed and respawn whenever a store is open. Do not gate on $HTTP_NU.services:
# `http-nu eval --services` starts dispatchers but leaves that const false, so
# a services check would skip the seed and tests would see an empty page.
def ensure-panes [] {
  mut l = layout
  if ($l.columns | is-empty) {
    let n = (next-n)
    let pid = $"p($n)"
    let cid = $"c($n)"
    spawn-pane $pid
    $l = {columns: [{id: $cid, panes: [$pid]}]}
    save-layout $l
  } else {
    for c in $l.columns {
      for pid in $c.panes { spawn-pane $pid }
    }
  }
  $l
}

if ($HTTP_NU.store? | default null) != null {
  ensure-panes | ignore
}

{|req|
  dispatch $req [
    (route {method: "GET", path: "/"} {|req ctx|
      let l = try { layout } catch { {columns: []} }
      {datastar: $DATASTAR_JS_PATH, columns: $l.columns}
      | .mj render $page_tpl
      | metadata set --content-type "text/html"
    })

    # Play the stream from the beginning and materialize the current view.
    # Liveness folds from the service lifecycle (create/active up, term down), so
    # a closed pane's keyframe is folded over rather than deleted -- nothing is
    # removed from the store. Replay emits nothing and reads no CAS: the fold
    # carries hashes only. xs.threshold marks the end of history, and there the
    # view goes out once, one keyframe per live pane. After that frames stream
    # through as they arrive.
    (route {method: "GET", path: "/sse"} {|req ctx|
      .cat --follow
      | generate {|f, s = {live: [], screens: {}, past: false}|
          let parts = ($f.topic | split row ".")

          if $f.topic == "xs.threshold" {
            let cols = ($s.screens | columns)
            let patches = ($s.live | each {|id|
              if ($id in $cols) { .cas ($s.screens | get $id) | to datastar-patch-elements }
            } | compact)
            return {out: $patches, next: ($s | update past true)}
          }

          if ($f.topic | str starts-with "xs.service.pty-") {
            let id = ($parts | get 2 | str replace "pty-" "")
            let kind = ($parts | get 3)
            if $kind in ["create" "active"] {
              return {next: ($s | update live ($s.live | append $id | uniq))}
            }
            if $kind == "term" {
              let cols = ($s.screens | columns)
              return {next: ($s
                | update live ($s.live | where {|x| $x != $id})
                | update screens (if ($id in $cols) { $s.screens | reject $id } else { $s.screens }))}
            }
            return {next: $s}
          }

          if ($f.topic | str starts-with "pty-") {
            let id = ($parts | get 0 | str replace "pty-" "")
            let kind = ($parts | get 1)
            if not $s.past {
              # Carry the hash, do not read it. Only what survives the fold is
              # ever fetched, and only once, at the threshold.
              if $kind == "screen" {
                return {next: ($s | update screens ($s.screens | upsert $id $f.hash))}
              }
              return {next: $s}
            }
            if not ($id in $s.live) { return {next: $s} }
            if $kind == "screen" {
              return {out: [(.cas $f.hash | to datastar-patch-elements)], next: $s}
            }
            if $kind == "diff" {
              let d = $f.meta.body | from json
              return {out: ([
                (if ($d.patch | is-not-empty) { $d.patch | to datastar-patch-elements })
                (if ($d.append | is-not-empty) {
                  $d.append | to datastar-patch-elements --mode append --selector $"#($d.target)"
                })
                (if ($d.trim | is-not-empty) {
                  "" | to datastar-patch-elements --mode remove --selector ($d.trim | each {|t| $"#($t)"} | str join ",")
                })
              ] | compact), next: $s}
            }
            return {next: $s}
          }

          if $f.topic == "panes.patch" and $s.past {
            let p = $f.meta
            return {out: [(
              if $p.mode == "remove" {
                "" | to datastar-patch-elements --mode remove --selector $p.selector
              } else {
                $p.html | to datastar-patch-elements --mode $p.mode --selector $p.selector
              }
            )], next: $s}
          }

          {next: $s}
        }
      | flatten
      | to sse
      | metadata set --content-type "text/event-stream"
    })

    (route {method: "POST", path: "/input"} {|req ctx|
      let body = $in | into string | str trim --right --char "\n"
      let pane = $req.query?.pane? | default ""
      if $pane == "" {
        "missing pane" | metadata set { merge {'http.response': {status: 400}} }
      } else {
        # ephemeral: a keystroke is read by the duplex service the moment it is
        # written and never read again, so persisting it only grows the journal
        # that every new /sse connection replays. These were 78% of it.
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
      # After the patch, never before. Both go out on one ordered .cat --follow,
      # and a pty emits its first keyframe within milliseconds of spawning. Spawn
      # first and that keyframe reaches the client ahead of the element it targets:
      # datastar drops it as PatchElementsNoTargetsFound, and the pane then gets
      # only row-level diffs against a grid that never got its keyframe. It stays
      # blank until the 5s healing keyframe, or until a keypress damages enough
      # rows to force one.
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
