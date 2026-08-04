//! scanbench: per-tick cost breakdown for the diff path, steady state.
//!
//! histbench covers keyframes (full-history render + brotli). This covers the
//! other 99% of ticks: a few lines of new output arrive, we scan for damage,
//! and re-render only the damaged rows. Questions:
//!   1. what does `get_changed_stable_rows` over the full scrollback cost?
//!   2. what would it cost clamped to the viewport (scrolled-off rows can't
//!      change outside resize/trim, which force a full re-render anyway)?
//!   3. what % of a tick (parse + scan + render) is the scan?
//!
//! Run: cargo run --release --example scanbench
//!
//! Render helpers are copied from src/main.rs (the crate is a bin, not a lib).

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use wezterm_term::{
    color::{ColorAttribute, ColorPalette},
    CellAttributes, Intensity, StableRowIndex, Terminal, TerminalConfiguration, TerminalSize,
    Underline,
};

const SCROLLBACK_LINES: usize = 3000;
const COLS: usize = 84;
const ROWS: usize = 42;
const TICKS: usize = 2000;

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

// --- synthetic shell output (same generator as histbench) --------------------

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

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

// --- copied verbatim from src/main.rs ---------------------------------------

fn attrs_equiv(a: &CellAttributes, b: &CellAttributes) -> bool {
    a.intensity() == b.intensity()
        && a.italic() == b.italic()
        && a.underline() == b.underline()
        && a.invisible() == b.invisible()
        && a.strikethrough() == b.strikethrough()
        && a.reverse() == b.reverse()
        && a.foreground() == b.foreground()
        && a.background() == b.background()
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
) {
    let _ = write!(out, "<div class=\"row\" id=\"{target}-r-{stable}\">");
    let default = CellAttributes::default();
    let mut expected = 0usize;
    let mut pending_blanks = 0usize;
    let mut span_open = false;
    let mut run_attrs: Option<CellAttributes> = None;

    for cell in line.visible_cells() {
        let col = cell.cell_index();
        if col >= cols {
            break;
        }
        if col < expected {
            continue;
        }
        pending_blanks += col - expected;
        let width = cell.width().max(1);
        expected = col + width;
        let s = cell.str();
        let attrs = cell.attrs();

        let plain = attrs_equiv(attrs, &default);
        if plain && width == 1 && (s == " " || s.is_empty()) {
            pending_blanks += 1;
            continue;
        }
        if pending_blanks > 0 {
            if span_open {
                out.push_str("</span>");
                span_open = false;
                run_attrs = None;
            }
            out.extend(std::iter::repeat(' ').take(pending_blanks));
            pending_blanks = 0;
        }
        if plain {
            if span_open {
                out.push_str("</span>");
                span_open = false;
                run_attrs = None;
            }
        } else {
            let same = run_attrs.as_ref().is_some_and(|r| attrs_equiv(r, attrs));
            if !same {
                if span_open {
                    out.push_str("</span>");
                }
                let (classes, style) = cell_class_and_style(attrs);
                if classes.is_empty() && style.is_empty() {
                    span_open = false;
                    run_attrs = None;
                } else {
                    out.push_str("<span class=\"c");
                    out.push_str(&classes);
                    out.push('"');
                    if !style.is_empty() {
                        let _ = write!(out, " style=\"{style}\"");
                    }
                    out.push('>');
                    span_open = true;
                    run_attrs = Some(attrs.clone());
                }
            }
        }
        let glyph = if s.is_empty() { " " } else { s };
        if cell_needs_box(glyph, width) {
            let _ = write!(out, "<span class=\"wc\" style=\"--w:{width}\">");
            html_escape(glyph, out);
            out.push_str("</span>");
        } else {
            html_escape(glyph, out);
        }
    }
    if span_open {
        out.push_str("</span>");
    }
    out.push_str("</div>");
}

