# ptyZZZ JSONL protocol

ptyZZZ owns one pty and speaks JSONL on stdio. It knows nothing about xs --
a nushell service closure adapts these lines to/from frames.

## stdin (commands -> pty), one JSON object per line

    {"t":"input","b":"ls\n"}        raw bytes for the pty (b is a utf-8 string)
    {"t":"resize","cols":80,"rows":24}

## stdout (events <- pty), one JSON object per line

    {"t":"screen","seqno":N,"cols":C,"rows":R,"html":"<div id=\"grid\"...>"}
    {"t":"exit","code":N}

v0 emits only `screen`: the full visible grid, coalesced over a 16ms window
(a burst collapses to one frame). The html is a `<div id="grid">` wrapping one
`<div class="row" id="grid-r-{i}">` per visible row; the service forwards it as
a datastar morph of `#grid`. `diff` frames (row/append/trim keyed by stable id)
are a later revision; the wire stays the same shape, just more `t` values.

## standalone probe (no xs)

    printf '{"t":"input","b":"ls\\n"}\n' | ptyZZZ run -- nu
