# ADR 0001: Terminal emulation crate: stay on wezterm-term, keep rio-vt as the successor candidate

Date: 2026-08-05

## Context

ptyZZZ needs a headless VT engine: parse pty bytes, hold a grid plus
scrollback, report damage, and let us read cells back out to render HTML
diffs. We use wezterm-term. It is a good engine, but wezterm does not
publish it to crates.io, so we pin a git rev into wezterm's large
workspace.

Two alternatives appeared in 2026:

- rio-vt (Rio 0.5): Rio's core extracted as a published Rust crate. Lean
  by default; renderer, graphics, and clipboard are opt-in features.
- libghostty-vt: Ghostty's core as a Zig library with a C ABI, plus Rust
  wrappers on crates.io (libghostty-vt / libghostty-vt-sys). Building it
  requires a pinned Zig toolchain.

## What we did

Benchmarks first, so candidates could be judged instead of argued about:

- bench/e2e.nu: five full-stack scenarios against the real binary
  (firehose throughput, scroll24 / urls24 / aqua24 / vim24 paced at 24
  fps, keystroke latency percentiles). Corpora are recorded raw pty
  byte streams (bench/record.sh, via the --record flag) and are vendored
  so runs are deterministic.
- examples/scanbench.rs: per-tick microbench of the diff path
  (parse / damage scan / row render).

Then two ports in parallel worktrees, each required to keep the JSONL
protocol and frame semantics identical and to pass the bench suite:

- origin/rio-vt: near drop-in. +87 lines in main.rs. Stable row ids
  derived from lines_evicted(). Dependency tree shrank from 505 cargo
  tree lines to 145; the git pin is gone.
- origin/ghostty-vt: worked, but fought the grain. The safe wrapper hid
  the batched C calls, so it owns an unsafe FFI module over the sys
  crate. Row identity is hand-built (a tracked-pin "odometer") because
  the render API is viewport-only. Terminal is not Send, forcing a
  channel architecture. +327 lines, plus Zig 0.15.2 at build time.

Steady-state cpu_ms per 10s run, after each port's best tuning:

    scenario     wezterm   rio-vt   ghostty-vt (batched)
    firehose       440       200        250
    scroll24        40        50         70
    vim24           20        20         10-20
    urls24         380       190        --
    latency p50   ~220us    ~280us     ~297us

Frame output is at parity everywhere: identical screen/diff splits and
bytes-per-frame within a few percent.

## Findings

- Both alternatives beat wezterm ~2x on keyframe-heavy throughput; their
  packed-cell layouts iterate cheaper than wezterm's per-cell attr
  clones.
- ghostty trails rio on our workload for structural reasons, not engine
  speed. Our pipeline reads lots of cells per tick; every read crosses
  the C ABI, and ~3 crossings per cell is the API's floor. Ghostty's
  strengths (SIMD parsing, page compression, snapshots, wasm builds)
  sit in stages that were never our bottleneck, and its intended
  architecture (viewport-only rendering, client-side emulation) inverts
  ptyZZZ's server-side-rendering thesis.
- rio's one soft spot: the stable-row-id arithmetic leans on
  lines_evicted() semantics documented for image placements, not as a
  public row-identity contract.
- The evaluation improved main: the damage scan is now clamped to the
  viewport (O(rows), not O(scrollback), and the shape rio's damage
  model wants), and the emitter deducts frame cost from the coalesce
  sleep to hold cadence.
- Hyperlink support (OSC 8 plus implicit URL detection) was implemented
  on main with wezterm-surface's rule set, with safe_href and anchor
  rendering kept engine-agnostic. The rio branch reimplemented only the
  detection half (render-time regex scan, no grid mutation) and runs it
  2x cheaper on urls24. The split proved the agnostic layering works.

## Decision

1. main stays on wezterm-term for now. It is correct, its row-identity
   and damage contracts are the richest, and nothing forces a move.
2. origin/rio-vt is the successor candidate. It is kept at feature
   parity (including hyperlinks) and benched by the same suite. Switching
   is a small, reversible step whenever the git pin becomes a problem.
3. ghostty-vt is out of contention. The work is preserved on
   origin/ghostty-vt; the local worktree and server are gone.

## Revisit when

- rio confirms (or breaks) lines_evicted() as a row-identity contract.
  An upstream issue is worth filing before promoting the branch.
- The wezterm git pin causes real maintenance pain.
- The roadmap adds very deep scrollback, lazy history rendering, or a
  client-side rendering tier. Those are the shapes where ghostty's
  compression, snapshots, and wasm story would earn their costs, and a
  new ADR should reopen it.
