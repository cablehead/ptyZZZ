//! ptyZZZ: own one pty, speak JSONL on stdio. See PROTOCOL.md.
//!
//! This build emulates the terminal with libghostty-vt (Ghostty's terminal
//! core) instead of wezterm-term. The frame protocol is unchanged: each emit
//! is either a keyframe (`screen`: the whole grid as one div, the join point
//! for new subscribers) or a diff (`diff`: changed rows, appended rows,
//! trimmed row ids, cursor).
//!
//! Ghostty's render-state API is viewport-only, so stable row identity is
//! maintained here rather than borrowed from the emulator: a monotonic row id
//! (gid) is assigned to every row as it enters the viewport, and a tracked
//! grid ref pinned at the active top-left acts as a scroll odometer between
//! ticks. Rows that scroll into history keep their gid and their cached HTML;
//! rows that fly through the viewport inside one coalesce window are read
//! back out of scrollback via grid refs. Keyframes are the ordered
//! concatenation of the cache, so scrollback survives without re-reading it.
//! A resize reflows the primary screen and invalidates the cache, so the
//! whole scrollback is re-read via grid refs (slow path, rare).

use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::Parser;
use libghostty_vt::{
    render::{CellIterator, Dirty, RenderState, RowIterator},
    screen::{CellContentTag, CellWide, Screen, TrackedGridRef},
    style::{PaletteIndex, StyleColor, Underline},
    terminal::{Point, PointCoordinate, PointSpace},
    Terminal, TerminalOptions,
};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Deserialize;
use std::fmt::Write as _;

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

