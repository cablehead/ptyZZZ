//! ptyZZZ: own one pty, speak JSONL on stdio. See PROTOCOL.md.
//!
//! v1 renders the full scrollback (not just the visible grid) and tracks
//! damage per row. Each emit is either a keyframe (`screen`: the whole grid as
//! one div, the join point for new subscribers) or a diff (`diff`: changed
//! rows, appended rows, trimmed row ids, cursor). Rows are keyed by a stable
//! row id derived from rio's grid (lines evicted off the scrollback ring plus
//! physical index), rendered once into a cache, and re-rendered only when
//! rio's viewport damage covers the line; re-rendered rows are byte-compared
//! against the cache so output that changes nothing visible emits nothing.

use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use clap::Parser;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Deserialize;
use std::fmt::Write as _;

use rio_vt::ansi::CursorShape;
use rio_vt::config::colors::AnsiColor;
use rio_vt::crosswords::grid::row::Row;
use rio_vt::crosswords::grid::ExtrasTable;
use rio_vt::crosswords::pos::Line;
use rio_vt::crosswords::square::{ContentTag, Square, Wide};
use rio_vt::crosswords::style::{StyleFlags, StyleId, StyleSet, DEFAULT_STYLE_ID};
use rio_vt::crosswords::{Crosswords, CrosswordsSize, Mode, TermDamage};
use rio_vt::event::{EventListener, RioEvent, WindowId};
use rio_vt::performer::handler::Processor;

/// Stable row id: `lines_evicted() + physical index`. rio counts every line
/// ever evicted off the scrollback ring, so a row keeps its id while it
/// scrolls from the viewport into history and the id is never reused.
type StableRow = u64;
type Term = Crosswords<PtyProxy>;

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