// --- bench ------------------------------------------------------------------

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
        "scanbench",
        "0",
        Box::new(std::io::sink()),
    );

    // Saturate scrollback so every tick runs at full history depth.
    let mut rng = Rng(42);
    let mut hist = String::new();
    for n in 0..(SCROLLBACK_LINES + ROWS) {
        hist.push_str(&synth_line(&mut rng, n));
    }
    term.advance_bytes(hist.as_bytes());

    let mut t_parse = Duration::ZERO;
    let mut t_scan_full = Duration::ZERO;
    let mut t_scan_vp = Duration::ZERO;
    let mut t_render = Duration::ZERO;
    let mut damaged_full = 0usize;
    let mut damaged_vp = 0usize;
    let mut rendered_bytes = 0usize;
    let mut html = String::new();

    let mut last_seqno = term.current_seqno();
    for n in 0..TICKS {
        // One tick of steady-state output: 1-3 new lines.
        let mut chunk = String::new();
        for _ in 0..(1 + rng.next() % 3) {
            chunk.push_str(&synth_line(&mut rng, n));
        }

        let t = Instant::now();
        term.advance_bytes(chunk.as_bytes());
        t_parse += t.elapsed();

        let seqno = term.current_seqno();
        let screen = term.screen();
        let total = screen.scrollback_rows();
        let base = screen.phys_to_stable_row_index(0);
        let max = base + total as StableRowIndex;

        // Today's scan: the whole scrollback.
        let t = Instant::now();
        let full = screen.get_changed_stable_rows(base..max, last_seqno);
        t_scan_full += t.elapsed();
        damaged_full += full.len();

        // Proposed: viewport only (scrolled-off rows can't change here).
        let vp_start = max.saturating_sub(ROWS as StableRowIndex).max(base);
        let t = Instant::now();
        let vp = screen.get_changed_stable_rows(vp_start..max, last_seqno);
        t_scan_vp += t.elapsed();
        damaged_vp += vp.len();
        assert_eq!(full, vp, "viewport scan missed damage outside the viewport");

        // Render the damaged rows, as produce() would.
        let t = Instant::now();
        for &stable in &full {
            let phys = (stable - base) as usize;
            let line = &screen.lines_in_phys_range(phys..phys + 1)[0];
            html.clear();
            render_row_into(&mut html, "grid", line, COLS, stable);
            rendered_bytes += html.len();
        }
        t_render += t.elapsed();

        last_seqno = seqno;
    }

    let per = |d: Duration| d / TICKS as u32;
    let tick_now = t_parse + t_scan_full + t_render;
    let tick_opt = t_parse + t_scan_vp + t_render;
    let pct = |part: Duration, whole: Duration| 100.0 * part.as_secs_f64() / whole.as_secs_f64();

    println!(
        "grid {COLS}x{ROWS}, scrollback {SCROLLBACK_LINES}, {TICKS} ticks, 1-3 lines/tick"
    );
    println!(
        "damaged rows/tick: {:.1} (viewport scan found the same {} rows; assert passed)",
        damaged_full as f64 / TICKS as f64,
        damaged_vp
    );
    println!("rendered {:.1} bytes/tick", rendered_bytes as f64 / TICKS as f64);
    println!();
    println!("per-tick averages:");
    println!("  parse (advance_bytes)      {:>9.2?}", per(t_parse));
    println!(
        "  scan full ({} rows)      {:>9.2?}  = {:.0}% of tick",
        SCROLLBACK_LINES + ROWS,
        per(t_scan_full),
        pct(t_scan_full, tick_now)
    );
    println!(
        "  scan viewport ({} rows)    {:>9.2?}  = {:.0}% of tick",
        ROWS,
        per(t_scan_vp),
        pct(t_scan_vp, tick_opt)
    );
    println!("  render damaged rows        {:>9.2?}", per(t_render));
    println!();
    println!(
        "tick today {:.2?} -> clamped {:.2?} ({:.1}x)",
        per(tick_now),
        per(tick_opt),
        tick_now.as_secs_f64() / tick_opt.as_secs_f64()
    );
}