/// Events folded into the terminal by the emitter thread. The ghostty
/// Terminal is not Send, so it lives on the emitter thread and the pty
/// reader/stdin threads feed it through this channel.
enum Ev {
    Bytes(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Eof,
}

/// Fold one event into the terminal. Returns true when the pty is done.
fn apply(term: &mut Terminal<'_, '_>, ev: Ev) -> bool {
    match ev {
        Ev::Bytes(b) => term.vt_write(&b),
        Ev::Resize { cols, rows } => {
            let _ = term.resize(cols, rows, 0, 0);
        }
        Ev::Eof => return true,
    }
    false
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
    let master = Arc::new(Mutex::new(pair.master));

    let (tx, rx) = mpsc::channel::<Ev>();

    // reader: drain pty -> event channel.
    {
        let tx = tx.clone();
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
                        if tx.send(Ev::Bytes(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            let _ = tx.send(Ev::Eof);
        });
    }

    // stdin: JSONL commands -> pty (input) / event channel (resize).
    {
        let writer = writer.clone();
        let master = master.clone();
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
                        let _ = tx.send(Ev::Resize { cols, rows });
                    }
                    Err(_) => eprintln!("ptyZZZ: bad command: {line}"),
                }
            }
        });
    }

    // emitter (this thread): own the terminal, fold events, emit frames.
    // ghostty's max_scrollback is a byte budget consumed in page-sized
    // chunks, not a line count. Approximate the --scrollback lines contract
    // with ~cols*10 bytes per retained row (measured ~810 B/row at 80 cols);
    // pruning stays page-granular, so retention is approximate either way.
    let mut term = Terminal::new(TerminalOptions {
        cols,
        rows,
        max_scrollback: scrollback * cols as usize * 10,
    })
    .expect("terminal");
    {
        let writer = writer.clone();
        term.on_pty_write(move |_t, data| {
            let mut w = writer.lock().unwrap();
            let _ = w.write_all(data);
            let _ = w.flush();
        })
        .expect("on_pty_write");
    }

    let out = std::io::stdout();
    let mut state = EmitState::new(target);
    let mut done = false;
    // Cost of building+writing the previous frame; the coalesce sleep is
    // shortened by this much so continuous output emits on a steady
    // ~coalesce cadence instead of coalesce + frame cost.
    let mut frame_cost = Duration::ZERO;
    loop {
        // Wait for pty output, a resize, or an owed keyframe.
        let mut got = false;
        while !done {
            let due_in = if state.dirty_since_keyframe {
                keyframe_interval.saturating_sub(state.last_keyframe.elapsed())
            } else {
                Duration::from_secs(3600)
            };
            if due_in.is_zero() {
                break;
            }
            match rx.recv_timeout(due_in.min(Duration::from_secs(1))) {
                Ok(ev) => {
                    done = apply(&mut term, ev);
                    got = true;
                    break;
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }
        // Coalesce: keep folding events until the window closes. Bytes are
        // fed to the terminal on arrival, so query responses (DSR/DA) go
        // back to the pty immediately; only frame emission is delayed.
        if got && !done {
            let deadline = Instant::now() + coalesce.saturating_sub(frame_cost);
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    while let Ok(ev) = rx.try_recv() {
                        if apply(&mut term, ev) {
                            done = true;
                            break;
                        }
                    }
                    break;
                }
                match rx.recv_timeout(left) {
                    Ok(ev) => {
                        if apply(&mut term, ev) {
                            done = true;
                            break;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => {
                        done = true;
                        break;
                    }
                }
            }
        }
        if done {
            while let Ok(ev) = rx.try_recv() {
                apply(&mut term, ev);
            }
        }

        let keyframe_due =
            state.dirty_since_keyframe && state.last_keyframe.elapsed() >= keyframe_interval;
        let started = Instant::now();
        let frame = state.produce(&mut term, keyframe_due);
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

        if done {
            break;
        }
    }

    let code = child.wait().map(|s| s.exit_code() as i64).unwrap_or(-1);
    let mut w = out.lock();
    let _ = writeln!(w, "{}", serde_json::json!({"t":"exit","code":code}));
    let _ = w.flush();
}

// --- frame production --------------------------------------------------------

/// How this tick maps rows to ids.
enum Mode {
    /// Steady state on the primary screen: pin math gives the scroll delta.
    Normal { forced: bool },
    /// Full re-derivation on the primary screen: fresh id epoch, scrollback
    /// re-read via grid refs. Used for startup, resize, and pin loss.
    Rebuild,
    /// Fresh id epoch covering only the viewport (alt screen has no
    /// scrollback). Used on alt entry and alt resize.
    AltEpoch,
}

struct EmitState {
    target: String,
    /// rendered row html by monotonic row id; the keyframe is the ordered
    /// concatenation of this map
    cache: BTreeMap<u64, String>,
    render_state: RenderState<'static>,
    rows_iter: RowIterator<'static>,
    cells_iter: CellIterator<'static>,
    /// scroll odometer: a tracked grid ref pinned at the active top-left
    /// after every tick, plus the row id it was pinned on
    pin: Option<TrackedGridRef>,
    pin_gid: u64,
    /// next unused row id; ids never repeat across epochs
    next_gid: u64,
    vp_base: u64,
    last_max: u64,
    last_cols: usize,
    last_rows: usize,
    last_alt: bool,
    last_cursor: (usize, usize),
    sent_initial: bool,
    last_keyframe: Instant,
    dirty_since_keyframe: bool,
    /// primary-screen cache parked while the alt screen is active, with the
    /// (cols, rows, vp_base) it was rendered at; restored on alt exit if the
    /// size still matches, otherwise the scrollback is re-read
    saved_primary: Option<(BTreeMap<u64, String>, usize, usize, u64)>,
    seqno: u64,
}

impl EmitState {
    fn new(target: String) -> Self {
        Self {
            target,
            cache: BTreeMap::new(),
            render_state: RenderState::new().expect("render state"),
            rows_iter: RowIterator::new().expect("row iterator"),
            cells_iter: CellIterator::new().expect("cell iterator"),
            pin: None,
            pin_gid: 0,
            next_gid: 0,
            vp_base: 0,
            last_max: 0,
            last_cols: 0,
            last_rows: 0,
            last_alt: false,
            last_cursor: (usize::MAX, usize::MAX),
            sent_initial: false,
            last_keyframe: Instant::now(),
            dirty_since_keyframe: false,
            saved_primary: None,
            seqno: 0,
        }
    }

    /// Inspect the terminal, update the row cache, and produce the next frame:
    /// a keyframe, a diff, or None when nothing visible changed.
    fn produce(
        &mut self,
        term: &mut Terminal<'static, 'static>,
        keyframe_due: bool,
    ) -> Option<serde_json::Value> {
        let alt = matches!(term.active_screen(), Ok(Screen::Alternate));
        let cols = term.cols().unwrap_or(0) as usize;
        let rows = term.rows().unwrap_or(0) as usize;
        if cols == 0 || rows == 0 {
            return None;
        }
        let scrollback_now = if alt {
            0
        } else {
            term.scrollback_rows().unwrap_or(0) as u64
        };
        let (cx, cy) = (
            term.cursor_x().unwrap_or(0) as usize,
            term.cursor_y().unwrap_or(0) as usize,
        );

        let size_changed = cols != self.last_cols || rows != self.last_rows;
        let flip = alt != self.last_alt;

        // Decide how this tick maps rows to ids. A resize reflows the primary
        // screen and an alt flip swaps screens; both invalidate the cache and
        // the pin-derived id basis.
        let mode = if !self.sent_initial {
            if alt {
                Mode::AltEpoch
            } else {
                Mode::Rebuild
            }
        } else if flip && alt {
            // Entering the alt screen: park the primary cache. The primary
            // screen and its scrollback are frozen while alt is active, and
            // the pin (owned by the primary screen) survives the excursion.
            self.saved_primary = Some((
                std::mem::take(&mut self.cache),
                self.last_cols,
                self.last_rows,
                self.vp_base,
            ));
            Mode::AltEpoch
        } else if flip && !alt {
            match self.saved_primary.take() {
                Some((saved, scols, srows, svp_base))
                    if scols == cols && srows == rows && self.pin.is_some() =>
                {
                    self.cache = saved;
                    self.vp_base = svp_base;
                    Mode::Normal { forced: true }
                }
                _ => Mode::Rebuild,
            }
        } else if size_changed {
            if alt {
                // The alt resize also reflows the parked primary; drop it so
                // alt exit takes the rebuild path.
                self.saved_primary = None;
                Mode::AltEpoch
            } else {
                Mode::Rebuild
            }
        } else {
            Mode::Normal { forced: false }
        };

        // Resolve the viewport base id and, for primary steady state, the
        // scroll delta since the last tick from the pin.
        let mut forced = false;
        let mut trimmed: Vec<u64> = Vec::new();
        let mut changed: Vec<u64> = Vec::new();
        let mut appended: Vec<u64> = Vec::new();

        let mode = match mode {
            Mode::Normal { forced: f } if !alt => {
                let pinned = self
                    .pin
                    .as_ref()
                    .and_then(|p| p.point(PointSpace::Screen).ok().flatten());
                match pinned {
                    Some(p) => {
                        // Rows above the pin are exactly the history rows we
                        // assigned contiguous ids to, so the pin's screen-space
                        // y converts directly into the id of screen row 0.
                        let row0_gid = self.pin_gid.saturating_sub(p.y as u64);
                        let new_base = (row0_gid + scrollback_now).max(self.vp_base);
                        self.vp_base = new_base;
                        forced = f;
                        Mode::Normal { forced: f }
                    }
                    // The pinned row was pruned or lost: the odometer broke,
                    // re-derive everything under a fresh id epoch.
                    None => Mode::Rebuild,
                }
            }
            m => m,
        };

        match mode {
            Mode::Normal { .. } => {}
            Mode::Rebuild => {
                forced = true;
                self.cache.clear();
                let base = self.next_gid;
                self.vp_base = base + scrollback_now;
                // Re-read the whole scrollback through grid refs. This is the
                // slow path (per-cell FFI), but it only runs on startup,
                // resize, and pin loss.
                for h in 0..scrollback_now {
                    let mut html = String::with_capacity(64);
                    render_history_row_into(&mut html, &self.target, term, base + h, h, cols);
                    self.cache.insert(base + h, html);
                }
                self.last_max = self.vp_base; // viewport rows re-render as new
            }
            Mode::AltEpoch => {
                forced = true;
                self.cache.clear();
                self.vp_base = self.next_gid;
                self.last_max = self.vp_base;
            }
        }

        let row0_gid = self.vp_base.saturating_sub(scrollback_now);

        if !alt && !forced {
            // Rows that left the viewport since the last tick: rows the last
            // snapshot rendered may have been touched again before scrolling
            // off, and rows that flew through the viewport inside one
            // coalesce window were never snapshot at all. Re-read both out
            // of scrollback via grid refs and byte-compare against the cache.
            let old_vp_start = self.last_max.saturating_sub(self.last_rows as u64);
            let start = old_vp_start.max(row0_gid);
            for gid in start..self.vp_base {
                let mut html = String::with_capacity(64);
                render_history_row_into(&mut html, &self.target, term, gid, gid - row0_gid, cols);
                if self.cache.get(&gid) != Some(&html) {
                    self.cache.insert(gid, html);
                    if gid < self.last_max {
                        changed.push(gid);
                    } else {
                        appended.push(gid);
                    }
                } else if gid >= self.last_max {
                    appended.push(gid);
                }
            }
            // Rows ghostty pruned off the scrollback fall off the client too.
            let cache_first = self.cache.keys().next().copied().unwrap_or(row0_gid);
            for gid in cache_first..row0_gid.min(self.vp_base) {
                if self.cache.remove(&gid).is_some() {
                    trimmed.push(gid);
                }
            }
        }

        // Render viewport rows from the render-state snapshot. Dirty flags
        // are consumed here; a re-rendered row whose html matches the cache
        // byte-for-byte was touched but not visibly changed, so it drops out
        // of the diff.
        {
            let snapshot = self
                .render_state
                .update(&*term)
                .expect("render state update");
            let full = matches!(snapshot.dirty(), Ok(Dirty::Full));
            let mut row_iter = self.rows_iter.update(&snapshot).expect("row iteration");
            let mut y: u64 = 0;
            let mut text = String::with_capacity(16);
            while let Some(row) = row_iter.next() {
                let gid = self.vp_base + y;
                let is_new = gid >= self.last_max;
                let dirty = row.dirty().unwrap_or(true);
                if forced || full || is_new || dirty {
                    let mut html = String::with_capacity(64);
                    {
                        let mut rb = RowBuilder::begin(&mut html, &self.target, gid);
                        let mut cell_iter =
                            self.cells_iter.update(row).expect("cell iteration");
                        let mut x = 0usize;
                        while let Some(cell) = cell_iter.next() {
                            if x >= cols {
                                break;
                            }
                            x += 1;
                            let raw = match cell.raw_cell() {
                                Ok(c) => c,
                                Err(_) => continue,
                            };
                            match raw.wide() {
                                Ok(CellWide::SpacerTail) | Ok(CellWide::SpacerHead) => continue,
                                Ok(CellWide::Wide) => {
                                    text.clear();
                                    let _ = cell.graphemes_utf8(&mut text);
                                    let attrs = attrs_for(&cell.style().ok(), &raw);
                                    rb.cell(if text.is_empty() { " " } else { &text }, 2, &attrs);
                                    continue;
                                }
                                _ => {}
                            }
                            let styled = cell.has_styling().unwrap_or(false);
                            let attrs = if styled {
                                attrs_for(&cell.style().ok(), &raw)
                            } else {
                                attrs_for(&None, &raw)
                            };
                            text.clear();
                            let _ = cell.graphemes_utf8(&mut text);
                            rb.cell(if text.is_empty() { " " } else { &text }, 1, &attrs);
                        }
                        rb.finish();
                    }
                    let fresh = self.cache.get(&gid) != Some(&html);
                    if fresh {
                        self.cache.insert(gid, html);
                        if is_new {
                            appended.push(gid);
                        } else {
                            changed.push(gid);
                        }
                    } else if is_new {
                        appended.push(gid);
                    }
                }
                let _ = row.set_dirty(false);
                y += 1;
            }
            let _ = snapshot.set_dirty(Dirty::Clean);
        }

        // Re-pin the odometer at the current active top-left. The pin lives
        // on the primary screen; while alt is active it must stay where it
        // is, or it would re-resolve against the alt screen.
        if !alt {
            let p = Point::Active(PointCoordinate { x: 0, y: 0 });
            match self.pin.as_mut() {
                Some(pin) => {
                    let _ = pin.set(term, p);
                }
                None => self.pin = term.track_grid_ref(p).ok(),
            }
            self.pin_gid = self.vp_base;
        }

        let max = self.vp_base + rows as u64;
        let total = self.cache.len();
        let cursor_now = (total.saturating_sub(rows) + cy, cx);
        let cursor_moved = cursor_now != self.last_cursor;

        if !forced
            && !keyframe_due
            && changed.is_empty()
            && appended.is_empty()
            && trimmed.is_empty()
            && !cursor_moved
        {
            self.last_max = max;
            self.next_gid = self.next_gid.max(max);
            return None;
        }

        self.seqno += 1;
        let seqno = self.seqno;

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
            for gid in &changed {
                patch.push_str(&self.cache[gid]);
            }
            if cursor_moved {
                render_cursor_into(&mut patch, &self.target, cursor_now.0, cursor_now.1);
            }
            let mut append = String::new();
            for gid in &appended {
                append.push_str(&self.cache[gid]);
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

        self.last_max = max;
        self.next_gid = self.next_gid.max(max);
        self.last_cols = cols;
        self.last_rows = rows;
        self.last_alt = alt;
        self.last_cursor = cursor_now;
        self.sent_initial = true;
        Some(frame)
    }
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

/// Exactly the attributes the renderer maps to output, so runs don't split
/// on invisible differences (blink, hyperlinks, wrap state).
#[derive(Clone, PartialEq)]
struct Attrs {
    bold: bool,
    faint: bool,
    italic: bool,
    underline: bool,
    invisible: bool,
    strikethrough: bool,
    inverse: bool,
    fg: StyleColor,
    bg: StyleColor,
}

impl Default for Attrs {
    fn default() -> Self {
        Self {
            bold: false,
            faint: false,
            italic: false,
            underline: false,
            invisible: false,
            strikethrough: false,
            inverse: false,
            fg: StyleColor::None,
            bg: StyleColor::None,
        }
    }
}

/// Fold a ghostty style plus the cell's content-tag background (an erased
/// cell carries its bg color in the cell, not the style) into render attrs.
fn attrs_for(
    style: &Option<libghostty_vt::style::Style>,
    raw: &libghostty_vt::screen::Cell,
) -> Attrs {
    let mut a = match style {
        Some(s) => Attrs {
            bold: s.bold,
            faint: s.faint,
            italic: s.italic,
            underline: !matches!(s.underline, Underline::None),
            invisible: s.invisible,
            strikethrough: s.strikethrough,
            inverse: s.inverse,
            fg: s.fg_color,
            bg: s.bg_color,
        },
        None => Attrs::default(),
    };
    match raw.content_tag() {
        Ok(CellContentTag::BgColorPalette) => {
            if let Ok(i) = raw.bg_color_palette() {
                a.bg = StyleColor::Palette(i);
            }
        }
        Ok(CellContentTag::BgColorRgb) => {
            if let Ok(c) = raw.bg_color_rgb() {
                a.bg = StyleColor::Rgb(c);
            }
        }
        _ => {}
    }
    a
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

fn append_color_inline(out: &mut String, prop: &str, c: StyleColor, default_var: &str) {
    match c {
        StyleColor::None => {
            if !default_var.is_empty() {
                let _ = write!(out, "{prop}:var({default_var});");
            }
        }
        StyleColor::Palette(PaletteIndex(i)) if i < 16 => {
            let _ = write!(out, "{prop}:var(--c{i});");
        }
        StyleColor::Palette(PaletteIndex(i)) => {
            let (r, g, b) = palette_to_rgb(i);
            let _ = write!(out, "{prop}:#{r:02x}{g:02x}{b:02x};");
        }
        StyleColor::Rgb(c) => {
            let _ = write!(out, "{prop}:#{:02x}{:02x}{:02x};", c.r, c.g, c.b);
        }
    }
}

fn cell_class_and_style(attrs: &Attrs) -> (String, String) {
    let mut classes = String::new();
    let mut style = String::new();
    if attrs.bold {
        classes.push_str(" sb");
    }
    if attrs.faint {
        classes.push_str(" sd");
    }
    if attrs.italic {
        classes.push_str(" si");
    }
    if attrs.underline {
        classes.push_str(" su");
    }
    if attrs.invisible {
        classes.push_str(" sx");
    }
    if attrs.strikethrough {
        classes.push_str(" ss");
    }
    if attrs.inverse {
        append_color_inline(&mut style, "color", attrs.bg, "--term-bg");
        append_color_inline(&mut style, "background", attrs.fg, "--term-fg");
    } else {
        match attrs.fg {
            StyleColor::None => {}
            StyleColor::Palette(PaletteIndex(i)) if i < 16 => {
                let _ = write!(classes, " f{i}");
            }
            other => append_color_inline(&mut style, "color", other, ""),
        }
        match attrs.bg {
            StyleColor::None => {}
            StyleColor::Palette(PaletteIndex(i)) if i < 16 => {
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

/// One streaming pass over a row's cells: runs of equal attrs share one span
/// and default-attr blanks are buffered so trailing ones are dropped --
/// `white-space:pre` plus `.row{min-height}` keep the geometry without them.
struct RowBuilder<'a> {
    out: &'a mut String,
    pending_blanks: usize,
    span_open: bool,
    run: Option<Attrs>,
}

impl<'a> RowBuilder<'a> {
    fn begin(out: &'a mut String, target: &str, gid: u64) -> Self {
        let _ = write!(out, "<div class=\"row\" id=\"{target}-r-{gid}\">");
        Self {
            out,
            pending_blanks: 0,
            span_open: false,
            run: None,
        }
    }

    fn close_span(&mut self) {
        if self.span_open {
            self.out.push_str("</span>");
            self.span_open = false;
            self.run = None;
        }
    }

    fn cell(&mut self, glyph: &str, width: usize, attrs: &Attrs) {
        let plain = *attrs == Attrs::default();
        if plain && width == 1 && (glyph == " " || glyph.is_empty()) {
            self.pending_blanks += 1;
            return;
        }
        if self.pending_blanks > 0 {
            self.close_span();
            let n = self.pending_blanks;
            self.out.extend(std::iter::repeat(' ').take(n));
            self.pending_blanks = 0;
        }
        if plain {
            self.close_span();
        } else {
            let same = self.run.as_ref().is_some_and(|r| r == attrs);
            if !same {
                if self.span_open {
                    self.out.push_str("</span>");
                }
                let (classes, style) = cell_class_and_style(attrs);
                if classes.is_empty() && style.is_empty() {
                    self.span_open = false;
                    self.run = None;
                } else {
                    self.out.push_str("<span class=\"c");
                    self.out.push_str(&classes);
                    self.out.push('"');
                    if !style.is_empty() {
                        let _ = write!(self.out, " style=\"{style}\"");
                    }
                    self.out.push('>');
                    self.span_open = true;
                    self.run = Some(attrs.clone());
                }
            }
        }
        let glyph = if glyph.is_empty() { " " } else { glyph };
        if cell_needs_box(glyph, width) {
            let _ = write!(self.out, "<span class=\"wc\" style=\"--w:{width}\">");
            html_escape(glyph, self.out);
            self.out.push_str("</span>");
        } else {
            html_escape(glyph, self.out);
        }
    }

    fn finish(mut self) {
        self.close_span();
        self.out.push_str("</div>");
    }
}

/// Render one scrollback row by walking its cells through grid refs. This is
/// per-cell FFI, so it is reserved for rows the viewport snapshot can't see:
/// rows that already scrolled off, and full-scrollback rebuilds.
fn render_history_row_into(
    out: &mut String,
    target: &str,
    term: &Terminal,
    gid: u64,
    screen_y: u64,
    cols: usize,
) {
    let mut rb = RowBuilder::begin(out, target, gid);
    let mut charbuf = [char::MAX; 32];
    let mut text = String::with_capacity(8);
    let mut x = 0usize;
    while x < cols {
        let gr = match term.grid_ref(Point::Screen(PointCoordinate {
            x: x as u16,
            y: screen_y as u32,
        })) {
            Ok(g) => g,
            Err(_) => break,
        };
        let raw = match gr.cell() {
            Ok(c) => c,
            Err(_) => break,
        };
        let wide = raw.wide().unwrap_or(CellWide::Narrow);
        if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
            x += 1;
            continue;
        }
        let width = if matches!(wide, CellWide::Wide) { 2 } else { 1 };
        text.clear();
        match raw.content_tag() {
            Ok(CellContentTag::CodepointGrapheme) => {
                if let Ok(n) = gr.graphemes(&mut charbuf) {
                    for c in &charbuf[..n] {
                        text.push(*c);
                    }
                }
            }
            _ => {
                if let Ok(cp) = raw.codepoint() {
                    if cp != 0 {
                        if let Some(c) = char::from_u32(cp) {
                            text.push(c);
                        }
                    }
                }
            }
        }
        let style = if raw.has_styling().unwrap_or(false) {
            gr.style().ok()
        } else {
            None
        };
        let attrs = attrs_for(&style, &raw);
        rb.cell(if text.is_empty() { " " } else { &text }, width, &attrs);
        x += 1;
    }
    rb.finish();
}
