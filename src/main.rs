//! ptyZZZ: own one pty, speak JSONL on stdio. See PROTOCOL.md.
//!
//! Render path (cell -> HTML) is lifted from stacks2099/src/pty.rs, trimmed
//! to the v0 keyframe: full visible grid per coalesced tick, no diff/href/cursor.

use std::io::{BufRead, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use clap::Parser;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Deserialize;
use std::fmt::Write as _;
use wezterm_term::{
    color::{ColorAttribute, ColorPalette},
    CellAttributes, Intensity, StableRowIndex, Terminal, TerminalConfiguration, TerminalSize,
    Underline,
};

const SCROLLBACK_LINES: usize = 3000;
/// Coalesce window: collapse an output burst into one frame.
const COALESCE: Duration = Duration::from_millis(16);

#[derive(Parser)]
#[command(about = "own one pty, speak JSONL on stdio")]
struct Args {
    #[command(subcommand)]
    sub: Sub,
}

#[derive(clap::Subcommand)]
enum Sub {
    /// Open a pty and stream it as JSONL.
    Run {
        /// initial columns
        #[arg(long, default_value_t = 80)]
        cols: u16,
        /// initial rows
        #[arg(long, default_value_t = 24)]
        rows: u16,
        /// command to run (default: $SHELL or bash). Everything after `--`.
        #[arg(trailing_var_arg = true)]
        cmd: Vec<String>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum Cmd {
    Input { b: String },
    Resize { cols: u16, rows: u16 },
}

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

struct SharedWriter(Arc<Mutex<Box<dyn Write + Send>>>);
impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().flush()
    }
}

fn main() {
    let Sub::Run { cols, rows, cmd } = Args::parse().sub;
    let cmd = if cmd.is_empty() {
        vec![std::env::var("SHELL").unwrap_or_else(|_| "bash".into())]
    } else {
        cmd
    };

    let size = PtySize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = native_pty_system().openpty(size).expect("openpty");

    let mut builder = CommandBuilder::new(&cmd[0]);
    for a in &cmd[1..] {
        builder.arg(a);
    }
    for (k, v) in std::env::vars() {
        builder.env(k, v);
    }
    builder.env("TERM", "xterm-256color");
    builder.env("COLORTERM", "truecolor");
    if let Ok(cwd) = std::env::current_dir() {
        builder.cwd(cwd);
    }
    let mut child = pair.slave.spawn_command(builder).expect("spawn");
    drop(pair.slave);

    let writer: Arc<Mutex<Box<dyn Write + Send>>> =
        Arc::new(Mutex::new(pair.master.take_writer().expect("writer")));
    let mut reader = pair.master.try_clone_reader().expect("reader");

    let term = Terminal::new(
        TerminalSize {
            rows: size.rows as usize,
            cols: size.cols as usize,
            pixel_width: 0,
            pixel_height: 0,
            dpi: 0,
        },
        Arc::new(MinimalConfig),
        "ptyZZZ",
        "0",
        Box::new(SharedWriter(writer.clone())),
    );
    let term = Arc::new(Mutex::new(term));
    let dirty = Arc::new((Mutex::new(0u64), Condvar::new()));
    let done = Arc::new(AtomicBool::new(false));
    let master = Arc::new(Mutex::new(pair.master));

    // reader: drain pty -> feed wezterm -> bump dirty.
    {
        let term = term.clone();
        let dirty = dirty.clone();
        let done = done.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        term.lock().unwrap().advance_bytes(&buf[..n]);
                        bump(&dirty);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            done.store(true, Ordering::SeqCst);
            bump(&dirty);
        });
    }

    // stdin: JSONL commands -> pty.
    {
        let writer = writer.clone();
        let master = master.clone();
        let term = term.clone();
        let dirty = dirty.clone();
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Cmd>(&line) {
                    Ok(Cmd::Input { b }) => {
                        let mut w = writer.lock().unwrap();
                        let _ = w.write_all(b.as_bytes());
                        let _ = w.flush();
                    }
                    Ok(Cmd::Resize { cols, rows }) => {
                        let _ = master.lock().unwrap().resize(PtySize {
                            cols,
                            rows,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                        term.lock().unwrap().resize(TerminalSize {
                            rows: rows as usize,
                            cols: cols as usize,
                            pixel_width: 0,
                            pixel_height: 0,
                            dpi: 0,
                        });
                        bump(&dirty);
                    }
                    Err(_) => eprintln!("ptyZZZ: bad command: {line}"),
                }
            }
        });
    }

    // emitter: wait on dirty, coalesce, render visible grid -> stdout JSONL.
    let out = std::io::stdout();
    let mut last_gen = u64::MAX;
    loop {
        let (lock, cv) = &*dirty;
        {
            let mut g = lock.lock().unwrap();
            while *g == last_gen && !done.load(Ordering::SeqCst) {
                let (ng, _) = cv.wait_timeout(g, COALESCE).unwrap();
                g = ng;
            }
            last_gen = *g;
        }
        if !done.load(Ordering::SeqCst) {
            std::thread::sleep(COALESCE);
            let (lock, _) = &*dirty;
            last_gen = *lock.lock().unwrap();
        }

        let (seqno, cols, rows, html) = {
            let t = term.lock().unwrap();
            let (c, r, h) = render_visible(&t, "grid");
            (t.current_seqno(), c, r, h)
        };
        let line = serde_json::json!({"t":"screen","seqno":seqno,"cols":cols,"rows":rows,"html":html});
        let mut w = out.lock();
        let _ = writeln!(w, "{line}");
        let _ = w.flush();

        if done.load(Ordering::SeqCst) {
            break;
        }
    }

    let code = child.wait().map(|s| s.exit_code() as i64).unwrap_or(-1);
    let mut w = out.lock();
    let _ = writeln!(w, "{}", serde_json::json!({"t":"exit","code":code}));
    let _ = w.flush();
}

fn bump(dirty: &Arc<(Mutex<u64>, Condvar)>) {
    let (lock, cv) = &**dirty;
    {
        let mut g = lock.lock().unwrap();
        *g = g.wrapping_add(1);
    }
    cv.notify_all();
}

// --- render (lifted from stacks2099/src/pty.rs, trimmed) ---------------------

fn render_visible(term: &Terminal, target: &str) -> (usize, usize, String) {
    let size = term.get_size();
    let cols = size.cols;
    let rows = size.rows;
    let screen = term.screen();
    let total = screen.scrollback_rows();
    let start = total.saturating_sub(rows);
    let lines = screen.lines_in_phys_range(start..total);
    let default = CellAttributes::default();
    let mut out = String::new();
    let _ = write!(out, "<div id=\"{target}\" data-cols=\"{cols}\" data-rows=\"{rows}\">");
    for (i, line) in lines.iter().enumerate() {
        render_row_into(&mut out, target, line, cols, i as StableRowIndex, &default);
    }
    out.push_str("</div>");
    (cols, rows, out)
}

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
