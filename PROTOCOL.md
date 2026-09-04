# ptyZZZ JSONL protocol

ptyZZZ owns one pty and speaks JSONL on stdio. It knows nothing about xs --
a nushell service closure adapts these lines to/from frames.

## stdin (commands -> pty), one JSON object per line

    {"t":"input","b":"ls\n"}        raw bytes for the pty (b is a utf-8 string)
    {"t":"key","key":"ArrowUp","mods":0}
    {"t":"paste","s":"multi\nline"}
    {"t":"resize","cols":80,"rows":24}
    {"t":"screen"}                  emit a keyframe now

`key` is a semantic key event: a browser KeyboardEvent.key name (a single
character, the editing/navigation cluster, or F1-F24) plus a modifier
bitfield (1 shift, 2 alt, 4 ctrl, 8 meta). The emulator encodes it against
its live input modes (application cursor keys, modifyOtherKeys, ...),
which a byte-sending client cannot know. `paste` is wrapped in
bracketed-paste markers when the application has enabled them. Prefer
these over `input` for anything a user typed; `input` stays as the raw
escape hatch (and carries IME-composed text).

`screen` asks for a keyframe on the next emit, whether or not anything
changed. An adapter sends it when a subscriber joins: the stored keyframe can
be up to `--keyframe-interval` stale, an idle pane never heals on its own, and
this replaces the wait with one coalesce window. It also resets the healing
clock.

## stdout (events <- pty), one JSON object per line

    {"t":"screen","seqno":N,"cols":C,"rows":R,"html":"<div id=\"grid\"...>"}
    {"t":"diff","seqno":N,"base":P,"target":"grid","patch":"...","append":"...","trim":["grid-r-0"]}
    {"t":"exit","code":N}

`screen` is a keyframe: the full scrollback plus visible grid (`--scrollback`
lines, default 3000; 0 = visible screen only) as one `<div id="grid">` wrapping
a `<div class="row" id="grid-r-{stable}">` per line, keyed by wezterm's stable
row index, plus a `<div class="cursor" id="grid-cursor">` overlay positioned by
`--cursor-row`/`--cursor-col` CSS vars. Keyframes are emitted on start, on
resize, on an alt-screen flip, when a burst changes more than half the rows,
and as a healing checkpoint every `--keyframe-interval` seconds (default 5)
while diffs are flowing. The adapter stores the latest keyframe (`ttl last:1`)
as the join point for new subscribers.

`base` is the seqno of the last frame actually sent, which is not always the
previous seqno: damage that renders to identical html emits nothing, and those
skipped seqnos must not appear as a base. A subscriber holding a frame
with seqno `S` applies a diff only when `base == S`, then advances to `seqno`.
Any other `base` means frames were missed in between: drop the diff and wait.
That wait is bounded, because a diff only exists if the pane is dirty, so a
healing keyframe is due within `--keyframe-interval`. Without `base`, a gap is
undetectable -- seqnos are wezterm damage counters and advance by arbitrary
amounts, so consecutive frames are not consecutive numbers.

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
row via wezterm seqnos, re-rendered rows are byte-compared against a row cache,
and byte-identical output (cursor-only escape traffic, no-op prompt redraws) is
suppressed. Output is coalesced over a 16ms window (`--coalesce`), so a burst
like `cat big.txt` becomes one frame instead of one per chunk.

## standalone probe (no xs)

    printf '{"t":"input","b":"ls\\n"}\n' | ptyZZZ run -- nu
