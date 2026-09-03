# panes

A niri-style strip of live ptyZZZ columns you can open, split, and close at
runtime. Each pane is this repo's `ptyZZZ` running `nu`. Close kills that pty.
A refresh reconnects; an http-nu restart respawns a fresh `nu` in each
surviving slot.

Root `serve.nu` stays the static `PTYZZZ_BINS` demo. This example is the
dynamic multiplexer.

## Layout

    serve.nu                 glue: store layout + pty services + routes
    templates/page.html      shell (minijinja; includes the rest)
    templates/strip.html     columns
    templates/column.html    one column of panes
    templates/pane.html      one pane
    templates/panel.html     mod+K command panel
    static/panes.css
    static/panes.js

Columns are 100ch, full height, panned horizontally. Split stacks another
pane in the focused column.

Zoom (`f`) maximizes the selected pane over the whole strip. It is per
browser tab, not layout: nothing is stored server side, and it follows the
selection rather than pinning to a pane id.

## Requirements

- `ptyZZZ` built in this repo: `cargo build --release`
- `http-nu` on PATH, with `--store`, `--services`, and `--datastar`

## Run

From the repo root:

    http-nu --dev --datastar --services --store ./store 127.0.0.1:5111 examples/panes/serve.nu

Use a dedicated store if you also run root `serve.nu` or cube against `./store`.

### https

Add `--tls <pem>` to serve https. Make a self-signed pair first:

    openssl req -x509 -newkey rsa:2048 -nodes -days 825 \
      -keyout key.pem -out cert.pem \
      -subj /CN=localhost -addext subjectAltName=DNS:localhost,IP:127.0.0.1
    cat cert.pem key.pem > localhost.pem

    http-nu --dev --datastar --services --tls localhost.pem --store ./store 127.0.0.1:5111 examples/panes/serve.nu

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
