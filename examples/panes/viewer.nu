# examples/panes -- read-only render of a replica of host.nu's stream.
#
# viewer: renders from a replica of host's stream. Runs WITHOUT --services
# and never calls spawn-pane/ensure-panes -- see host.nu and
# examples/panes/README.md for why: `xs.service.pty-<id>.create` carries the
# closure that spawns a ptyZZZ process, and a viewer that replicates those
# frames *and* runs --services would spawn a second, divergent terminal
# instead of showing the remote one.
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

if ($HTTP_NU.store? | default null) != null {
  register-replica
}

let page_tpl = .mj compile ($TPL | path join "page.html")

{|req|
  dispatch $req [
    (route {method: "GET", path: "/"} {|req ctx|
      let l = try { layout $HOST_CORE } catch { {columns: []} }
      {datastar: $DATASTAR_JS_PATH, columns: $l.columns}
      | .mj render $page_tpl
      | metadata set --content-type "text/html"
    })

    (route {method: "GET", path: "/sse"} {|req ctx| sse-response $HOST_CORE })

    (route {method: "GET", path-matches: "/static/:file"} {|req ctx|
      .static ($HERE | path join "static") $"/($ctx.file)"
    })

    (route {method: "GET", path-matches: "/fonts/:file"} {|req ctx|
      .static $FONTS $"/($ctx.file)"
    })
  ]
}
