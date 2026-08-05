//! ptyZZZ: own one pty, speak JSONL on stdio. See PROTOCOL.md.
//!
//! v1 renders the full scrollback (not just the visible grid) and tracks
//! damage per row. Each emit is either a keyframe (`screen`: the whole grid as
//! one div, the join point for new subscribers) or a diff (`diff`: changed
//! rows, appended rows, trimmed row ids, cursor). Rows are keyed by wezterm's
//! stable row index, rendered once into a cache, and re-rendered only when
//! wezterm reports the line changed; re-rendered rows are byte-compared
//! against the cache so output that changes nothing visible emits nothing.

use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use clap::Parser;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Deserialize;
use std::fmt::Write as _;
use wezterm_surface::hyperlink::{
    Rule, CLOSING_PARENTHESIS_HYPERLINK_PATTERN, GENERIC_HYPERLINK_PATTERN,
};
use wezterm_term::{
    color::{ColorAttribute, ColorPalette},
    CellAttributes, Intensity, Line, StableRowIndex, Terminal, TerminalConfiguration,
    TerminalSize, Underline,
};

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
        /// element id to render the grid into (container + row ids)
        #[arg(long, default_value = "grid")]
        target: String,
        /// coalesce window in ms; caps frame rate (~1000/ms). Higher = fewer,
        /// coarser frames -- lighter on the client and the fan-out.
        #[arg(long, default_value_t = 16)]
        coalesce: u64,
        /// seconds between healing keyframes while diffs are flowing; a full
        /// keyframe re-syncs every subscriber and bounds joiner catch-up
        #[arg(long, default_value_t = 5)]
        keyframe_interval: u64,
        /// scrollback lines kept and rendered (0 = visible screen only)
        #[arg(long, default_value_t = 3000)]
        scrollback: usize,
        /// tee the raw pty byte stream to this file (for bench corpora)
        #[arg(long)]
        record: Option<std::path::PathBuf>,
        /// command to run (default: $SHELL or nu). Everything after `--`.
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

