use std/assert

# Layout-mechanics endpoint tests: `source viewer.nu; do $c $req`. These
# cover everything viewer owns outright (page rendering, layout mutation,
# panes.patch forwarding) without a live pty, since after the host/viewer
# split a real pty only exists on host, driven by its dispatcher reacting to
# viewer's intents -- see test-viewer.nu for the two-process version of this
# same test that drives it end to end (spawn, input, echo).
#
#   http-nu eval --datastar --store /tmp/panes-test examples/panes/test.nu

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
assert ($lay.columns.0.panes == ["p1", "p3"]) "split stacked p3 under p1 in c1"
assert ($lay.columns.1.id == "c2") "second column is c2"
assert ($lay.columns.1.panes == ["p2"]) "new-column p2 is its own column"

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

# /sse is not exercised here on purpose: sse-response folds an interleave
# of viewer's own store and a replica of host's (common.nu), and nushell's
# `interleave` does not clean up early termination -- `first N`/`lines |
# first N`, called in-process the way this file calls every other route,
# hangs the whole eval rather than returning (a real HTTP connection over
# a real port does not hit this, since the client disconnecting tears the
# per-connection thread down a different way -- see test-viewer.nu, which
# curls a real running viewer for exactly this).

print "test.nu: all assertions passed"
