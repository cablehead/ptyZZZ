# examples/panes -- shared read side between host.nu and viewer.nu.
#
# host writes pty-*.screen, pty-*.diff, xs.service.pty-*.* -- the content a
# pty produces. viewer writes panes.layout, panes.patch, panes.seq -- the UI
# state describing where panes live on screen. viewer is the only browser
# surface (see viewer.nu), so its `/sse` folds an interleave of its own store
# and a replica of host's; layout is always local, since only viewer ever
# writes it.

use http-nu/datastar *

export const HERE = (path self | path dirname)
export const TPL = ($HERE | path join "templates")
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

def cat-from [core: string, from?: string] {
  if $from == null { .cat $core --follow } else { .cat $core --follow --from $from }
}

export def layout [] {
  let f = (.last "panes.layout")
  if $f == null { {columns: []} } else { $f.meta }
}

# viewer's `/sse`: an interleave of its own store (panes.layout/panes.patch,
# the only things it writes) and `host_core`, a replica of host (pty-*.screen/
# diff, xs.service.pty-*.* -- the only things host writes). Per-frame handling
# is unchanged from the single-store version; only the read is now two
# streams merged instead of one, since content that used to share a journal
# now lives on either side of the host/viewer split.
export def sse-response [host_core: string] {
  let live = (try { layout | get columns | each {|c| $c.panes} | flatten } catch { [] })
  let seeds = ($live | each {|id| last-core $"pty-($id).screen" $host_core} | compact)
  # Host side: replay from the oldest live keyframe, same reasoning as
  # before -- `--from` is inclusive, so every live pane's keyframe is caught.
  # No live panes yet (a fresh workspace) means nothing to seed; read from
  # the start of the (empty, so far) replica.
  let from_host = (if ($seeds | is-empty) { null } else { $seeds | get id | sort | first })
  # Local side: replay from the current layout snapshot's own id. Every
  # layout mutation route saves layout *before* emitting its patch, so this
  # still catches a patch from the same operation that produced the layout
  # we just read (patch id > layout id); anything older is already reflected
  # in that snapshot, so no need to replay it again.
  let from_local = (try { .last "panes.layout" | get id } catch { null })

  interleave { cat-local $from_local } { cat-from $host_core $from_host }
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
        if not ($id in $s.live) { return {next: $s} }

        if $kind == "screen" {
          # A keyframe is self-contained: adopt its seqno as the new basis.
          # Screen content always lives on host's side, whichever branch of
          # the interleave this frame arrived on.
          return {
            out: [(cas-core $f.hash $host_core | to datastar-patch-elements)]
            next: ($s | update sent ($s.sent | upsert $id ($f.meta.seqno)))
          }
        }

        if $kind == "diff" {
          let d = ($f.meta.body | from json)
          let held = (if ($id in ($s.sent | columns)) { $s.sent | get $id } else { null })
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