#[derive(Debug)]
struct MinimalConfig {
    scrollback: usize,
}
impl TerminalConfiguration for MinimalConfig {
    fn color_palette(&self) -> ColorPalette {
        ColorPalette::default()
    }
    fn scrollback_size(&self) -> usize {
        self.scrollback
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
    let Sub::Run { cols, rows, target, coalesce, keyframe_interval, scrollback, record, cmd } =
        Args::parse().sub;
    let coalesce = Duration::from_millis(coalesce);
    let keyframe_interval = Duration::from_secs(keyframe_interval.max(1));
    let cmd = if cmd.is_empty() {
        vec![std::env::var("SHELL").unwrap_or_else(|_| "nu".into())]
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
        Arc::new(MinimalConfig { scrollback }),
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
        let mut record = record.map(|p| std::fs::File::create(p).expect("record file"));
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Some(f) = record.as_mut() {
                            let _ = f.write_all(&buf[..n]);
                        }
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

    // emitter: wait on dirty (or an owed keyframe), coalesce, emit frames.
    let out = std::io::stdout();
    let mut state = EmitState::new(target);
    let mut last_gen = u64::MAX; // != 0 so the first pass emits immediately
    // Cost of building+writing the previous frame; the coalesce sleep is
    // shortened by this much so continuous output emits on a steady
    // ~coalesce cadence instead of coalesce + frame cost.
    let mut frame_cost = Duration::ZERO;
    loop {
        {
            let (lock, cv) = &*dirty;
            let mut g = lock.lock().unwrap();
            loop {
                if *g != last_gen || done.load(Ordering::SeqCst) {
                    break;
                }
                let due_in = if state.dirty_since_keyframe {
                    keyframe_interval.saturating_sub(state.last_keyframe.elapsed())
                } else {
                    Duration::from_secs(3600)
                };
                if due_in.is_zero() {
                    break;
                }
                let (ng, _) = cv.wait_timeout(g, due_in.min(Duration::from_secs(1))).unwrap();
                g = ng;
            }
            last_gen = *g;
        }
        if !done.load(Ordering::SeqCst) {
            std::thread::sleep(coalesce.saturating_sub(frame_cost));
            let (lock, _) = &*dirty;
            last_gen = *lock.lock().unwrap();
        }

        let keyframe_due =
            state.dirty_since_keyframe && state.last_keyframe.elapsed() >= keyframe_interval;
        let started = Instant::now();
        let frame = {
            let mut t = term.lock().unwrap();
            state.produce(&mut t, keyframe_due)
        };
        if let Some(v) = frame {
            let mut w = out.lock();
            // A dead stdout means no one is listening (the adapter that
            // spawned us is gone): exit instead of rendering forever.
            if writeln!(w, "{v}").and_then(|_| w.flush()).is_err() {
                let _ = child.kill();
                break;
            }
        }
        frame_cost = started.elapsed();

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

// --- frame production --------------------------------------------------------

struct EmitState {
    target: String,
    /// rendered row html by stable row index; the keyframe is the ordered
    /// concatenation of this map
    cache: BTreeMap<StableRowIndex, String>,
    last_seqno: usize,
    last_base: StableRowIndex,
    last_max: StableRowIndex,
    last_cols: usize,
    last_rows: usize,
    last_alt: bool,
    last_cursor: (usize, usize),
    sent_initial: bool,
    last_keyframe: Instant,
    dirty_since_keyframe: bool,
    /// seqno of the last implicit-hyperlink scan
    scan_seqno: usize,
}

impl EmitState {
    fn new(target: String) -> Self {
        Self {
            target,
            cache: BTreeMap::new(),
            last_seqno: 0,
            last_base: 0,
            last_max: 0,
            last_cols: 0,
            last_rows: 0,
            last_alt: false,
            last_cursor: (usize::MAX, usize::MAX),
            sent_initial: false,
            last_keyframe: Instant::now(),
            dirty_since_keyframe: false,
            scan_seqno: 0,
        }
    }

    /// Inspect the terminal, update the row cache, and produce the next frame:
    /// a keyframe, a diff, or None when nothing visible changed.
    fn produce(&mut self, term: &mut Terminal, keyframe_due: bool) -> Option<serde_json::Value> {
        // Implicit-link scan first (it mutates cell attrs on changed lines),
        // clamped to the rows that can still change.
        let scan_lo = self
            .sent_initial
            .then(|| self.last_max - self.last_rows as StableRowIndex);
        self.scan_seqno = scan_hyperlinks(term, self.scan_seqno, scan_lo);

        let seqno = term.current_seqno();
        let alt = term.is_alt_screen_active();
        let size = term.get_size();
        let (cols, rows) = (size.cols, size.rows);
        let cursor = term.cursor_pos();
        let screen = term.screen();
        let total = screen.scrollback_rows();
        let base = screen.phys_to_stable_row_index(0);
        let max = base + total as StableRowIndex;
        let cursor_now = (
            total.saturating_sub(rows) + cursor.y.max(0) as usize,
            cursor.x,
        );

        // A resize reflows rows and an alt-screen flip swaps line storage;
        // both invalidate the seqno diff basis, so re-render everything.
        let forced = !self.sent_initial
            || alt != self.last_alt
            || cols != self.last_cols
            || rows != self.last_rows;

        // Damage since the last emit, limited to rows the client still has.
        // New rows are the append path; trimmed rows fell off the scrollback.
        // Rows already in scrollback at the last scan are frozen -- apps
        // can't address them, and the reflow/alt-flip cases that could move
        // them force a full re-render above -- so only rows that were in
        // the viewport at the last emit can have changed. Scanning from the
        // old viewport top instead of the scrollback base keeps this
        // O(rows), not O(scrollback); the old top also covers rows damaged
        // and then scrolled off within one coalesce window.
        let overlap_end = max.min(self.last_max);
        let vp_start = (self.last_max - self.last_rows as StableRowIndex).max(base);
        let damaged: Vec<StableRowIndex> = if !forced && overlap_end > vp_start {
            screen.get_changed_stable_rows(vp_start..overlap_end, self.last_seqno)
        } else {
            Vec::new()
        };
        let appended: Vec<StableRowIndex> = if forced {
            Vec::new()
        } else {
            (self.last_max.max(base)..max).collect()
        };
        let trimmed: Vec<StableRowIndex> = if forced {
            Vec::new()
        } else {
            (self.last_base..base.min(self.last_max)).collect()
        };

        let cursor_moved = cursor_now != self.last_cursor;
        if !forced
            && !keyframe_due
            && damaged.is_empty()
            && appended.is_empty()
            && trimmed.is_empty()
            && !cursor_moved
        {
            self.last_seqno = seqno;
            return None;
        }

        // Render damaged rows into the cache. A row whose new html matches the
        // cache byte-for-byte was touched but not visibly changed (a prompt
        // redraw writing identical cells); drop it from the diff.
        let mut changed: Vec<StableRowIndex> = Vec::new();
        if forced {
            self.cache.clear();
            let lines = screen.lines_in_phys_range(0..total);
            for (i, line) in lines.iter().enumerate() {
                let stable = base + i as StableRowIndex;
                let mut html = String::with_capacity(64);
                render_row_into(&mut html, &self.target, line, cols, stable);
                self.cache.insert(stable, html);
            }
        } else {
            self.cache = self.cache.split_off(&base);
            for &stable in damaged.iter().chain(appended.iter()) {
                let phys = (stable - base) as usize;
                let line = &screen.lines_in_phys_range(phys..phys + 1)[0];
                let mut html = String::with_capacity(64);
                render_row_into(&mut html, &self.target, line, cols, stable);
                let fresh = self.cache.get(&stable) != Some(&html);
                if fresh {
                    self.cache.insert(stable, html);
                    if stable < self.last_max {
                        changed.push(stable);
                    }
                }
            }
        }

        if !forced
            && !keyframe_due
            && changed.is_empty()
            && appended.is_empty()
            && trimmed.is_empty()
            && !cursor_moved
        {
            self.last_seqno = seqno;
            return None;
        }

        // A burst that touched most rows is cheaper as a keyframe than as a
        // stack of row patches, and it doubles as the healing checkpoint.
        let big = changed.len() + appended.len() > total / 2;
        let frame = if forced || big || keyframe_due {
            let body: usize = self.cache.values().map(|s| s.len()).sum();
            let mut html = String::with_capacity(body + 256);
            let _ = write!(
                html,
                "<div id=\"{}\" data-cols=\"{cols}\" data-rows=\"{rows}\">",
                self.target
            );
            render_cursor_into(&mut html, &self.target, cursor_now.0, cursor_now.1);
            for row in self.cache.values() {
                html.push_str(row);
            }
            html.push_str("</div>");
            self.last_keyframe = Instant::now();
            self.dirty_since_keyframe = false;
            serde_json::json!({"t":"screen","seqno":seqno,"cols":cols,"rows":rows,"html":html})
        } else {
            let mut patch = String::new();
            for stable in &changed {
                patch.push_str(&self.cache[stable]);
            }
            if cursor_moved {
                render_cursor_into(&mut patch, &self.target, cursor_now.0, cursor_now.1);
            }
            let mut append = String::new();
            for stable in &appended {
                append.push_str(&self.cache[stable]);
            }
            let trim: Vec<String> = trimmed
                .iter()
                .map(|s| format!("{}-r-{}", self.target, s))
                .collect();
            self.dirty_since_keyframe = true;
            serde_json::json!({
                "t":"diff","seqno":seqno,"target":self.target,
                "patch":patch,"append":append,"trim":trim
            })
        };

        self.last_seqno = seqno;
        self.last_base = base;
        self.last_max = max;
        self.last_cols = cols;
        self.last_rows = rows;
        self.last_alt = alt;
        self.last_cursor = cursor_now;
        self.sent_initial = true;
        Some(frame)
    }
}

// --- hyperlinks: engine-agnostic policy --------------------------------------
// safe_href and the anchor emission in render_row_into work on plain strings
// and know nothing about the emulator; only the scan below is wezterm-bound.

/// Return the URI to use as an `href`, or None if its scheme isn't one we'll
/// make clickable. OSC 8 can carry any scheme (including `javascript:` and
/// `data:`), so gate every link through this allowlist before it reaches the
/// DOM. Our implicit rules only ever emit http/https/mailto anyway.
fn safe_href(uri: &str) -> Option<&str> {
    let u = uri.trim();
    let ok = ["http://", "https://", "mailto:"]
        .iter()
        .any(|p| u.len() >= p.len() && u.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes()));
    ok.then_some(u)
}

// --- hyperlinks: wezterm implicit-URL scan -----------------------------------

/// URL detection rules for bare (non-OSC-8) text. This is wezterm's own
/// `default_hyperlink_rules` set, rebuilt from the patterns wezterm-surface
/// exports. Compiled once.
///
/// The bracket rules link the URL without its surrounding `()`/`[]`/`<>`
/// (capture group 1 is the highlighted range). The generic pattern ends in
/// `[_/a-zA-Z0-9-]`, so trailing prose punctuation (`.`, `,`, `)`) is left
/// out of the link while a real trailing `/` or `-` is kept -- and it has no
/// TLD requirement, so `http://localhost:8080` links too.
fn hyperlink_rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES
        .get_or_init(|| {
            vec![
                // URLs wrapped in brackets: link the inner URL, not the bracket.
                Rule::with_highlight(r"\((\w+://\S+)\)", "$1", 1).unwrap(),
                Rule::with_highlight(r"\[(\w+://\S+)\]", "$1", 1).unwrap(),
                Rule::with_highlight(r"<(\w+://\S+)>", "$1", 1).unwrap(),
                // Bare URLs: balanced closing paren, then the generic form.
                Rule::new(CLOSING_PARENTHESIS_HYPERLINK_PATTERN, "$0").unwrap(),
                Rule::new(GENERIC_HYPERLINK_PATTERN, "$0").unwrap(),
                // Implicit mailto.
                Rule::new(r"\b\w+@[\w-]+(\.[\w-]+)+\b", "mailto:$0").unwrap(),
            ]
        })
        .as_slice()
}

/// Attach implicit-hyperlink attributes to the logical lines whose rows
/// changed since `since`. wezterm groups wrapped physical rows into one
/// logical line (and `apply_hyperlink_rules` writes the attribute back onto
/// each physical row's cells), so a URL that wraps across the right edge is
/// linked across the rows it spans. `lo` clamps the scan to rows that can
/// still change (the old viewport top, same frozen-scrollback argument as
/// the damage scan in `produce`); None scans everything. Returns the seqno
/// to pass as `since` next time; wezterm's per-line scanned bits mean only
/// genuinely-changed lines re-scan.
fn scan_hyperlinks(term: &mut Terminal, since: usize, lo: Option<StableRowIndex>) -> usize {
    let seqno = term.current_seqno();
    let screen = term.screen_mut();
    let base = screen.phys_to_stable_row_index(0);
    let total = screen.scrollback_rows() as StableRowIndex;
    if total == 0 {
        return seqno;
    }
    let lo = lo.unwrap_or(base).max(base);
    let changed = screen.get_changed_stable_rows(lo..base + total, since);
    if let (Some(&a), Some(&b)) = (changed.first(), changed.last()) {
        screen.for_each_logical_line_in_stable_range_mut(a..b + 1, |_, lines| {
            // Rule scan only where it can matter: the text contains a URL /
            // mailto marker, or a cell still carries a link that a rescan
            // may need to clear. URL-free output (the common case) pays a
            // substring check instead of six regexes.
            let candidate = lines.iter().any(|l| {
                let s = l.as_str();
                s.contains("://") || s.contains('@')
            }) || lines
                .iter()
                .any(|l| l.visible_cells().any(|c| c.attrs().hyperlink().is_some()));
            if candidate {
                Line::apply_hyperlink_rules(hyperlink_rules(), lines);
            }
            true
        });
    }
    seqno
}

// --- render ------------------------------------------------------------------

/// The cursor is its own self-identified overlay, positioned purely by CSS
/// vars, so a cursor move patches ~90 bytes instead of touching any row.
fn render_cursor_into(out: &mut String, target: &str, row: usize, col: usize) {
    let _ = write!(
        out,
        "<div class=\"cursor\" id=\"{target}-cursor\" style=\"--cursor-row:{row};--cursor-col:{col}\"></div>"
    );
}

/// Equivalence over exactly the attributes the renderer maps to output.
/// Deliberately ignores non-rendered bits (blink, hyperlinks, wrap) so runs
/// don't split on invisible differences.
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

/// One streaming pass over the row's cells: runs of equivalent attrs AND the
/// same link target share one wrapper (`<a>` when linked, `<span>` when only
/// styled), attrs are cloned only at run boundaries, and no per-cell
/// allocation happens beyond the run's href. Default-attr blanks are buffered
/// so trailing ones are dropped -- `white-space:pre` plus `.row{min-height}`
/// keep the geometry without them.
fn render_row_into(
    out: &mut String,
    target: &str,
    line: &Line,
    cols: usize,
    stable: StableRowIndex,
) {
    let _ = write!(out, "<div class=\"row\" id=\"{target}-r-{stable}\">");
    let default = CellAttributes::default();
    let mut expected = 0usize;
    let mut pending_blanks = 0usize;
    // Some(close_tag) while a wrapper is open; run identity is attrs + href.
    let mut open: Option<&'static str> = None;
    let mut run: Option<(CellAttributes, Option<String>)> = None;
    let close = |out: &mut String, open: &mut Option<&'static str>,
                     run: &mut Option<(CellAttributes, Option<String>)>| {
        if let Some(tag) = open.take() {
            out.push_str(tag);
        }
        *run = None;
    };

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
        let href: Option<String> = attrs
            .hyperlink()
            .and_then(|h| safe_href(h.uri()))
            .map(str::to_string);

        let plain = href.is_none() && attrs_equiv(attrs, &default);
        if plain && width == 1 && (s == " " || s.is_empty()) {
            pending_blanks += 1;
            continue;
        }
        if pending_blanks > 0 {
            close(out, &mut open, &mut run);
            out.extend(std::iter::repeat(' ').take(pending_blanks));
            pending_blanks = 0;
        }
        if plain {
            close(out, &mut open, &mut run);
        } else {
            let same = run
                .as_ref()
                .is_some_and(|(ra, rh)| attrs_equiv(ra, attrs) && *rh == href);
            if !same {
                close(out, &mut open, &mut run);
                let (classes, style) = cell_class_and_style(attrs);
                if href.is_none() && classes.is_empty() && style.is_empty() {
                    // visually default (e.g. only non-rendered bits set)
                } else {
                    if let Some(h) = &href {
                        out.push_str("<a href=\"");
                        html_escape(h, out);
                        out.push_str("\" target=\"_blank\" rel=\"noopener\"");
                        open = Some("</a>");
                    } else {
                        out.push_str("<span");
                        open = Some("</span>");
                    }
                    if !classes.is_empty() {
                        let _ = write!(out, " class=\"c{classes}\"");
                    }
                    if !style.is_empty() {
                        let _ = write!(out, " style=\"{style}\"");
                    }
                    out.push('>');
                    run = Some((attrs.clone(), href.clone()));
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
    close(out, &mut open, &mut run);
    out.push_str("</div>");
}
