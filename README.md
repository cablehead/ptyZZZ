<h1>
<p align="center">
  ptyZZZ
  <br><br>
  <sup>A terminal as a unix pipe.</sup>
</p>
</h1>

<p align="center">
  <a href="https://discord.com/invite/YNbScHBHrh">
    <img src="https://img.shields.io/discord/1182364431435436042?logo=discord" alt="Discord">
  </a>
</p>

https://github.com/user-attachments/assets/6cce5e50-480e-4252-a808-84bb8addb533

---

ptyZZZ is a small binary that owns one pty and renders its screen. It runs a
shell, parses the bytes with [wezterm-term](https://github.com/wezterm/wezterm)
into a cell grid, and renders that grid to HTML. Keystrokes and resizes go in as
JSONL on stdin; the rendered screen comes out as JSONL on stdout.

```
JSONL commands ──> ptyZZZ ──> JSONL screen frames
   (stdin)        pty + grid       (stdout)
```

No TUI, no browser-side emulator, no daemon. It is a filter you can run in a
pipe:

```
printf '{"t":"input","b":"ls\n"}\n' | ptyZZZ run -- bash
```

Or wire it to [cross.stream](https://cross.stream), so its screen lands on a log
and any number of readers can follow it. That second path is what the rest of
this is about.

## The protocol

Two streams, both newline-delimited JSON. Commands in:

```
{"t":"input","b":"ls\n"}              raw bytes for the pty
{"t":"resize","cols":80,"rows":24}
```

Screen out:

```
{"t":"screen","seqno":N,"cols":C,"rows":R,"html":"<div id=\"grid\"...>"}
{"t":"exit","code":N}
```

Output is coalesced over a 16ms window, so a burst like `cat big.txt` becomes one
frame rather than one per chunk. ptyZZZ knows nothing about HTTP or cross.stream;
it is a vanilla filter you can test in a pipe, which keeps the glue that turns its
lines into frames in one place.

## Why on a stream

[stacks2099](https://github.com/cablehead/stacks2099) already renders a pty
server-side this way, but each terminal opens its own SSE connection. A handful
of terminals plus the keystroke POSTs runs into the browser's ~6-connection
limit over HTTP/1.1, and input stalls. HTTP/2 sidesteps it, but needs TLS.

The other option is to put the screen on a log. If each terminal is a topic, one
connection can carry many of them, and you choose which to watch by which topics
you subscribe to. ptyZZZ is the piece that makes a terminal fit that model: a
plain process whose screen can become frames.

## As a cross.stream service

cross.stream services run a [Nushell](https://www.nushell.sh) closure as a
long-lived process. With `duplex: true`, frames appended to `<name>.send` are fed
to the closure's stdin; whatever the closure emits becomes `<name>.recv` frames.
The adapter is the only cross.stream-aware code in the project:

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

The `.send` frames flow into ptyZZZ's stdin; its JSONL stdout is fanned out into
typed `.append`s. The closure emits nothing of its own (`| ignore`), so it routes
by `t` rather than letting the default channel dump everything onto one topic.

The web tier is then a reader. The page opens one `/sse`, follows the
`pty.screen` topic, and morphs each frame into `#grid`. A keystroke POSTs to
`/input`, which appends a `pty.send` frame. The grid is rendered on the server,
not in the browser.

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
into ptyZZZ explicitly. It hung: the service went `active`, but no ptyZZZ process
appeared.

`$in` on a stream collects it before passing it on. The duplex input never ends,
so `$in` blocked waiting for it to finish and the external command was never
reached. The fix is to make the external the head of the pipeline. A duplex
service feeds its input to the first command's stdin directly, the way
`websocat | lines` does in the cross.stream docs. No `$in`. Worth knowing for any
service that wraps a long-running CLI.

## What goes on the log

A screen can be stored as the full grid every frame, as pure diffs, or as
keyframes with diffs between. Full-state bloats the log on every keystroke. Pure
diffs can't survive a cold replay, since wezterm's stable row ids and sequence
numbers live in process, not on the log, so a new subscriber has nothing to apply
them against. Keyframes with diffs between is the fit, and the shape
cross.stream's own examples settle on: a snapshot frame (`ttl last:1`) plus deltas
(`ttl time:Ns`, enough to bridge to the next snapshot).

The 16ms window caps the rate at roughly 62 frames per second per terminal
regardless of how fast the shell writes. The worst case for diffs, a full repaint
where every visible row changes (htop, a vim redraw), is also where a single
keyframe is smaller than a stack of row deltas. So the writer can choose per
frame: heavy churn ships a keyframe and resets the basis; quiet typing ships a few
small rows.

ptyZZZ v0 ships keyframes only. The diff path is the next step; the wire already
has room for it (`t` gains `diff`).

## HTML, not JSON

The frame body is rendered HTML, not a cell model. With one writer and many
readers, the render should happen once, at the writer, and each subscriber just
forwards bytes. Storing cells instead means every `/sse` re-runs the cell-to-HTML
pass in Nushell, once per connection. JSON is smaller on disk, but Brotli closes
most of that gap on the wire, and the cost is a per-connection render tax on the
path you set out to make cheap.

## Key by the session, not the clip

Frames are keyed by the pty's session, so a closed pty's screen stays replayable
on the log, and a respawn (new session, same pane) is a swap the web tier makes,
not something the producer tracks. ptyZZZ stays a function of one pty's bytes; the
durable plumbing lives a layer up.

## Run it

```
cargo build --release            # builds ptyZZZ
http-nu --dev --datastar --services --store ./store 127.0.0.1:5111 serve.nu
```

Open http://127.0.0.1:5111 and type into the page. `serve.nu` registers the
service on boot and serves the one-page client.

Needs [http-nu](https://github.com/cablehead/http-nu) (`--store` for the log,
`--services` for the service, `--datastar` for the SSE helpers) and a `bash` on
PATH. The pty render is lifted from
[stacks2099](https://github.com/cablehead/stacks2099); ptyZZZ is where it learns
to live on a stream.

## Driving it over HTTP

Input is a POST that appends a `pty.send` frame, so anything that can make an HTTP
request can type into the terminal. The body of `POST /input` is forwarded to the
pty verbatim, so a command and the carriage return that submits it are two writes:

```
# type a command, then submit it with a carriage return
curl -X POST 127.0.0.1:5111/input --data-binary 'cargo run --example mandelbrot'
curl -X POST 127.0.0.1:5111/input --data-binary $'\r'
```

Send any bytes the same way, control characters included. Ctrl-C is `\x03`, Tab is
`\t`, Escape is `\x1b`:

```
curl -X POST 127.0.0.1:5111/input --data-binary $'\x03'   # interrupt
```

Read the current screen once, or follow the live stream:

```
# latest frame, tags stripped to plain text
curl -s 127.0.0.1:5111/snap | sed 's/<[^>]*>/ /g'

# the SSE stream the browser uses
curl -sN 127.0.0.1:5111/sse
```

Same path the page uses; the page is a keyboard and a `#grid` bound to it.