/// rio's terminal takes no writer; it answers queries (DSR, DA, DECRPM,
/// XTGETTCAP, ...) by emitting `RioEvent::PtyWrite`. Route those bytes back
/// into the pty master so applications that probe the terminal keep working.
struct PtyProxy(Arc<Mutex<Box<dyn Write + Send>>>);
impl EventListener for PtyProxy {
    fn send_event(&self, event: RioEvent, _id: WindowId) {
        if let RioEvent::PtyWrite(_route, text) = event {
            let mut w = self.0.lock().unwrap();
            let _ = w.write_all(text.as_bytes());
            let _ = w.flush();
        }
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

    let term = Crosswords::new(
        CrosswordsSize::new(size.cols as usize, size.rows as usize),
        CursorShape::Block,
        PtyProxy(writer.clone()),
        WindowId::from(0),
        0,
        scrollback,
    );
    let term = Arc::new(Mutex::new(term));
    let dirty = Arc::new((Mutex::new(0u64), Condvar::new()));
    let done = Arc::new(AtomicBool::new(false));
    let master = Arc::new(Mutex::new(pair.master));

    // reader: drain pty -> feed rio's parser -> bump dirty.
    {
        let term = term.clone();
        let dirty = dirty.clone();
        let done = done.clone();
        let mut record = record.map(|p| std::fs::File::create(p).expect("record file"));
        std::thread::spawn(move || {
            let mut processor = Processor::default();
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Some(f) = record.as_mut() {
                            let _ = f.write_all(&buf[..n]);
                        }
                        processor.advance(&mut *term.lock().unwrap(), &buf[..n]);
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
                        term.lock()
                            .unwrap()
                            .resize(CrosswordsSize::new(cols as usize, rows as usize));
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
    /// rendered row html by stable row id; the keyframe is the ordered
    /// concatenation of this map
    cache: BTreeMap<StableRow, String>,
    /// monotonic frame counter; rio has no terminal seqno, so this stands in
    /// for the protocol's `seqno` field
    seq: u64,
    last_base: StableRow,
    last_max: StableRow,
    last_cols: usize,
    last_rows: usize,
    last_alt: bool,
    last_cursor: (usize, usize),
    sent_initial: bool,
    last_keyframe: Instant,
    dirty_since_keyframe: bool,
}

impl EmitState {
    fn new(target: String) -> Self {
        Self {
            target,
            cache: BTreeMap::new(),
            seq: 0,
            last_base: 0,
            last_max: 0,
            last_cols: 0,
            last_rows: 0,
            last_alt: false,
            last_cursor: (usize::MAX, usize::MAX),
            sent_initial: false,
            last_keyframe: Instant::now(),
            dirty_since_keyframe: false,
        }
    }

    /// Inspect the terminal, update the row cache, and produce the next frame:
    /// a keyframe, a diff, or None when nothing visible changed.
    fn produce(&mut self, term: &mut Term, keyframe_due: bool) -> Option<serde_json::Value> {
        self.seq += 1;
        let seqno = self.seq;
        let alt = term.mode().contains(Mode::ALT_SCREEN);
        let cols = term.columns();
        let rows = term.screen_lines();
        let history = term.history_size();
        let total = history + rows;
        let base: StableRow = term.lines_evicted();
        let max = base + total as StableRow;
        let cursor = term.cursor();
        let cursor_now = (
            history + cursor.pos.row.0.max(0) as usize,
            cursor.pos.col.0,
        );

        // Consume rio's damage (viewport-only, consume-and-reset).
        let mut damage_full = false;
        let mut damage_lines: Vec<usize> = Vec::new();
        match term.damage() {
            TermDamage::Full => damage_full = true,
            TermDamage::Partial(it) => damage_lines.extend(it.map(|l| l.line)),
        }
        term.reset_damage();

        // A resize reflows rows and an alt-screen flip swaps line storage;
        // both invalidate the diff basis, so re-render everything.
        let forced = !self.sent_initial
            || alt != self.last_alt
            || cols != self.last_cols
            || rows != self.last_rows;

        // Damage since the last emit, limited to rows the client still has.
        // New rows are the append path; trimmed rows fell off the scrollback.
        // Rows already in scrollback at the last scan are frozen -- apps
        // can't address them, and the reflow/alt-flip cases that could move
        // them force a full re-render above -- so only rows that were in
        // the viewport at the last emit can have changed. rio's damage is
        // positional over the current viewport: when the grid scrolled since
        // the last emit, line numbers no longer identify the rows that
        // changed (content keeps its stable id but moves lines), so fall
        // back to scanning the old-viewport overlap; the row cache
        // byte-compare drops the false positives. Either way this stays
        // O(rows), not O(scrollback); the old viewport top also covers rows
        // damaged and then scrolled off within one coalesce window.
        let overlap_end = max.min(self.last_max);
        let vp_start = self.last_max.saturating_sub(self.last_rows as StableRow).max(base);
        let scrolled = max != self.last_max || base != self.last_base;
        let damaged: Vec<StableRow> = if forced || overlap_end <= vp_start {
            Vec::new()
        } else if damage_full || scrolled {
            (vp_start..overlap_end).collect()
        } else {
            damage_lines
                .iter()
                .filter_map(|&n| {
                    let stable = base + (history + n) as StableRow;
                    (stable >= vp_start && stable < overlap_end).then_some(stable)
                })
                .collect()
        };
        let appended: Vec<StableRow> = if forced {
            Vec::new()
        } else {
            (self.last_max.max(base)..max).collect()
        };
        let trimmed: Vec<StableRow> = if forced {
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
            return None;
        }

        // Render damaged rows into the cache. A row whose new html matches the
        // cache byte-for-byte was touched but not visibly changed (a prompt
        // redraw writing identical cells); drop it from the diff.
        let styles = &term.grid.style_set;
        let extras = &term.grid.extras_table;
        let mut changed: Vec<StableRow> = Vec::new();
        if forced {
            self.cache.clear();
            for phys in 0..total {
                let stable = base + phys as StableRow;
                let line = &term.grid[Line(phys as i32 - history as i32)];
                let mut html = String::with_capacity(64);
                render_row_into(&mut html, &self.target, line, styles, extras, cols, stable);
                self.cache.insert(stable, html);
            }
        } else {
            self.cache = self.cache.split_off(&base);
            for &stable in damaged.iter().chain(appended.iter()) {
                let phys = (stable - base) as usize;
                let line = &term.grid[Line(phys as i32 - history as i32)];
                let mut html = String::with_capacity(64);
                render_row_into(&mut html, &self.target, line, styles, extras, cols, stable);
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
// and know nothing about the emulator.

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

// --- render ------------------------------------------------------------------

/// The cursor is its own self-identified overlay, positioned purely by CSS
/// vars, so a cursor move patches ~90 bytes instead of touching any row.
fn render_cursor_into(out: &mut String, target: &str, row: usize, col: usize) {
    let _ = write!(
        out,
        "<div class=\"cursor\" id=\"{target}-cursor\" style=\"--cursor-row:{row};--cursor-col:{col}\"></div>"
    );
}

/// What a cell contributes to paint, reduced to a copyable key: either an id
/// into the grid's interned style table, or an inline background for rio's
/// bg-only cells (which skip the style table entirely). Comparing keys is the
/// run-merge equivalence -- two cells with the same key share one span.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Paint {
    Style(StyleId),
    BgPalette(u8),
    BgRgb(u8, u8, u8),
}

fn paint_of(sq: Square) -> Paint {
    match sq.content_tag() {
        ContentTag::Codepoint => Paint::Style(sq.style_id()),
        ContentTag::BgPalette => Paint::BgPalette(sq.bg_palette_index()),
        ContentTag::BgRgb => {
            let (r, g, b) = sq.bg_rgb();
            Paint::BgRgb(r, g, b)
        }
    }
}

/// A color as the renderer maps it to output: default (inherit / CSS var),
/// one of the 16 palette slots (CSS var), or a concrete rgb.
#[derive(Clone, Copy)]
enum ColorClass {
    Default,
    Palette(u8),
    Rgb(u8, u8, u8),
}

fn classify(c: AnsiColor) -> ColorClass {
    match c {
        AnsiColor::Named(n) => {
            let v = n as u32;
            if v < 16 {
                ColorClass::Palette(v as u8)
            } else {
                // Foreground/Background and the dim/light derived names all
                // render as the default fg/bg here, matching the previous
                // renderer which had no such variants.
                ColorClass::Default
            }
        }
        AnsiColor::Indexed(i) if i < 16 => ColorClass::Palette(i),
        AnsiColor::Indexed(i) => {
            let (r, g, b) = palette_to_rgb(i);
            ColorClass::Rgb(r, g, b)
        }
        AnsiColor::Spec(rgb) => ColorClass::Rgb(rgb.r, rgb.g, rgb.b),
    }
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

fn append_color_inline(out: &mut String, prop: &str, c: ColorClass, default_var: &str) {
    match c {
        ColorClass::Default => {
            if !default_var.is_empty() {
                let _ = write!(out, "{prop}:var({default_var});");
            }
        }
        ColorClass::Palette(i) => {
            let _ = write!(out, "{prop}:var(--c{i});");
        }
        ColorClass::Rgb(r, g, b) => {
            let _ = write!(out, "{prop}:#{r:02x}{g:02x}{b:02x};");
        }
    }
}

fn cell_class_and_style(paint: Paint, styles: &StyleSet) -> (String, String) {
    let mut classes = String::new();
    let mut style = String::new();
    match paint {
        Paint::BgPalette(i) if i < 16 => {
            let _ = write!(classes, " b{i}");
        }
        Paint::BgPalette(i) => {
            let (r, g, b) = palette_to_rgb(i);
            append_color_inline(&mut style, "background", ColorClass::Rgb(r, g, b), "");
        }
        Paint::BgRgb(r, g, b) => {
            append_color_inline(&mut style, "background", ColorClass::Rgb(r, g, b), "");
        }
        Paint::Style(id) => {
            let s = styles.get(id);
            let f = s.flags;
            if f.contains(StyleFlags::BOLD) {
                classes.push_str(" sb");
            }
            if f.contains(StyleFlags::DIM) {
                classes.push_str(" sd");
            }
            if f.contains(StyleFlags::ITALIC) {
                classes.push_str(" si");
            }
            if f.intersects(StyleFlags::ALL_UNDERLINES) {
                classes.push_str(" su");
            }
            if f.contains(StyleFlags::HIDDEN) {
                classes.push_str(" sx");
            }
            if f.contains(StyleFlags::STRIKEOUT) {
                classes.push_str(" ss");
            }
            let fg = classify(s.fg);
            let bg = classify(s.bg);
            if f.contains(StyleFlags::INVERSE) {
                append_color_inline(&mut style, "color", bg, "--term-bg");
                append_color_inline(&mut style, "background", fg, "--term-fg");
            } else {
                match fg {
                    ColorClass::Default => {}
                    ColorClass::Palette(i) => {
                        let _ = write!(classes, " f{i}");
                    }
                    other => append_color_inline(&mut style, "color", other, ""),
                }
                match bg {
                    ColorClass::Default => {}
                    ColorClass::Palette(i) => {
                        let _ = write!(classes, " b{i}");
                    }
                    other => append_color_inline(&mut style, "background", other, ""),
                }
            }
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

/// One streaming pass over the row's cells: runs with the same paint key AND
/// the same link target share one wrapper (`<a>` when linked, `<span>` when
/// only styled), styles are looked up only at run boundaries, and no per-cell
/// allocation happens (grapheme clusters aside; they are rare). Default-paint
/// blanks are buffered so trailing ones are dropped -- `white-space:pre` plus
/// `.row{min-height}` keep the geometry without them.
fn render_row_into(
    out: &mut String,
    target: &str,
    row: &Row<Square>,
    styles: &StyleSet,
    extras: &ExtrasTable,
    cols: usize,
    stable: StableRow,
) {
    let _ = write!(out, "<div class=\"row\" id=\"{target}-r-{stable}\">");
    let ncols = cols.min(row.inner.len());
    let mut expected = 0usize;
    let mut pending_blanks = 0usize;
    let mut cluster = String::new();
    // Some(close_tag) while a wrapper is open; run identity is paint + href.
    let mut open: Option<&'static str> = None;
    let mut run: Option<(Paint, Option<&str>)> = None;

    for col in 0..ncols {
        if col < expected {
            continue; // spacer cell of a wide char
        }
        let sq = row.inner[col];
        let width = match sq.wide() {
            Wide::Wide => 2,
            _ => 1,
        };
        expected = col + width;
        let paint = paint_of(sq);
        let ch = sq.c();

        // Grapheme clusters and OSC 8 hyperlinks share the grid's extras
        // table; the cell itself only carries the base char and an id.
        // bg-only cells reuse the extras bits for color, so gate on tag.
        cluster.clear();
        let mut href: Option<&str> = None;
        if matches!(sq.content_tag(), ContentTag::Codepoint) && sq.has_extras() {
            if let Some(ex) = sq.extras_id().and_then(|id| extras.get(id)) {
                if sq.has_grapheme() {
                    cluster.push(if ch == '\0' { ' ' } else { ch });
                    cluster.extend(ex.zerowidth.iter());
                }
                if sq.has_hyperlink() {
                    href = ex.hyperlink.as_ref().and_then(|h| safe_href(h.uri()));
                }
            }
        }

        let plain = href.is_none() && paint == Paint::Style(DEFAULT_STYLE_ID);
        if plain && width == 1 && cluster.is_empty() && (ch == ' ' || ch == '\0') {
            pending_blanks += 1;
            continue;
        }
        if pending_blanks > 0 {
            if let Some(tag) = open.take() {
                out.push_str(tag);
            }
            run = None;
            out.extend(std::iter::repeat(' ').take(pending_blanks));
            pending_blanks = 0;
        }
        if plain {
            if let Some(tag) = open.take() {
                out.push_str(tag);
            }
            run = None;
        } else if run != Some((paint, href)) {
            if let Some(tag) = open.take() {
                out.push_str(tag);
            }
            run = None;
            let (classes, style) = cell_class_and_style(paint, styles);
            if href.is_none() && classes.is_empty() && style.is_empty() {
                // visually default (e.g. only non-rendered bits set)
            } else {
                if let Some(h) = href {
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
                run = Some((paint, href));
            }
        }
        let mut chbuf = [0u8; 4];
        let glyph: &str = if !cluster.is_empty() {
            &cluster
        } else if ch == '\0' {
            " "
        } else {
            ch.encode_utf8(&mut chbuf)
        };
        if cell_needs_box(glyph, width) {
            let _ = write!(out, "<span class=\"wc\" style=\"--w:{width}\">");
            html_escape(glyph, out);
            out.push_str("</span>");
        } else {
            html_escape(glyph, out);
        }
    }
    if let Some(tag) = open.take() {
        out.push_str(tag);
    }
    out.push_str("</div>");
}
