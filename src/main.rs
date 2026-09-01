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
    CellAttributes, Intensity, KeyCode, KeyModifiers, Line, StableRowIndex, Terminal,
    TerminalConfiguration, TerminalSize, Underline,
};

/// Which emulation engine this build carries; exported to the child as
/// PTYZZZ_ENGINE so a session can identify its backend (the rio-vt branch
/// sets this to "rio-vt").
const ENGINE: &str = "wezterm-term";

#[derive(Parser)]
#[command(version, about = "own one pty, speak JSONL on stdio")]
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
        /// min gap in ms between frame production starts; caps frame rate
        /// (~1000/ms). Leading edge: after idle the first change emits
        /// immediately. Higher = fewer, coarser frames -- lighter on the
        /// client and the fan-out.
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
        /// what stdin EOF means: the supervisor feeding us commands is gone.
        /// "exit" kills the pty child and shuts down (a duplex service's
        /// input never ends while the service lives, so EOF is a reliable
        /// orphan signal, even after the supervisor is SIGKILLed); "ignore"
        /// is for harnesses that pipe a finite script and read on
        #[arg(long, default_value = "exit", value_parser = ["exit", "ignore"])]
        on_stdin_eof: String,
        /// die when the parent process dies (Linux PR_SET_PDEATHSIG):
        /// exec-like lifetime coupling to the supervisor that no pipe or
        /// event protocol can miss
        #[arg(long)]
        die_with_parent: bool,
        /// command to run (default: $SHELL or nu). Everything after `--`.
        #[arg(trailing_var_arg = true)]
        cmd: Vec<String>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum Cmd {
    /// raw bytes to the pty (composed IME text, tests, escape hatches)
    Input { b: String },
    /// a semantic key event; the emulator encodes it per its current modes
    /// (application cursor keys, modifyOtherKeys, kitty protocol, ...)
    Key {
        key: String,
        #[serde(default)]
        mods: u8,
    },
    /// pasted text; wrapped in bracketed-paste markers when the app enabled them
    Paste { s: String },
    Resize { cols: u16, rows: u16 },
}

/// Browser `KeyboardEvent.key` name -> wezterm KeyCode. Single chars pass
/// through; named keys cover the editing/navigation cluster and F1-F24.
fn parse_key(k: &str) -> Option<KeyCode> {
    let mut chars = k.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(KeyCode::Char(c));
    }
    Some(match k {
        "ArrowUp" => KeyCode::UpArrow,
        "ArrowDown" => KeyCode::DownArrow,
        "ArrowLeft" => KeyCode::LeftArrow,
        "ArrowRight" => KeyCode::RightArrow,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Insert" => KeyCode::Insert,
        "Delete" => KeyCode::Delete,
        "Enter" => KeyCode::Enter,
        "Tab" => KeyCode::Tab,
        "Backspace" => KeyCode::Backspace,
        "Escape" => KeyCode::Escape,
        _ => {
            let n = k.strip_prefix('F')?.parse::<u8>().ok()?;
            if n == 0 || n > 24 {
                return None;
            }
            KeyCode::Function(n)
        }
    })
}

/// Encode a SUPER-modified named key as its xterm CSI form: param is
/// 1 + shift|alt|ctrl|meta bits, e.g. Cmd+Left = CSI 1;9D. wezterm's
/// encoder deliberately drops SUPER (plain xterm never sends it), but
/// TUIs accept these forms and stacks2099 shipped them. Modified cursor
/// keys use CSI even in application cursor mode, so no terminal state is
/// needed. None = no CSI form (meta+char etc); the caller falls back.
fn encode_super_key(kc: &KeyCode, mods_bits: u8) -> Option<String> {
    let p = 1 + mods_bits;
    Some(match kc {
        KeyCode::UpArrow => format!("\x1b[1;{p}A"),
        KeyCode::DownArrow => format!("\x1b[1;{p}B"),
        KeyCode::RightArrow => format!("\x1b[1;{p}C"),
        KeyCode::LeftArrow => format!("\x1b[1;{p}D"),
        KeyCode::Home => format!("\x1b[1;{p}H"),
        KeyCode::End => format!("\x1b[1;{p}F"),
        KeyCode::Insert => format!("\x1b[2;{p}~"),
        KeyCode::Delete => format!("\x1b[3;{p}~"),
        KeyCode::PageUp => format!("\x1b[5;{p}~"),
        KeyCode::PageDown => format!("\x1b[6;{p}~"),
        KeyCode::Function(n @ 1..=4) => format!("\x1b[1;{p}{}", (b'P' + n - 1) as char),
        KeyCode::Function(n @ 5..=12) => {
            let intro = [15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize];
            format!("\x1b[{intro};{p}~")
        }
        _ => return None,
    })
}

