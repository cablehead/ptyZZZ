use std/assert

# Layout-mechanics endpoint tests: `source viewer.nu; do $c $req`. These
# cover everything viewer owns outright (page rendering, layout mutation,
# panes.patch forwarding) without a live pty, since after the split a real
# pty only ever exists on a host, started by xs's own service machinery
# reacting to a remote append -- see test-viewer.nu for the two-process
# version of this same test that drives it end to end (spawn, input, echo).
#
#   PANES_HOSTS=host-a=/tmp/panes-test-fake-host \
#     http-nu eval --datastar --store /tmp/panes-test examples/panes-replicated/test.nu

const script_dir = path self | path dirname
let c = source ($script_dir | path join viewer.nu)

let page = (do $c {method: "GET", path: "/", headers: {}, query: {}} | into string)
assert ($page | str contains "<!doctype html") "GET / is html"
assert ($page | str contains "grid-p1") "GET / seeds grid-p1"
assert ($page | str contains "col-c1") "GET / seeds col-c1"
assert ($page | str contains "actions-panel") "GET / includes mod+K panel"
assert ($page | str contains "/static/panes.js") "GET / loads static js"
assert ($page | str contains "/datastar@1.0.2.js") "GET / loads datastar"
assert (not ($page | str contains "id=split")) "no root serve.nu split toggle"
assert (not ($page | str contains "PTYZZZ_BINS")) "no PTYZZZ_BINS chrome"

let n1 = (do $c {method: "POST", path: "/pane/new-column", headers: {}, query: {after: "p1"}} | into string | from json)
assert ($n1.id == "p2") $"new-column id p2, got ($n1.id)"
assert ($n1.col == "c2") $"new-column col c2, got ($n1.col)"

let page2 = (do $c {method: "GET", path: "/", headers: {}, query: {}} | into string)
assert ($page2 | str contains "grid-p2") "page has p2"
assert ($page2 | str contains "col-c2") "page has c2"

let sp = (do $c {method: "POST", path: "/pane/split", headers: {}, query: {pane: "p1"}} | into string | from json)
assert ($sp.col == "c1") $"split stays in c1, got ($sp.col)"
assert ($sp.id == "p3") $"split id p3, got ($sp.id)"

let page3 = (do $c {method: "GET", path: "/", headers: {}, query: {}} | into string)
assert ($page3 | str contains "grid-p3") "page has split pane p3"
let lay = (.last "panes.layout" | get meta)
assert ($lay.columns.0.id == "c1") "first column is c1"
assert (($lay.columns.0.panes | get id) == ["p1", "p3"]) "split stacked p3 under p1 in c1"
assert ($lay.columns.1.id == "c2") "second column is c2"
assert (($lay.columns.1.panes | get id) == ["p2"]) "new-column p2 is its own column"
assert ($lay.columns.0.panes | all {|p| $p.host == "host-a"}) "every pane in this single-host test carries its host"

let cl = (do $c {method: "POST", path: "/pane/close", headers: {}, query: {pane: "p1"}} | into string | from json)
assert ($cl.id == "p1") "close echoes p1"
assert ($cl.next == "p3") $"close next is p3, got ($cl.next)"
let page4 = (do $c {method: "GET", path: "/", headers: {}, query: {}} | into string)
assert (not ($page4 | str contains "grid-p1")) "p1 gone"
assert ($page4 | str contains "grid-p3") "p3 remains"
assert ($page4 | str contains "grid-p2") "p2 remains"

let bad = (do $c {method: "POST", path: "/pane/split", headers: {}, query: {pane: "nope"}} | into string)
assert ($bad | str contains "unknown pane") "split unknown pane errors"

do $c {method: "POST", path: "/pane/close", headers: {}, query: {pane: "p3"}} | ignore
let last = (do $c {method: "POST", path: "/pane/close", headers: {}, query: {pane: "p2"}} | into string | from json)
assert ($last.next == null) "closing the last pane returns next null"
let empty = (do $c {method: "GET", path: "/", headers: {}, query: {}} | into string)
assert (not ($empty | str contains "grid-p")) "close-all stays empty on GET /"

let n2 = (do $c {method: "POST", path: "/pane/new-column", headers: {}, query: {}} | into string | from json)
assert ($n2.id | str starts-with "p") "new-column from empty returns a pane id"
let page5 = (do $c {method: "GET", path: "/", headers: {}, query: {}} | into string)
assert ($page5 | str contains $"grid-($n2.id)") "empty workspace can open a column"

let bad_input = ("hello" | do $c {method: "POST", path: "/input", headers: {}, query: {pane: "nope"}} | into string)
assert ($bad_input | str contains "unknown pane") "/input on an unknown pane errors (it needs the pane's host)"

# SKIPPED, not deleted: sse-response folds an interleave of viewer's own
# store and one replica per host (common.nu), and nushell's `interleave`
# does not clean up early termination -- `first N`/`lines | first N`,
# called in-process the way every other route in this file is called,
# hangs the whole eval rather than returning. A real HTTP connection does
# not hit this (the client disconnecting tears the per-connection thread
# down a different way -- see test-viewer.nu, which curls a real running
# viewer for exactly this), so the bug is specific to in-process testing,
# not to the feature.
#
# This is xs-replica-panes.md task 3 (nushell `interleave` producer-thread
# cleanup on signal), already fixed and pushed to
# cablehead/nushell fix/interleave-cancellation -- not yet in this repo's
# nushell. Once it is, call `skipped-sse-diff-forwarding-test $c` from the
# body above instead of leaving it unreferenced below.
def skipped-sse-diff-forwarding-test [c] {
  # /sse forwards a diff only when it chains to the last frame that
  # connection sent. Diffs are normally ephemeral; these are stored so the
  # --from replay delivers them without needing a concurrent writer.
  '<div id="grid-seedpane">BASE</div>' | .append "pty-seedpane.screen" --ttl last:1 --meta {seqno: 100} | ignore
  def fake-diff [seqno: int, base: int, marker: string] {
    null | .append "pty-seedpane.diff" --meta {body: ({
      t: "diff", seqno: $seqno, base: $base, target: "grid-seedpane",
      patch: $'<div class="row" id="grid-seedpane-r-($seqno)">($marker)</div>', append: "", trim: []
    } | to json -r)} | ignore
  }
  fake-diff 200 100 "GOOD-ONE"
  fake-diff 900 999 "STALE"
  fake-diff 300 200 "GOOD-TWO"
  let lay = (.last "panes.layout" | get meta)
  null | .append "panes.layout" --ttl last:1 --meta {columns: ($lay.columns | append {id: "cseed", panes: [{id: "seedpane", host: "host-a"}]})} | ignore

  # `first` rejects a string stream, so take lines: three sse events are nine.
  let sse = (null | do $c {method: "GET", path: "/sse", headers: {}, query: {}} | lines | first 9 | str join "\n")
  assert ($sse | str contains "BASE") "sse seeds the current keyframe"
  assert ($sse | str contains "GOOD-ONE") "a diff whose base matches is forwarded"
  assert ($sse | str contains "GOOD-TWO") "a diff chaining off the previous diff is forwarded"
  assert (not ($sse | str contains "STALE")) "a diff whose base does not match is dropped"
}

print "test.nu: all assertions passed"
