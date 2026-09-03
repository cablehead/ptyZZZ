# examples/panes-replicated -- a bare pty host. Run with --services and
# nothing else is needed: opening a pane is `xs.service.pty-<id>.create`
# landing directly on this store (a remote append from viewer.nu, an RPC to
# the store that owns the log -- see that file and this directory's
# README.md for the model), and xs's own service machinery, which
# --services turns on, does the rest. No dispatcher, no relay: there is
# nothing unsupervised here, because there is nothing custom here.
#
# Run:
#   http-nu --dev --services --store ./host-store 127.0.0.1:5111 examples/panes-replicated/host.nu

use http-nu/router *

{|req|
  dispatch $req [
    (route {method: "GET", path: "/"} {|req ctx|
      "panes-replicated host: no browser routes here, see viewer.nu" | metadata set --content-type "text/plain"
    })
  ]
}
