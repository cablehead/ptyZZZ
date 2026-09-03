# examples/panes-replicated -- shared read side.
#
# One store per host, and the host's store is the only writer of anything a
# pty produces (pty-*.screen, pty-*.diff, xs.service.pty-*.*). Opening or
# killing a pane is an RPC to that store -- `xs append <host-addr> ...` --
# not a local append; see viewer.nu. That is one writer per log, same as
# ever: the host's store mints every id and orders every frame, an RPC
# with two clients is not two writers. Reads never touch a host directly:
# viewer replicates each one (`xs.replica.<name>.create`) and folds them
# with `interleave`, which is what lets this scale to N hosts, one screen,
# rather than being wired to exactly one.
#
# Layout (panes.layout/panes.patch/panes.seq) is local to viewer -- it is
# how *this* viewer arranges panes on screen, not something any host needs
# to know about.

use http-nu/datastar *

export const HERE = (path self | path dirname)
# static/ is shared with examples/panes/ by path reference: `.static` serves
# it as plain files, nothing about the split changes what's in it.
#
# templates/ is duplicated, not shared: `.mj compile`'s loader resolves
# `{% include %}` against the directory of the file it first compiled, and
# that resolution recurses through the whole include chain (page.html includes
# strip.html includes column.html includes pane.html, all against page.html's
# own directory) -- there is no way to point just column.html/pane.html
# elsewhere without also moving everything upstream of them in that chain.
# column.html and pane.html render a `{id, host}` record here where the
# original renders a bare pane id (a pane's host is part of viewer's layout
# now); page.html/strip.html/panel.html are unchanged copies.
export const PANES_DIR = ($HERE | path join ".." "panes")
export const TPL = ($HERE | path join "templates")
export const STATIC = ($PANES_DIR | path join "static")
export const FONTS = ($HERE | path join ".." ".." "static" "fonts")

# `.last`/`.cas` take core as a `--flag`; a null core means "the local store".
export def last-core [topic: string, core?: string] {
  if $core == null { .last $topic } else { .last $topic --core $core }
}

export def cas-core [hash: any, core?: string] {
  if $core == null { .cas $hash } else { .cas $hash --core $core }
}

def cat-local [from?: string] {
  if $from == null { .cat --follow } else { .cat --follow --from $from }
}

# Tags every frame with which host's replica it came from, so a fold reading
# N interleaved hosts still knows where to dereference a keyframe's hash --
# `interleave`'s output doesn't carry that on its own.
def cat-tagged [host: string, from?: string] {
  (if $from == null { .cat $host --follow } else { .cat $host --follow --from $from })
  | each {|f| $f | insert _host $host }
}

export def layout [] {
  let f = (.last "panes.layout")
  if $f == null { {columns: []} } else { $f.meta }
}

# viewer's `/sse`: an interleave of its own store (panes.layout/panes.patch)
# and one replica per host in `hosts` (pty-*.screen/diff, xs.service.pty-*.*).
# Per-frame handling is otherwise unchanged from a single-store fold; a
# keyframe just dereferences its hash against whichever host tagged it,
# rather than a single fixed core.
export def sse-response [hosts: list<string>] {
  let live = (try { layout | get columns | each {|c| $c.panes} | flatten | get id } catch { [] })
  let from_local = (try { .last "panes.layout" | get id } catch { null })

  let host_branches = ($hosts | each {|h|
    let seeds = ($live | each {|id| last-core $"pty-($id).screen" $h} | compact)
    # Replay from the oldest keyframe of a pane living on *this* host --
    # `--from` is inclusive, so that catches all of them. No live panes on
    # this host yet means nothing to seed; read from the start of its
    # replica.
    let from_h = (if ($seeds | is-empty) { null } else { $seeds | get id | sort | first })
    {|| cat-tagged $h $from_h }
  })

  interleave ...([{|| cat-local $from_local }] | append $host_branches)
  | generate {|f, s|
      let parts = ($f.topic | split row ".")

      # Liveness, so a closed pane's frames are not forwarded to a page that
      # no longer has an element for them.
      if ($f.topic | str starts-with "xs.service.pty-") {
        let id = ($parts | get 2 | str replace "pty-" "")
        let kind = ($parts | get 3)
        if $kind in ["create" "active"] {
          return {next: ($s | update live ($s.live | append $id | uniq))}
        }
        if $kind == "term" {
          return {next: ($s | update live ($s.live | where {|x| $x != $id}))}
        }
        return {next: $s}
      }

      if ($f.topic | str starts-with "pty-") {
        let id = ($parts | get 0 | str replace "pty-" "")
        let kind = ($parts | get 1)
        let held = (if ($id in ($s.sent | columns)) { $s.sent | get $id } else { null })
        if not ($id in $s.live) { return {next: $s} }

        if $kind == "screen" {
          # A keyframe is self-contained: adopt its seqno as the new basis.
          return {
            out: [(cas-core $f.hash $f._host | to datastar-patch-elements)]
            next: ($s | update sent ($s.sent | upsert $id ($f.meta.seqno)))
          }
        }

        if $kind == "diff" {
          let d = ($f.meta.body | from json)
          # Only forward a diff that chains to what we last sent. Anything
          # else means frames were missed; drop it and wait for the heal.
          if $held == null or $d.base != $held { return {next: $s} }
          return {
            out: ([
              (if ($d.patch | is-not-empty) { $d.patch | to datastar-patch-elements })
              (if ($d.append | is-not-empty) {
                $d.append | to datastar-patch-elements --mode append --selector $"#($d.target)"
              })
              (if ($d.trim | is-not-empty) {
                "" | to datastar-patch-elements --mode remove --selector ($d.trim | each {|t| $"#($t)"} | str join ",")
              })
            ] | compact)
            next: ($s | update sent ($s.sent | upsert $id $d.seqno))
          }
        }
        return {next: $s}
      }

      if $f.topic == "panes.patch" {
        let p = $f.meta
        if "signals" in ($p | columns) {
          return {out: [($p.signals | to datastar-patch-signals)], next: $s}
        }
        return {out: [(
          if $p.mode == "remove" {
            "" | to datastar-patch-elements --mode remove --selector $p.selector
          } else {
            $p.html | to datastar-patch-elements --mode $p.mode --selector $p.selector
          }
        )], next: $s}
      }

      {next: $s}
    } {live: $live, sent: {}}
  | flatten
  | to sse
  | metadata set --content-type "text/event-stream"
}