/// Client modifier bits (1 shift, 2 alt, 4 ctrl, 8 meta) -> wezterm modifiers.
fn parse_mods(bits: u8) -> KeyModifiers {
    let mut m = KeyModifiers::NONE;
    if bits & 1 != 0 {
        m |= KeyModifiers::SHIFT;
    }
    if bits & 2 != 0 {
        m |= KeyModifiers::ALT;
    }
    if bits & 4 != 0 {
        m |= KeyModifiers::CTRL;
    }
    if bits & 8 != 0 {
        m |= KeyModifiers::SUPER;
    }
    m
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

/// PR_SET_PDEATHSIG via a direct prctl declaration (no libc crate for one
/// call). Fires when the spawning thread dies, which for our supervisors
/// (a service's run thread) is exactly when we should go too. The getppid
/// check closes the race where the parent died before we armed.
#[cfg(target_os = "linux")]
fn arm_pdeathsig() {
    unsafe extern "C" {
        fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
        fn getppid() -> i32;
    }
    const PR_SET_PDEATHSIG: i32 = 1;
    const SIGTERM: u64 = 15;
    unsafe {
        prctl(PR_SET_PDEATHSIG, SIGTERM, 0, 0, 0);
        if getppid() == 1 {
            std::process::exit(0);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn arm_pdeathsig() {}

fn main() {
    let Sub::Run {
        cols,
        rows,
        target,
        coalesce,
        keyframe_interval,
        scrollback,
        record,
        on_stdin_eof,
        die_with_parent,
        cmd,
    } = Args::parse().sub;
    if die_with_parent {
        arm_pdeathsig();
    }
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
    builder.env("PTYZZZ_ENGINE", ENGINE);
    if let Ok(cwd) = std::env::current_dir() {
        builder.cwd(cwd);
    }
    let child = Arc::new(Mutex::new(pair.slave.spawn_command(builder).expect("spawn")));
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

    // stdin: JSONL commands -> pty. EOF means the supervisor feeding us is
    // gone (a duplex service input never ends while the service lives):
    // kill the pty child so the normal teardown runs, unless the caller
    // opted out for finite piped scripts.
    {
        let writer = writer.clone();
        let master = master.clone();
        let child = child.clone();
        let exit_on_eof = on_stdin_eof == "exit";
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
                    // key_down / send_paste encode through the terminal's own
                    // writer (the pty), honoring its current input modes.
                    // SUPER-modified named keys are encoded here instead,
                    // since wezterm's encoder drops that modifier.
                    Ok(Cmd::Key { key, mods }) => match parse_key(&key) {
                        Some(kc) => {
                            if mods & 8 != 0 {
                                if let Some(s) = encode_super_key(&kc, mods) {
                                    let mut w = writer.lock().unwrap();
                                    let _ = w.write_all(s.as_bytes());
                                    let _ = w.flush();
                                } else {
                                    let _ = term
                                        .lock()
                                        .unwrap()
                                        .key_down(kc, parse_mods(mods & !8));
                                }
                            } else {
                                let _ = term.lock().unwrap().key_down(kc, parse_mods(mods));
                            }
                        }
                        None => eprintln!("ptyZZZ: unknown key: {key}"),
                    },
                    Ok(Cmd::Paste { s }) => {
                        let _ = term.lock().unwrap().send_paste(&s);
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
            if exit_on_eof {
                let _ = child.lock().unwrap().kill();
            }
        });
    }

    // emitter: wait on dirty (or an owed keyframe), coalesce, emit frames.
    let out = std::io::stdout();
    let mut state = EmitState::new(target);
    let mut last_gen = u64::MAX; // != 0 so the first pass emits immediately
    // Leading-edge pacing: a frame's production may start no sooner than
    // `coalesce` after the previous frame's production started. After idle
    // the deadline is already past, so the first change emits immediately;
    // during a burst, production starts on a constant cadence, and the time
    // spent producing a frame is not charged against the gate.
    let mut last_frame_at = Instant::now().checked_sub(coalesce).unwrap_or_else(Instant::now);
    loop {
        {
            let (lock, cv) = &*dirty;
            let mut g = lock.lock().unwrap();
            loop {
                if *g != last_gen || done.load(Ordering::SeqCst) {
                    break;
                }
                let keyframe_in = if state.dirty_since_keyframe {
                    keyframe_interval.saturating_sub(state.last_keyframe.elapsed())
                } else {
                    Duration::from_secs(3600)
                };
                let due_in = keyframe_in.min(
                    state
                        .cursor_hide_due_in()
                        .unwrap_or(Duration::from_secs(3600)),
                );
                if due_in.is_zero() {
                    break;
                }
                let (ng, _) = cv.wait_timeout(g, due_in.min(Duration::from_secs(1))).unwrap();
                g = ng;
            }
            last_gen = *g;
        }
        if !done.load(Ordering::SeqCst) {
            std::thread::sleep((last_frame_at + coalesce).saturating_duration_since(Instant::now()));
            let (lock, _) = &*dirty;
            last_gen = *lock.lock().unwrap();
        }

        let keyframe_due =
            state.dirty_since_keyframe && state.last_keyframe.elapsed() >= keyframe_interval;
        last_frame_at = Instant::now();
        let frame = {
            let mut t = term.lock().unwrap();
            state.produce(&mut t, keyframe_due)
        };
        if let Some(v) = frame {
            let mut w = out.lock();
            // A dead stdout means no one is listening (the adapter that
            // spawned us is gone): exit instead of rendering forever.
            if writeln!(w, "{v}").and_then(|_| w.flush()).is_err() {
                let _ = child.lock().unwrap().kill();
                break;
            }
        }

        if done.load(Ordering::SeqCst) {
            break;
        }
    }

    let code = child.lock().unwrap().wait().map(|s| s.exit_code() as i64).unwrap_or(-1);
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

/// How long the cursor must stay DECTCEM-hidden before the client is told.
/// Long enough to swallow a repaint's hide/show pair (TUIs cycle those about
/// ten times a second), short enough that a real hide reads as immediate.
const CURSOR_HIDE_GRACE: Duration = Duration::from_millis(120);

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
    /// cursor state the client currently has: position plus visibility. Only
    /// ever advanced from a sample taken while the cursor was visible.
    shown_cursor: (usize, usize, bool),
    /// when the terminal's cursor first went DECTCEM-hidden, cleared the
    /// moment it comes back. Drives the hide grace below.
    hidden_since: Option<Instant>,
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
            shown_cursor: (usize::MAX, usize::MAX, true),
            hidden_since: None,
            sent_initial: false,
            last_keyframe: Instant::now(),
            dirty_since_keyframe: false,
            scan_seqno: 0,
        }
    }

    /// Time until a pending hide is owed to the client, if one is pending. The
    /// emitter waits on this: a grace can expire with no further damage to wake
    /// it (an app hides the cursor, finishes its repaint, then sits idle).
    fn cursor_hide_due_in(&self) -> Option<Duration> {
        let since = self.hidden_since?;
        if !self.shown_cursor.2 {
            return None; // already hidden on the client
        }
        Some(CURSOR_HIDE_GRACE.saturating_sub(since.elapsed()))
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
        // DECTCEM off means "this position is scratch". A TUI hides the cursor,
        // repaints (leaving it wherever the repaint ended -- a spinner row,
        // column 0), then parks it and shows it again. Sampling every 16ms
        // catches both halves, so adopting a hidden sample strobed the overlay
        // between the repaint row and the prompt. Two rules follow:
        //
        //   - a hidden sample never moves the cursor. Its coordinates are only
        //     ever a repaint artifact.
        //   - a hide reaches the client only if it outlives CURSOR_HIDE_GRACE.
        //     A repaint's hide/show pair is milliseconds; an app that turns the
        //     cursor off and leaves it off crosses the grace and goes dark.
        let now = Instant::now();
        let raw = (
            total.saturating_sub(rows) + cursor.y.max(0) as usize,
            cursor.x,
            cursor.visibility == wezterm_surface::CursorVisibility::Visible,
        );
        if raw.2 {
            self.hidden_since = None;
        } else if self.hidden_since.is_none() {
            self.hidden_since = Some(now);
        }
        let cursor_now = if raw.2 || !self.sent_initial {
            // The first frame has no shown position to hold, so it takes the
            // sample as-is however the app left it.
            raw
        } else if now.duration_since(self.hidden_since.unwrap()) >= CURSOR_HIDE_GRACE {
            (self.shown_cursor.0, self.shown_cursor.1, false)
        } else {
            self.shown_cursor
        };

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

        let cursor_moved = cursor_now != self.shown_cursor;
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
            render_cursor_into(&mut html, &self.target, cursor_now.0, cursor_now.1, cursor_now.2);
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
                render_cursor_into(&mut patch, &self.target, cursor_now.0, cursor_now.1, cursor_now.2);
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
        self.shown_cursor = cursor_now;
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
/// DECTCEM hidden renders as display:none. Whether a hide is real, and which
/// position survives it, is decided in `produce` -- see CURSOR_HIDE_GRACE.
fn render_cursor_into(out: &mut String, target: &str, row: usize, col: usize, visible: bool) {
    let display = if visible { "" } else { ";display:none" };
    let _ = write!(
        out,
        "<div class=\"cursor\" id=\"{target}-cursor\" style=\"--cursor-row:{row};--cursor-col:{col}{display}\"></div>"
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
