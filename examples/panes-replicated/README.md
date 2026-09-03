# panes-replicated

A niri-style strip of ptyZZZ columns, like [`examples/panes`](../panes), but
the panes can live on any number of remote hosts and one viewer renders all
of them at once. `examples/panes` stays exactly as it is -- standalone, no
`xs` replica dependency -- this is a separate example, not a mutation of it.

## The model

One [xs](https://github.com/cablehead/xs) store per host, and each host's
store is the only writer of anything its ptys produce (`pty-*.screen`,
`pty-*.diff`, `xs.service.pty-*.*`):

    host.nu (one per machine)   runs --services, owns nothing custom.
                                Opening a pane is `xs.service.pty-<id>.create`
                                landing directly on its store -- xs's own
                                service machinery does the rest.
    viewer.nu (one)             the browser surface. Replicates every
                                configured host and folds them into one
                                `/sse` with `interleave`. Owns its own
                                layout (panes.layout/panes.patch/panes.seq)
                                -- how *this* viewer arranges panes on
                                screen, not something any host needs to
                                know about. Never runs --services.

**Writes go to the remote.** To open a pane, viewer appends
`xs.service.pty-<id>.create` directly to the owning host's store --
`xs append <host-addr> ...`, an RPC, not a local append (`remote-append` in
viewer.nu). This does not break single-writer: the host's store still mints
every id and orders every frame, and an RPC with two clients is one writer,
not two. The rule is that one *store* owns a log, not that only one
*process* may call append into it.

**Reads come from the replica.** Viewer never talks to a host directly to
render something. It declares a replica of it (`xs.replica.<name>.create`)
and reads that instead, same as any other read (`.cat <name> --follow`,
`.last <name> ...`).

**One viewer, many hosts.** `common.nu`'s `sse-response` interleaves a
replica of every configured host, tagging each frame with which one it came
from so a keyframe's hash dereferences against the right one:

    interleave { .cat --follow } { .cat host-a --follow } { .cat host-b --follow } ...

`interleave` is why this scales past one host without changing shape: adding
a host is adding a replica to the list, not touching the fold.

There is no host-side dispatcher, unlike an earlier version of this example.
`xs.service.pty-<id>.create` landing on a host's store already starts the
pty -- that is what `--services` turns on. Hand-rolling something to react
to it would just be redoing what xs does natively, and would also be one
more thing to keep alive: an `xs.service` gets xs's own lifecycle and
restart-on-boot; a bespoke background job would not, and if it silently died
the host would stop acting on every open/close a viewer sent with nothing to
notice. There is nothing unsupervised here because there is nothing custom
here.

## Known limitations

- **Same-VM only, for now.** `xs append <host-addr> ...` and
  `xs.replica.<name>.create {addr: <host-addr>}` both need `<host-addr>` to
  be reachable -- a filesystem path to a Unix-socket store on this
  machine. Reaching a host over pai-sho is a follow-on (not this task's
  job); see `~/task/xs-replica-panes.md` for the model this leans on.
- **Viewer needs to know a host's `ptyZZZ` binary path.** Viewer, not the
  host, constructs the pty-spawning closure (`$SVC` in viewer.nu) before
  remote-appending it, since it is the one deciding a pane should exist.
  That closure embeds an absolute path to `ptyZZZ`, resolved on viewer's own
  filesystem. Same-VM, that path is valid on the host too; across machines
  it would not be, and nothing here checks. A `PTYZZZ` path per configured
  host (not one global constant) is the fix when that stops being true.
- **No placement UI.** `/pane/new-column?host=<name>` places a pane
  explicitly; omitted, it defaults to the first configured host. `/pane/
  split` keeps a column co-located on whichever host it started on. Picking
  a host interactively (least loaded, geographically nearest, whatever)
  is future work, not modeled here.

## Layout

    common.nu             shared read side: layout lookup, the interleaved /sse fold
    host.nu                a bare pty host: --services and nothing else
    viewer.nu              the browser surface: layout, remote writes, N-host replica reads
    templates/             page/strip/panel.html are unchanged copies of examples/panes/templates;
                            column.html/pane.html render a pane as {id, host}, not a bare id --
                            see common.nu for why they can't be shared by path reference here
    static/                none -- served from ../panes/static by path reference (`.static`
                            serves plain files, so this one *can* be shared)

## Requirements

- `ptyZZZ` built in this repo: `cargo build --release`
- `http-nu` on PATH, with `--store`, `--services`, and `--datastar`, built
  against an `xs` that has replica stores (`feat/replica-stores` as of this
  writing -- not yet released, so this means a local build; see that
  branch's `docs/adr/0008-replica-stores.md` handoff section). As of this
  writing http-nu's `main` needs two small local patches to build and run
  against that branch -- neither is a panes design change, both are the
  surrounding harness catching up to xs's current API (a third patch,
  wrapping top-level script eval in `tokio::task::block_in_place` to work
  around `.last`/`.cat` panicking on a runtime thread, is *not* needed as of
  xs `feat/replica-stores` commit `525c1481`: `Store::blocking_recv` now
  steps out of the runtime itself, so no embedder has to know about the
  constraint):
  - `add_read_commands`'s now-removed `ReadMode` argument dropped
    (`src/engine.rs`), and `store.read()` awaited as async dropped to a
    plain call (`src/store.rs`) -- xs's read commands were consolidated
    since http-nu's last `cross-stream` version bump.
  - the replica supervisor (`xs::processor::replica::run`) spawned
    unconditionally in `Store::init` (`src/store.rs`), not gated behind
    `services` like actor/service/action -- a viewer needs its replica
    running without opting into `--services`.
- `xs` (the CLI, not just the library http-nu embeds) on PATH -- viewer
  shells out to `xs append <host-addr> ...` for every remote write

## Run

One host:

    http-nu --dev --services --store ./host-a-store 127.0.0.1:5111 examples/panes-replicated/host.nu

A second, to see the multi-host fold do something (optional -- one host
works fine too):

    http-nu --dev --services --store ./host-b-store 127.0.0.1:5121 examples/panes-replicated/host.nu

The viewer, naming every host it should replicate:

    PANES_HOSTS=host-a=./host-a-store,host-b=./host-b-store \
      http-nu --dev --datastar --store ./viewer-store 127.0.0.1:5112 examples/panes-replicated/viewer.nu

Open http://127.0.0.1:5112 -- that is the only port with a browser UI.
`http://127.0.0.1:5111`/`5121` each answer a plaintext "no browser routes
here" on `/`, useful only to confirm a host came up. Use dedicated stores if
you also run root `serve.nu`, `examples/panes`, or cube.

Order doesn't matter at startup -- `xs.replica.*.create` waits and
reconnects with backoff until a host's socket exists; a remote append that
races a host's own startup just retries the same way any RPC would (nothing
here retries it automatically yet -- see Known limitations).

## Tests

`test.nu` sources viewer.nu in-process (`do $c $req`, http-nu eval) and
covers everything viewer owns outright: page rendering, layout mutation
(`new-column`/`split`/`close`, now carrying a `{id, host}` per pane),
`panes.patch` forwarding. `PANES_HOSTS` points at a store nothing is serving,
so every remote append it triggers fails and is logged, not raised -- layout
changes still land, the same way a real, briefly-unreachable host would not
block an edit either:

    PANES_HOSTS=host-a=/tmp/panes-test-fake-host \
      http-nu eval --datastar --store /tmp/panes-test examples/panes-replicated/test.nu

`test-viewer.nu` proves the model for real: two real hosts, one real viewer,
three real stores (`job spawn`). It places one pane on each host and drives
both through viewer's routes only -- open, type, and read back the echo --
confirming the interleaved fold renders both and `/input`/`/pane/*` route
each write to the pane's own host, not a single fixed one:

    examples/panes-replicated/test-viewer.nu
