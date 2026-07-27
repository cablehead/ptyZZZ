//! histbench: measure full-history HTML frames under streaming brotli.
//!
//! Scenario: render the ENTIRE scrollback (3000 lines) as one HTML frame per
//! tick, the way stacks2099 does, instead of ptyZZZ v0's visible-grid-only
//! keyframe. Questions:
//!   1. how big is a full-history frame, raw and brotli'd?
//!   2. when frame N+1 follows frame N on one SSE connection (one streaming
//!      brotli encoder, flush per frame -- http-nu's setup, quality 4), how
//!      small is the incremental cost of a frame that appends a few rows?
//!   3. what do render and compress cost the server per tick?
//!
//! Run: cargo run --release --example histbench
//!
//! Render helpers are copied from src/main.rs (the crate is a bin, not a lib).

use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::Arc;
use std::time::Instant;

use brotli::enc::backward_references::BrotliEncoderParams;
use brotli::enc::encode::{BrotliEncoderOperation, BrotliEncoderStateStruct};
use brotli::enc::StandardAlloc;
use wezterm_term::{
    color::{ColorAttribute, ColorPalette},
    CellAttributes, Intensity, StableRowIndex, Terminal, TerminalConfiguration, TerminalSize,
    Underline,
};

const SCROLLBACK_LINES: usize = 3000;
const COLS: usize = 84;
const ROWS: usize = 42;
const BROTLI_QUALITY: i32 = 4; // http-nu's setting

#[derive(Debug, Default)]
struct MinimalConfig;
impl TerminalConfiguration for MinimalConfig {
    fn color_palette(&self) -> ColorPalette {
        ColorPalette::default()
    }
    fn scrollback_size(&self) -> usize {
        SCROLLBACK_LINES
    }
}

// --- streaming brotli, one encoder per "connection", flush per frame --------

struct BrotliConn {
    encoder: BrotliEncoderStateStruct<StandardAlloc>,
    tmp: Vec<u8>,
}

impl BrotliConn {
    fn new() -> Self {
        let mut encoder = BrotliEncoderStateStruct::new(StandardAlloc::default());
        encoder.params = BrotliEncoderParams {
            quality: BROTLI_QUALITY,
            ..Default::default()
        };
        Self {
            encoder,
            tmp: vec![0u8; 16 * 1024],
        }
    }

    /// Feed one frame and flush; return the bytes this frame put on the wire.
    fn push_frame(&mut self, input: &[u8]) -> usize {
        let mut wire = 0usize;
        for op in [
            BrotliEncoderOperation::BROTLI_OPERATION_PROCESS,
            BrotliEncoderOperation::BROTLI_OPERATION_FLUSH,
        ] {
            let feed: &[u8] = if op == BrotliEncoderOperation::BROTLI_OPERATION_PROCESS {
                input
            } else {
                &[]
            };
            let mut in_offset = 0usize;
            loop {
                let mut avail_in = feed.len().saturating_sub(in_offset);
                let mut avail_out = self.tmp.len();
                let mut out_offset = 0usize;
                let ok = self.encoder.compress_stream(
                    op,
                    &mut avail_in,
                    &feed[in_offset..],
                    &mut in_offset,
                    &mut avail_out,
                    &mut self.tmp,
                    &mut out_offset,
                    &mut None,
                    &mut |_, _, _, _| (),
                );
                assert!(ok, "brotli compression failed");
                wire += out_offset;
                let done = match op {
                    BrotliEncoderOperation::BROTLI_OPERATION_FLUSH => {
                        !self.encoder.has_more_output()
                    }
                    _ => in_offset >= feed.len() && !self.encoder.has_more_output(),
                };
                if done {
                    break;
                }
            }
        }
        wire
    }
}

// --- synthetic shell output -------------------------------------------------

