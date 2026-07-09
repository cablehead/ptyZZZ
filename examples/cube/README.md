# cube

A rotating CSS-3D cube whose six faces are six live ptyZZZ screens. Five run
animations; the front face is an interactive `nu` shell you can type into.

It is a demo of running several ptyZZZ views on one page: each face is its own
ptyZZZ instance rendering into a distinct morph target (`--target grid-<n>`),
fanned onto its own cross.stream topic, and morphed into the cube by one `/sse`.

## Layout

    serve.nu             glue: service wiring + routes (no markup)
    templates/cube.html  the page (minijinja, rendered by http-nu's .mj)
    static/cube.css      styles
    static/cube.js       fit-to-face + keystroke handling

## Requirements

- `ptyZZZ` built in this repo: `cargo build --release`
- Two sibling projects, built, at the paths configured at the top of `serve.nu`:
  - `yazelix-screen` -- the `play_style` example (boids, mandelbrot, ...)
  - `asciiquarium-rs` -- the `asciiquarium` binary
  Edit `PLAYSTYLE` / `AQUA` in `serve.nu` if yours live elsewhere. `serve.nu`
  fails at load with a clear message if any binary is missing.
- `http-nu` on PATH, with `--store`, `--services`, and `--datastar`.

## Run

From the repo root:

    http-nu --dev --datastar --services --store ./store 127.0.0.1:5111 examples/cube/serve.nu

Add `--tls <pem>` to serve https. Then open the page and type into the front
(nu) face -- keystrokes (including Ctrl-C, arrows, tab) go to that shell.

## Faces

    0 front  nu (interactive)   3 left    asciiquarium
    1 right  boids_predator     4 top     mandelbrot
    2 back   mandelbrot         5 bottom  asciiquarium

Face layout is data: edit the `FACES` list in `serve.nu` to rearrange, resize, or
swap what each face runs.
