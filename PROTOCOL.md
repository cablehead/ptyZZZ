# ptyZZZ JSONL protocol

ptyZZZ owns one pty and speaks JSONL on stdio. It knows nothing about xs --
a nushell service closure adapts these lines to/from frames.

## stdin (commands -> pty), one JSON object per line

    {"t":"input","b":"ls\n"}        raw bytes for the pty (b is a utf-8 string)
    {"t":"resize","cols":80,"rows":24}

## stdout (events <- pty), one JSON object per line

    {"t":"screen","seqno":N,"cols":C,"rows":R,"html":"<div id=\"grid\"...>"}
    {"t":"diff","seqno":N,"target":"grid","patch":"...","append":"...","trim":["grid-r-0"]}
    {"t":"exit","code":N}

`screen` is a keyframe: the full scrollback plus visible grid (`--scrollback`
lines, default 3000; 0 = visible screen only) as one `<div id="grid">` wrapping
a `<div class="row" id="grid-r-{stable}">` per line, keyed by a stable row id
that follows each line into scrollback, plus a
`<div class="cursor" id="grid-cursor">` overlay positioned by
`--cursor-row`/`--cursor-col` CSS vars. Keyframes are emitted on start, on
resize, on an alt-screen flip, when a burst changes more than half the rows,
and as a healing checkpoint every `--keyframe-interval` seconds (default 5)
while diffs are flowing. The adapter stores the latest keyframe (`ttl last:1`)
as the join point for new subscribers.

`diff` carries only what changed since the previous frame:

    patch    changed rows, and the cursor overlay when it moved -- morph by id
    append   rows that scrolled into the grid -- append into `#target`
    trim     ids of rows that fell off the scrollback -- remove

Diffs are ephemeral on the log (`ttl ephemeral`): live subscribers apply them,
joiners start from the stored keyframe instead, and because every subscriber
also receives the periodic keyframes, a missed or misapplied diff heals within
one keyframe interval. Since an ephemeral frame is never stored, the adapter
should carry the diff payload in frame meta rather than the CAS
(`null | .append pty.diff --ttl ephemeral --meta {body: $line}`) -- this skips
a disk write per diff and a CAS read per subscriber.

Frames are emitted only when something visibly changed: damage is tracked per
row via the emulator's dirty flags, re-rendered rows are byte-compared against
a row cache,
and byte-identical output (cursor-only escape traffic, no-op prompt redraws) is
suppressed. Output is coalesced over a 16ms window (`--coalesce`), so a burst
like `cat big.txt` becomes one frame instead of one per chunk.

## standalone probe (no xs)

    printf '{"t":"input","b":"ls\\n"}\n' | ptyZZZ run -- nu
