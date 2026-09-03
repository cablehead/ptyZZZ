# examples/panes -- shared read side between host.nu and viewer.nu.
#
# Frames carry everything the fold needs, so the same fold works whether it
# reads the local store or a replica: only which store gets threaded through
# as an optional `core` (the name a `xs.replica.<core>.create` opened). Write
# helpers (spawn/kill a pane, layout mutation) stay local to whichever file
# owns that stream -- this module is read-only, on purpose.

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

def cat-core [core?: string, --from: string] {
  if $core == null { .cat --follow --from $from } else { .cat $core --follow --from $from }
}

export def layout [core?: string] {
  let f = (last-core "panes.layout" $core)
  if $f == null { {columns: []} } else { $f.meta }
}

# `/sse`: a pure fold over one ordered `.cat --follow`, reading `$core`
# (null for the local store, a replica name otherwise). See host.nu's git
# history for the shape of the reasoning; unchanged here except every read
# ( `.last`, `.cas` ) is core-aware so a viewer folding a replica dereferences
# keyframes from the replica's own on-demand CAS pull, not the local one.
export def sse-response [core?: string] {
  let live = (try { layout $core | get columns | each {|c| $c.panes} | flatten } catch { [] })
  let seeds = ($live | each {|id| last-core $"pty-($id).screen" $core} | compact)
  let from = (if ($seeds | is-empty) {
    (last-core "panes.layout" $core | get id)
  } else {
    $seeds | get id | sort | first
  })

  cat-core $core --from $from
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
          return {
            out: [(cas-core $f.hash $core | to datastar-patch-elements)]
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
