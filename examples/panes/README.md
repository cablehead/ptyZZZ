# panes

A niri-style strip of live ptyZZZ columns you can open, split, and close at
runtime. Each pane is this repo's `ptyZZZ` running `nu`. Close kills that pty.
A refresh reconnects; an http-nu restart respawns a fresh `nu` in each
surviving slot.

Root `serve.nu` stays the static `PTYZZZ_BINS` demo. This example is the
dynamic multiplexer.

## host and viewer

Two processes, two [xs](https://github.com/cablehead/xs) stores, one writer
per stream (ADR 0008 in the `xs` repo, and this repo's
`~/task/xs-replica-panes.md` task 5):

    host.nu     owns the ptys, runs with --services, writes pty-*.screen,
                pty-*.diff, xs.service.pty-*.* -- content a pty produces
    viewer.nu   the only browser surface. Writes panes.layout, panes.patch,
                panes.seq, pty-*.send, and pane intents -- panes.spawn.<id>,
                panes.kill.<id> -- to its own stream. No --services.

Each replicates the other (`xs.replica.<name>.create`) and reacts only to
the frames that are its to react to. Viewer's `/sse` (`common.nu`'s
`sse-response`) folds an interleave of its own store and a replica of
host's, so a page render draws on both: layout/patches from one side, pty
content from the other, merged the same way `interleave { .cat --follow } {
.cat host --follow }` merges any two streams. Host's dispatcher (a
`job spawn` background fold in host.nu, not an `xs.service` -- that closure
text is a self-contained script reparsed in an isolated engine, so it
couldn't see host.nu's own `spawn-pane`/`kill-pane` the way a same-session
background job can) does the mirror image: it folds its own store with a
replica of viewer's, and turns a `panes.spawn.<id>`/`panes.kill.<id>` intent
into a real `xs.service.pty-<id>.create`/`.term`, and relays a `pty-<id>.send`
into its own store so the pty's duplex service (which only watches host's
own store) forwards it to the child's stdin.

Viewer never touches `xs.service.*` and never runs `--services`, on purpose:
`xs.service.pty-<id>.create` carries the closure that spawns a ptyZZZ
process, and a viewer that both replicated those frames *and* ran
`--services` would spawn its own pty -- a second, divergent terminal instead
of a view of the host's. Viewer's `spawn-pane`/`kill-pane` only ever append
an intent, never `xs.service.*`, so this holds structurally: there is
nothing on viewer's store `--services` could act on even by mistake.

## Layout

    common.nu                 shared read side: layout lookup, /sse fold
    host.nu                    pty services + a dispatcher reacting to viewer's intents
    viewer.nu                  the browser surface: layout, patches, intents, replica of host
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
  branch's `docs/adr/0008-replica-stores.md` handoff section). As of this
  writing http-nu's `main` needs two small local patches to build and run
  against that branch (neither is a panes design change, both are the
  surrounding harness catching up to xs's current API):
  - `Cargo.toml`'s `cross-stream` dependency pointed at a local `xs`
    checkout instead of the crates.io release, and `add_read_commands`'s
    now-removed `ReadMode` argument dropped (`src/engine.rs`), and
    `store.read()` awaited as async dropped to a plain call
    (`src/store.rs`) -- xs's read commands were consolidated since
    http-nu's last `cross-stream` bump.
  - the replica supervisor (`xs::processor::replica::run`) spawned
    unconditionally in `Store::init` (`src/store.rs`), not gated behind
    `services` like actor/service/action -- a viewer needs its replica
    running without opting into `--services`.
  - top-level script evaluation (`Engine::parse_closure`, `Engine::eval` in
    `src/engine.rs`) wrapped in `tokio::task::block_in_place` -- xs's
    `.last`/`.cat` block synchronously now (`Receiver::blocking_recv`),
    which panics if called inline on a tokio runtime thread the way
    http-nu's startup-script eval does today. Per-request route handling
    already gets its own dedicated thread (`worker::spawn_eval_thread`) and
    was unaffected; only the once-at-startup eval path needed this.

## Run

From the repo root, in one terminal:

    PANES_VIEWER_ADDR=./viewer-store \
      http-nu --dev --services --store ./host-store 127.0.0.1:5111 examples/panes/host.nu

In another:

    PANES_HOST_ADDR=./host-store \
      http-nu --dev --datastar --store ./viewer-store 127.0.0.1:5112 examples/panes/viewer.nu

Open http://127.0.0.1:5112 -- that is the only port with a browser UI now.
http://127.0.0.1:5111 answers a plaintext "no browser routes here" on `/`,
useful only to confirm host itself came up. Use dedicated stores if you also
run root `serve.nu` or cube against `./store`.

Order doesn't matter at startup -- each side's `xs.replica.*.create` just
waits and reconnects with backoff until the other's socket exists, and
viewer's own layout/patches don't depend on host being up yet (though
nothing will actually render inside a pane until host's dispatcher can
reach it).

### https

Add `--tls <pem>` to serve https on viewer's port. Make a self-signed pair
first:

    openssl req -x509 -newkey rsa:2048 -nodes -days 825 \
      -keyout key.pem -out cert.pem \
      -subj /CN=localhost -addext subjectAltName=DNS:localhost,IP:127.0.0.1
    cat cert.pem key.pem > localhost.pem

    PANES_HOST_ADDR=./host-store \
      http-nu --dev --datastar --tls localhost.pem --store ./viewer-store 127.0.0.1:5112 examples/panes/viewer.nu

This is what gets the /sse diff stream compressed. http-nu encodes responses
with brotli and nothing else, and browsers only advertise `br` over https. On
plain http the diffs go out raw. Typing `ls -la /usr/bin | first 40` four times
sent 113546 bytes uncompressed against 2396 with brotli, a 47x reduction. TLS
also turns on HTTP/2.

`.static` responses skip the encoder, so `panes.css` and `panes.js` still go out
raw. That cost is one-time per load. The diff stream is the traffic that matters.

## Tests

`test.nu` sources viewer.nu in-process (`do $c $req`, http-nu eval) and
covers everything viewer owns outright without a live pty: page rendering,
layout mutation (`new-column`/`split`/`close`), `panes.patch` forwarding on
`/sse`. No `--services`, no store needed for a pty since none spawns here:

    http-nu eval --datastar --store /tmp/panes-test examples/panes/test.nu

`test-viewer.nu` proves the host/viewer split for real: it spawns actual
host and viewer server processes (`job spawn`, two stores, host with
`--services`) and drives everything through viewer's HTTP port -- host has
no `/pane/*` or `/input` route to hit directly anymore. It covers phase A
(a pane host spawns at boot shows up in viewer's `/sse` via the replica) and
phase B (opening a column and typing through viewer's own routes reaches a
real pty on host and its echo comes back through the replica).

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
