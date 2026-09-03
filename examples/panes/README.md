# panes

A niri-style strip of live ptyZZZ columns you can open, split, and close at
runtime. Each pane is this repo's `ptyZZZ` running `nu`. Close kills that pty.
A refresh reconnects; an http-nu restart respawns a fresh `nu` in each
surviving slot.

Root `serve.nu` stays the static `PTYZZZ_BINS` demo. This example is the
dynamic multiplexer.

## host and viewer

Two processes, two [xs](https://github.com/cablehead/xs) stores:

    host.nu     owns the ptys, runs with --services, writes its own stream
    viewer.nu   renders a replica of host's stream, no --services

`common.nu` holds the read side both share: layout lookup and the `/sse`
fold. It is a pure fold over one ordered `.cat --follow`, parametrized by an
optional replica core name (`null` reads the local store) -- the only thing
that changes between host and viewer is which store that fold reads.

The viewer runs without `--services` and never calls `spawn-pane`/
`ensure-panes` on purpose: `xs.service.pty-<id>.create` carries the closure
that spawns a ptyZZZ process, and a viewer that both replicates those frames
and runs `--services` would spawn its own pty -- a second, divergent
terminal instead of a view of the host's.

Currently only rendering is split (phase A): `/input` and `/pane/*` are still
host-only routes, so drive the layout through host's port. See
[docs/adr/0008-replica-stores.md](https://github.com/cablehead/xs/blob/feat/replica-stores/docs/adr/0008-replica-stores.md)
in the `xs` repo for the replica model this leans on.

## Layout

    common.nu                 shared read side: layout lookup, /sse fold
    host.nu                    store layout + pty services + routes
    viewer.nu                  replica of host's stream + read-only routes
    templates/page.html        shell (minijinja; includes the rest)
    templates/strip.html       columns
    templates/column.html      one column of panes
    templates/pane.html        one pane
    templates/panel.html       mod+K command panel
    static/panes.css
    static/panes.js

Columns are 100ch, full height, panned horizontally. Split stacks another
pane in the focused column.

Zoom (`f`) maximizes the selected pane over the whole strip. It is per
browser tab, not layout: nothing is stored server side, and it follows the
selection rather than pinning to a pane id.

## Requirements

- `ptyZZZ` built in this repo: `cargo build --release`
- `http-nu` on PATH, with `--store`, `--services`, and `--datastar`, built
  against an `xs` that has replica stores (`feat/replica-stores` as of this
  writing -- not yet released, so this means a local build; see that
  branch's `docs/adr/0008-replica-stores.md` handoff section)

## Run

From the repo root, in one terminal:

    http-nu --dev --datastar --services --store ./host-store 127.0.0.1:5111 examples/panes/host.nu

In another:

    PANES_HOST_ADDR=./host-store \
      http-nu --dev --datastar --store ./viewer-store 127.0.0.1:5112 examples/panes/viewer.nu

Open http://127.0.0.1:5111 to drive it (spawn/split/close panes, type into
them) and http://127.0.0.1:5112 to watch the same panes render from the
replica. Use dedicated stores if you also run root `serve.nu` or cube
against `./store`.

To run just the host, standalone, as it worked before the split: same
command as above, any port, no viewer needed.

### https

Add `--tls <pem>` to serve https. Make a self-signed pair first:

    openssl req -x509 -newkey rsa:2048 -nodes -days 825 \
      -keyout key.pem -out cert.pem \
      -subj /CN=localhost -addext subjectAltName=DNS:localhost,IP:127.0.0.1
    cat cert.pem key.pem > localhost.pem

    http-nu --dev --datastar --services --tls localhost.pem --store ./host-store 127.0.0.1:5111 examples/panes/host.nu

This is what gets the /sse diff stream compressed. http-nu encodes responses
with brotli and nothing else, and browsers only advertise `br` over https. On
plain http the diffs go out raw. Typing `ls -la /usr/bin | first 40` four times
sent 113546 bytes uncompressed against 2396 with brotli, a 47x reduction. TLS
also turns on HTTP/2.

`.static` responses skip the encoder, so `panes.css` and `panes.js` still go out
raw. That cost is one-time per load. The diff stream is the traffic that matters.

## Tests

`source` the handler and `do $c $req` (http-nu eval). Flags go after `eval`:

    http-nu eval --datastar --store /tmp/panes-test --services examples/panes/test.nu

`test-viewer.nu` proves the host/viewer split instead: it spawns real host
and viewer server processes (`job spawn`, two stores) and curls the
viewer's `/sse` to confirm a pane spawned on the host shows up there.

    examples/panes/test-viewer.nu

## Keys

Two modes, same as stacks2099.

- **navigate** (default): bare `h`/`l` move columns, `j`/`k` move panes in
  the column, `n` new column, `s` split, `f` zoom. Enter or click focuses the
  pty.
- **focus**: keys go to `nu`. `mod+Enter` toggles back to navigate.
- **mod+K**: command panel in both modes (which-key after a short pause).
  Close is `mod+K x` only.

`mod` is Cmd on macOS. On Linux, Ctrl+K is the leader in navigate mode;
Ctrl+Enter toggles focus.