/// Deterministic LCG so runs are reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// One line of plausible build/test output: mixed plain text, ANSI color,
/// varying tokens. Realistic enough that compression isn't measuring pure
/// repetition.
fn synth_line(rng: &mut Rng, n: usize) -> String {
    let words = ["parser", "render", "stream", "socket", "buffer", "cursor", "keymap", "worker"];
    let w1 = words[(rng.next() % 8) as usize];
    let w2 = words[(rng.next() % 8) as usize];
    match rng.next() % 5 {
        0 => format!(
            "\x1b[32m   ok\x1b[0m  {w1}::{w2}::test_{:04x} ... {}ms\r\n",
            rng.next() % 0xffff,
            rng.next() % 900
        ),
        1 => format!(
            "\x1b[33mwarn\x1b[0m  src/{w1}/{w2}.rs:{}: unused variable `{w2}_{:x}`\r\n",
            rng.next() % 2000,
            rng.next() % 0xff
        ),
        2 => format!(
            "-rw-r--r--  1 app app {:>8} Jul 26 10:{:02} {w1}_{w2}_{:04x}.log\r\n",
            rng.next() % 900000,
            rng.next() % 60,
            rng.next() % 0xffff
        ),
        3 => format!(
            "[{n:04}] GET /api/{w1}/{:x} \x1b[36m200\x1b[0m {}b {}us\r\n",
            rng.next() % 0xfffff,
            rng.next() % 40000,
            rng.next() % 9000
        ),
        _ => format!(
            "compiling {w1}-{w2} v0.{}.{} ({} deps, {} units)\r\n",
            rng.next() % 20,
            rng.next() % 40,
            rng.next() % 300,
            rng.next() % 90
        ),
    }
}

// --- full-history render ----------------------------------------------------

/// Render the whole scrollback + visible screen, rows keyed by wezterm's
/// stable row index (stacks2099-style), so ids stay pinned to content as the
/// buffer scrolls.
fn render_full(term: &Terminal, target: &str) -> String {
    let size = term.get_size();
    let cols = size.cols;
    let screen = term.screen();
    let total = screen.scrollback_rows();
    let stable_base = screen.phys_to_stable_row_index(0);
    let lines = screen.lines_in_phys_range(0..total);
    let default = CellAttributes::default();
    let mut out = String::new();
    let _ = write!(
        out,
        "<div id=\"{target}\" data-cols=\"{cols}\" data-rows=\"{}\">",
        size.rows
    );
    for (i, line) in lines.iter().enumerate() {
        render_row_into(&mut out, target, line, cols, stable_base + i as StableRowIndex, &default);
    }
    out.push_str("</div>");
    out
}

// --- copied verbatim from src/main.rs ---------------------------------------

fn attrs_equiv(a: &CellAttributes, b: &CellAttributes) -> bool {
    let bits = if a.wrapped() == b.wrapped() {
        a.attribute_bits_equal(b)
    } else {
        let (mut a, mut b) = (a.clone(), b.clone());
        a.set_wrapped(false);
        b.set_wrapped(false);
        a.attribute_bits_equal(&b)
    };
    bits && a.foreground() == b.foreground() && a.background() == b.background()
}

