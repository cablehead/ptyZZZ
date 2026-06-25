# ptyZZZ

https://github.com/user-attachments/assets/6cce5e50-480e-4252-a808-84bb8addb533

[stacks2099](https://github.com/cablehead/stacks2099) puts a live terminal in
the browser without a terminal emulator in the browser. The pty runs on the
server, [wezterm-term](https://github.com/wezterm/wezterm) parses its bytes into
a cell grid, the grid renders to HTML, and [Datastar](https://data-star.dev)
morphs it into the DOM. The browser holds no terminal state. It just renders
what the server hands it. The
[journey](https://github.com/cablehead/stacks2099/blob/main/journey.md) to that
shape is its own story.

This is the next question. In stacks2099 each terminal opens its own `/pty/view`
SSE connection. A stack with five terminals is five long-lived streams, plus the
main `/sse`, plus the POSTs that carry your keystrokes. Browsers cap concurrent
connections per origin at roughly six over HTTP/1.1, so you run out. The input
POSTs have nowhere to go and the whole interface freezes.

The fix everyone reaches for is HTTP/2: one connection multiplexes every stream
and the cap never bites. But that needs TLS, and it sidesteps the more
interesting question. What if the pty's screen lived on a log, and the screens
of many terminals were just different topics on that log? Then one connection,
plain HTTP/1.1, could carry them all -- and you could partition which terminals
you watch by which topics you subscribe to.

ptyZZZ is the experiment that asks that. It is a tiny CLI that owns one pty and
speaks JSONL on stdio. It knows nothing about the web, or about
[cross.stream](https://cross.stream). You run it as a cross.stream *service*,
and its screen becomes frames on the log.

## A pty that speaks JSONL

ptyZZZ opens a pty, runs a shell in it, and feeds every byte to wezterm-term --
the same grid stacks2099 uses, lifted out. Two pipes, both newline-delimited
JSON:

```
stdin   {"t":"input","b":"ls\n"}          raw bytes for the pty
        {"t":"resize","cols":80,"rows":24}

stdout  {"t":"screen","seqno":N,"cols":C,"rows":R,"html":"<div id=\"grid\"...>"}
        {"t":"exit","code":N}
```

Commands in, screen out. On a 16ms coalescing window -- so a burst like
`cat big.txt` collapses into one frame rather than one per chunk -- it renders
the visible grid to HTML and prints a `screen` line. It can be driven by hand,
no server in sight:

```
printf '{"t":"input","b":"ls\n"}\n' | ptyZZZ run -- bash
```

Keeping ptyZZZ ignorant of cross.stream is the whole point. It is a vanilla
filter you can test in a pipe. The glue that turns its lines into frames lives
in exactly one place.

## A pty as a cross.stream service

cross.stream services run a [Nushell](https://www.nushell.sh) closure as a
long-lived process. With `duplex: true`, frames appended to `<name>.send` are
fed to the closure's stdin; whatever the closure emits becomes `<name>.recv`
frames. That is the adapter, and it is the only cross.stream-aware code in the
project:

```nushell
{
  run: {||
    ^ptyZZZ run -- bash
    | lines | each {|l|
        let e = $l | from json
        match $e.t {
          screen => ( $e.html | .append pty.screen --ttl last:1 )
          exit   => ( {code: $e.code} | .append pty.exit )
        }
      } | ignore
  }
  duplex: true
}
```

The `.send` frames flow into ptyZZZ's stdin; ptyZZZ's JSONL stdout is fanned out
into typed `.append`s. It emits nothing of its own (`| ignore`), so there is no
`.recv` noise -- the closure routes by `t` instead of letting the default
output channel dump everything onto one topic.

The web tier is now a pure reader. The page opens one `/sse`, which follows the
`pty.screen` topic and morphs each frame into `#grid`. A keystroke POSTs to
`/input`, which appends a `pty.send` frame. Nobody renders a grid in the browser,
and nobody renders one twice.

```mermaid
sequenceDiagram
    autonumber
    participant Browser as Browser (DOM + Datastar)
    participant HTTP as http-nu (/sse, /input)
    participant XS as cross.stream log
    participant Svc as service closure (nu)
    participant Pty as ptyZZZ (pty + wezterm grid)
    participant Sh as bash (child)

    Note over Svc,Sh: service spawns ptyZZZ once;<br/>wezterm-term owns the grid

    rect rgb(235, 245, 255)
        Note over Browser,HTTP: --- client attaches ---
        Browser->>HTTP: GET /sse  (data-init)
        HTTP->>XS: follow topic pty.screen
        XS-->>HTTP: replay last:1 keyframe
        HTTP-->>Browser: Datastar morphs #grid <- SSE patch
    end

    rect rgb(245, 255, 235)
        Note over Browser,Sh: --- keystroke ---
        Browser->>HTTP: POST /input  body=<bytes>
        HTTP->>XS: append pty.send  {t:input,b:..}
        XS->>Svc: frame -> closure stdin
        Svc->>Pty: JSONL line -> ptyZZZ stdin
        Pty->>Sh: bytes via pty.master
        HTTP-->>Browser: 204
    end

    rect rgb(255, 245, 235)
        Note over Sh,Browser: --- output ---
        Sh->>Pty: bytes via pty.master
        Pty->>Pty: wezterm grid mutates, 16ms coalesce
        Pty->>Svc: {t:screen, html:..} on stdout
        Svc->>XS: append pty.screen --ttl last:1
        XS-->>HTTP: follow yields frame
        HTTP-->>Browser: Datastar morphs #grid <- SSE patch
    end
```

## The pipe that deadlocks

The first version wrote `$in | ^ptyZZZ run -- bash`, threading the service input
into ptyZZZ explicitly. It hung -- the service went `active`, but no ptyZZZ
process ever appeared.

`$in` on a stream *collects* it before passing it on. The duplex input is an
infinite stream that never ends, so `$in` blocked forever waiting for it to
finish, and the external command at the other end of the pipe was never reached.
The fix is to make the external the *head* of the pipeline. A duplex service
feeds its input to the first command's stdin directly, the way `websocat | lines`
does in the cross.stream docs. No `$in`. This is the one gotcha worth knowing for
any service that wraps a long-running CLI.

## What goes on the log

A screen could be stored three ways: the full grid every frame, pure diffs, or
keyframes with diffs between. Pure full-state bloats the log on every keystroke.
Pure diffs can't survive a cold replay -- wezterm's stable row ids and sequence
numbers live in process, not on the log -- so a new subscriber would have nothing
to apply them against. Keyframes with diffs between is the fit, and it is the
shape cross.stream's own examples converge on: a snapshot frame
(`ttl last:1`, the newest kept) plus deltas (`ttl time:Ns`, enough to bridge to
the next snapshot).

The coalescing window already caps the rate at ~62 frames per second per
terminal no matter how fast the shell spews, and the worst case for diffs -- a
full repaint where every visible row changes (htop, a vim redraw) -- is exactly
where a single keyframe is *smaller* than a pile of row deltas. So the writer can
pick per frame: lots of in-place churn ships a keyframe and resets the basis;
quiet typing ships a handful of small rows. The expensive case becomes the cheap
one.

ptyZZZ v0 only ships keyframes. The diff path is the obvious next step, and the
wire already has room for it (`t` just gains `diff`).

## HTML, not JSON

The frame body is rendered HTML, not a structured cell model. With one writer and
many readers -- every open tab follows the same topic -- the render should happen
once, at the writer, and every subscriber should forward bytes. Store the cells
instead and each `/sse` has to re-run the cell-to-HTML pass in Nushell, the
slowest language in the stack, once per connection. JSON is modestly smaller on
disk, but Brotli closes most of that gap on the wire, and you would be paying an
M-times render tax on the exact path you set out to optimize. The whole reason to
put the screen on a log is render-once, fan-out-cheap.

## Key by the session, not the clip

Frames are keyed by the pty's session, so a closed pty's screen stays replayable
on the log, and a respawn (new session, same pane) is a swap the web tier makes,
not a thing the producer has to know about. ptyZZZ stays purely a function of one
pty's bytes; the durable plumbing lives a layer up.

## Run it

```
cargo build --release            # builds ptyZZZ
http-nu --dev --datastar --services --store ./store 127.0.0.1:5111 serve.nu
```

Open http://127.0.0.1:5111. Type into the page; the keystrokes round-trip through
the log and the screen morphs back. `serve.nu` registers the service on boot and
serves the one-page client.

Needs [http-nu](https://github.com/cablehead/http-nu) (`--store` for the log,
`--services` for the service, `--datastar` for the SSE helpers) and a `bash` on
PATH. The pty render is lifted from
[stacks2099](https://github.com/cablehead/stacks2099); ptyZZZ is where it learns
to live on a stream.

## Driving it over HTTP

The browser is just one client. Because input is a POST that appends a
`pty.send` frame, anything that can make an HTTP request can type into the
terminal -- a script, a `curl`, another machine on the log. The body of
`POST /input` is forwarded to the pty verbatim, so a command and the carriage
return that submits it are two writes:

```
# type a command, then submit it with a carriage return
curl -X POST 127.0.0.1:5111/input --data-binary 'cargo run --example mandelbrot'
curl -X POST 127.0.0.1:5111/input --data-binary $'\r'
```

Send any bytes the same way -- control characters included. Ctrl-C is `\x03`,
Tab is `\t`, Escape is `\x1b`:

```
curl -X POST 127.0.0.1:5111/input --data-binary $'\x03'   # interrupt
```

Read the current screen as one shot, or follow the live stream:

```
# latest frame, tags stripped to plain text
curl -s 127.0.0.1:5111/snap | sed 's/<[^>]*>/ /g'

# the SSE stream the browser uses: one datastar morph per frame
curl -sN 127.0.0.1:5111/sse
```

This is the same path the page uses; the page is just a keyboard and a `#grid`
bound to it.
