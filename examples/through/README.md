# through

A single full-screen ptyZZZ pane, plus a yaw that shows what the page actually
is. Face on, it feels like typing into a real terminal. The grid is inert
HTML: the emulator lives in a headless wezterm on the server, and the page is
the image it projects.

`see through` yaws that plane around its vertical axis. From the side, a small
box behind the pane is the headless wezterm. Live markup flies from that box
into the back of the pane; `POST /input` chips fly the other way, carrying
the keystrokes. Keys still go to `nu` while it is side-on. `face on` snaps
the plane back.

No splits, no extra panes, no navigate mode. Chrome is the toggle and the
round-trip time to the server.

## Layout

    serve.nu                 glue: one pty service + routes
    templates/page.html      the page (minijinja)
    static/through.css
    static/through.js

## Requirements

- `ptyZZZ` built in this repo: `cargo build --release`
- `http-nu` on PATH, with `--store`, `--services`, and `--datastar`

## Run

From the repo root:

    http-nu --dev --datastar --services --store ./through-store 127.0.0.1:5111 examples/through/serve.nu

Use a dedicated store if you also run root `serve.nu`, cube, or panes.

### https

Add `--tls <pem>` to serve https. Make a self-signed pair first:

    openssl req -x509 -newkey rsa:2048 -nodes -days 825 \
      -keyout key.pem -out cert.pem \
      -subj /CN=localhost -addext subjectAltName=DNS:localhost,IP:127.0.0.1
    cat cert.pem key.pem > localhost.pem

    http-nu --dev --datastar --services --tls localhost.pem --store ./through-store 127.0.0.1:5111 examples/through/serve.nu

This is what gets the /sse diff stream compressed. http-nu encodes responses
with brotli and nothing else, and browsers only advertise `br` over https.

## Tests

    http-nu eval --datastar --store /tmp/through-test --services examples/through/test.nu

`node examples/through/shot.mjs` takes five screenshots (front, typed, side,
side while typing, face on again) if Playwright is available.