fn palette_to_rgb(i: u8) -> (u8, u8, u8) {
    const P16: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (0xcd, 0, 0),
        (0, 0xcd, 0),
        (0xcd, 0xcd, 0),
        (0x1e, 0x90, 0xff),
        (0xcd, 0, 0xcd),
        (0, 0xcd, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x4d, 0x4d, 0x4d),
        (0xff, 0x54, 0x54),
        (0x54, 0xff, 0x54),
        (0xff, 0xff, 0x54),
        (0x54, 0x54, 0xff),
        (0xff, 0x54, 0xff),
        (0x54, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    if i < 16 {
        return P16[i as usize];
    }
    if i < 232 {
        let n = i - 16;
        let to_v = |c: u8| if c == 0 { 0 } else { 55 + c * 40 };
        return (to_v((n / 36) % 6), to_v((n / 6) % 6), to_v(n % 6));
    }
    let l = (8u16 + (i as u16 - 232) * 10).min(255) as u8;
    (l, l, l)
}

fn append_color_inline(out: &mut String, prop: &str, c: ColorAttribute, default_var: &str) {
    match c {
        ColorAttribute::Default => {
            if !default_var.is_empty() {
                let _ = write!(out, "{prop}:var({default_var});");
            }
        }
        ColorAttribute::PaletteIndex(i) if i < 16 => {
            let _ = write!(out, "{prop}:var(--c{i});");
        }
        ColorAttribute::PaletteIndex(i) => {
            let (r, g, b) = palette_to_rgb(i);
            let _ = write!(out, "{prop}:#{r:02x}{g:02x}{b:02x};");
        }
        ColorAttribute::TrueColorWithDefaultFallback(rgb)
        | ColorAttribute::TrueColorWithPaletteFallback(rgb, _) => {
            let r = (rgb.0 * 255.0).round() as u8;
            let g = (rgb.1 * 255.0).round() as u8;
            let b = (rgb.2 * 255.0).round() as u8;
            let _ = write!(out, "{prop}:#{r:02x}{g:02x}{b:02x};");
        }
    }
}

fn cell_class_and_style(attrs: &CellAttributes) -> (String, String) {
    let mut classes = String::new();
    let mut style = String::new();
    match attrs.intensity() {
        Intensity::Bold => classes.push_str(" sb"),
        Intensity::Half => classes.push_str(" sd"),
        Intensity::Normal => {}
    }
    if attrs.italic() {
        classes.push_str(" si");
    }
    if !matches!(attrs.underline(), Underline::None) {
        classes.push_str(" su");
    }
    if attrs.invisible() {
        classes.push_str(" sx");
    }
    if attrs.strikethrough() {
        classes.push_str(" ss");
    }
    if attrs.reverse() {
        append_color_inline(&mut style, "color", attrs.background(), "--term-bg");
        append_color_inline(&mut style, "background", attrs.foreground(), "--term-fg");
    } else {
        match attrs.foreground() {
            ColorAttribute::Default => {}
            ColorAttribute::PaletteIndex(i) if i < 16 => {
                let _ = write!(classes, " f{i}");
            }
            other => append_color_inline(&mut style, "color", other, ""),
        }
        match attrs.background() {
            ColorAttribute::Default => {}
            ColorAttribute::PaletteIndex(i) if i < 16 => {
                let _ = write!(classes, " b{i}");
            }
            other => append_color_inline(&mut style, "background", other, ""),
        }
    }
    (classes, style)
}

fn html_escape(s: &str, out: &mut String) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

fn cell_needs_box(glyph: &str, width: usize) -> bool {
    width != 1 || glyph.chars().count() > 1
}

fn render_row_into(
    out: &mut String,
    target: &str,
    line: &wezterm_term::Line,
    cols: usize,
    stable: StableRowIndex,
    default_attrs: &CellAttributes,
) {
    let _ = write!(out, "<div class=\"row\" id=\"{target}-r-{stable}\">");
    struct Tok {
        text: String,
        attrs: CellAttributes,
        boxed: bool,
        width: usize,
    }
    let space = |a: &CellAttributes| Tok {
        text: " ".into(),
        attrs: a.clone(),
        boxed: false,
        width: 1,
    };
    let mut toks: Vec<Tok> = Vec::with_capacity(cols);
    let mut expected = 0usize;
    for cell in line.visible_cells() {
        let col = cell.cell_index();
        if col >= cols {
            break;
        }
        if col < expected {
            continue;
        }
        while expected < col {
            toks.push(space(default_attrs));
            expected += 1;
        }
        let width = cell.width().max(1);
        let s = cell.str();
        let glyph = if s.is_empty() { " ".to_string() } else { s.to_string() };
        toks.push(Tok {
            boxed: cell_needs_box(&glyph, width),
            text: glyph,
            attrs: cell.attrs().clone(),
            width,
        });
        expected = col + width;
    }
    while expected < cols {
        toks.push(space(default_attrs));
        expected += 1;
    }

    let mut i = 0;
    while i < toks.len() {
        let a = &toks[i];
        let mut j = i + 1;
        while j < toks.len() && attrs_equiv(&a.attrs, &toks[j].attrs) {
            j += 1;
        }
        let (classes, style) = cell_class_and_style(&a.attrs);
        let styled = !classes.is_empty() || !style.is_empty();
        if styled {
            out.push_str("<span class=\"c");
            out.push_str(&classes);
            out.push('"');
            if !style.is_empty() {
                let _ = write!(out, " style=\"{style}\"");
            }
            out.push('>');
        }
        for t in &toks[i..j] {
            if t.boxed {
                let _ = write!(out, "<span class=\"wc\" style=\"--w:{}\">", t.width);
                html_escape(&t.text, out);
                out.push_str("</span>");
            } else {
                html_escape(&t.text, out);
            }
        }
        if styled {
            out.push_str("</span>");
        }
        i = j;
    }
    out.push_str("</div>");
}

fn main() {
    let mut term = Terminal::new(
        TerminalSize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
            dpi: 0,
        },
        Arc::new(MinimalConfig),
        "histbench",
        "0",
        Box::new(std::io::sink()),
    );

    // Saturate scrollback: 3000 lines of history + a full visible screen.
    let mut rng = Rng(42);
    let mut hist = String::new();
    for n in 0..(SCROLLBACK_LINES + ROWS) {
        hist.push_str(&synth_line(&mut rng, n));
    }
    hist.push_str("\x1b[32mapp\x1b[0m:~/ptyZZZ$ ");
    term.advance_bytes(hist.as_bytes());

    let t = Instant::now();
    let frame_a = render_full(&term, "grid");
    let render_a = t.elapsed();

    // One new command: echo + 12 lines of output + fresh prompt.
    let mut cmd = String::from("ls -la\r\n");
    for n in 0..12 {
        cmd.push_str(&synth_line(&mut rng, n));
    }
    cmd.push_str("\x1b[32mapp\x1b[0m:~/ptyZZZ$ ");
    term.advance_bytes(cmd.as_bytes());

    let t = Instant::now();
    let frame_b = render_full(&term, "grid");
    let render_b = t.elapsed();

    // A heavier follow-up: a command that scrolls 200 lines through.
    let mut big = String::from("make test\r\n");
    for n in 0..200 {
        big.push_str(&synth_line(&mut rng, n));
    }
    big.push_str("\x1b[32mapp\x1b[0m:~/ptyZZZ$ ");
    term.advance_bytes(big.as_bytes());

    let t = Instant::now();
    let frame_c = render_full(&term, "grid");
    let render_c = t.elapsed();

    // One SSE connection: stream A, then B, then C through one encoder.
    let mut conn = BrotliConn::new();
    let t = Instant::now();
    let wire_a = conn.push_frame(frame_a.as_bytes());
    let comp_a = t.elapsed();
    let t = Instant::now();
    let wire_b = conn.push_frame(frame_b.as_bytes());
    let comp_b = t.elapsed();
    let t = Instant::now();
    let wire_c = conn.push_frame(frame_c.as_bytes());
    let comp_c = t.elapsed();

    // Cold subscriber: frame B alone on a fresh encoder.
    let mut cold = BrotliConn::new();
    let wire_b_cold = cold.push_frame(frame_b.as_bytes());

    // Optional: dump the frames for browser-side benchmarking.
    if let Some(dir) = std::env::args().nth(1) {
        for (name, frame) in [("a", &frame_a), ("b", &frame_b), ("c", &frame_c)] {
            std::fs::write(format!("{dir}/frame_{name}.html"), frame).expect("dump frame");
        }
    }

    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "grid {COLS}x{ROWS}, scrollback {SCROLLBACK_LINES}, brotli q{BROTLI_QUALITY} (streaming, flush per frame)");
    let _ = writeln!(out);
    let row = |name: &str, raw: usize, wire: usize, render: std::time::Duration, comp: std::time::Duration| {
        format!(
            "{name:<28} {:>9} raw  {:>8} wire  {:>7.1}x  render {:>6.2?}  compress {:>6.2?}",
            raw, wire, raw as f64 / wire as f64, render, comp
        )
    };
    let _ = writeln!(out, "{}", row("A: 3000-line history", frame_a.len(), wire_a, render_a, comp_a));
    let _ = writeln!(out, "{}", row("B: A + one command (13 rows)", frame_b.len(), wire_b, render_b, comp_b));
    let _ = writeln!(out, "{}", row("C: B + 200-line command", frame_c.len(), wire_c, render_c, comp_c));
    let _ = writeln!(out);
    let _ = writeln!(out, "B on a fresh connection (cold subscriber): {} bytes wire", wire_b_cold);
    let _ = writeln!(
        out,
        "for scale: the 13 rows B actually changed, rendered alone: {} bytes raw",
        13 * (frame_b.len() / (SCROLLBACK_LINES + ROWS))
    );
}
