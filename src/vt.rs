//! Thin batched-FFI layer over libghostty-vt-sys.
//!
//! The safe libghostty-vt wrapper issues one C call per field, so describing
//! one cell costs ~6 ABI crossings, and its raw handles are crate-private,
//! which puts the batched `*_get_multi` C calls out of reach. This module
//! talks to the sys crate directly instead: one `get_multi` fetches the raw
//! cell plus its style from the render-state iterator, and a second decodes
//! codepoint, content tag, and wide state from the cell value. A viewport
//! cell costs 3 crossings (including the iterator step), a scrollback cell 2.
//! Scrollback rows also reuse one grid ref per row: `GhosttyGridRef` is a
//! public (node, x, y) pin, and every column of a row lives in the same page
//! node, so stepping x locally replaces a per-cell terminal call.
//!
//! All unsafe lives here; main.rs consumes the narrow safe API.

use std::io::Write;
use std::os::raw::c_void;
use std::sync::{Arc, Mutex};

pub use libghostty_vt_sys as ffi;

pub type PtyWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// A color as carried by a style attribute: unset, palette index, or rgb.
#[derive(Clone, Copy, PartialEq)]
pub enum Col {
    None,
    Palette(u8),
    Rgb(u8, u8, u8),
}

pub fn style_col(c: &ffi::StyleColor) -> Col {
    match c.tag {
        ffi::StyleColorTag::PALETTE => Col::Palette(unsafe { c.value.palette }),
        ffi::StyleColorTag::RGB => {
            let v = unsafe { c.value.rgb };
            Col::Rgb(v.r, v.g, v.b)
        }
        _ => Col::None,
    }
}

/// Everything the renderer needs to know about one cell. `style` is the
/// zeroed default for unstyled cells (the C API returns exactly that).
pub struct CellFacts {
    pub codepoint: u32,
    pub tag: ffi::CellContentTag::Type,
    pub wide: ffi::CellWide::Type,
    pub style: ffi::Style,
    raw: ffi::Cell,
}

impl Default for CellFacts {
    fn default() -> Self {
        Self {
            codepoint: 0,
            tag: ffi::CellContentTag::CODEPOINT,
            wide: ffi::CellWide::NARROW,
            style: style_default(),
            raw: 0,
        }
    }
}

impl CellFacts {
    /// The background color a bg-content cell carries in the cell itself
    /// (an erase with a color set). `Col::None` for text cells.
    pub fn content_bg(&self) -> Col {
        unsafe {
            match self.tag {
                ffi::CellContentTag::BG_COLOR_PALETTE => {
                    let mut i: u8 = 0;
                    let r = ffi::ghostty_cell_get(
                        self.raw,
                        ffi::CellData::COLOR_PALETTE,
                        (&mut i as *mut u8).cast(),
                    );
                    if r == ffi::Result::SUCCESS {
                        Col::Palette(i)
                    } else {
                        Col::None
                    }
                }
                ffi::CellContentTag::BG_COLOR_RGB => {
                    let mut c = ffi::ColorRgb { r: 0, g: 0, b: 0 };
                    let r = ffi::ghostty_cell_get(
                        self.raw,
                        ffi::CellData::COLOR_RGB,
                        (&mut c as *mut ffi::ColorRgb).cast(),
                    );
                    if r == ffi::Result::SUCCESS {
                        Col::Rgb(c.r, c.g, c.b)
                    } else {
                        Col::None
                    }
                }
                _ => Col::None,
            }
        }
    }
}

pub struct Scalars {
    pub alt: bool,
    pub cols: u16,
    pub rows: u16,
    pub scrollback_rows: u64,
    pub cursor_x: u16,
    pub cursor_y: u16,
}

fn style_default() -> ffi::Style {
    let mut s: ffi::Style = unsafe { std::mem::zeroed() };
    s.size = std::mem::size_of::<ffi::Style>();
    s
}

fn active_origin() -> ffi::Point {
    ffi::Point {
        tag: ffi::PointTag::ACTIVE,
        value: ffi::PointValue {
            coordinate: ffi::PointCoordinate { x: 0, y: 0 },
        },
    }
}

unsafe extern "C" fn write_pty_cb(
    _term: ffi::Terminal,
    userdata: *mut c_void,
    data: *const u8,
    len: usize,
) {
    if len == 0 || data.is_null() {
        return;
    }
    // SAFETY: userdata is the boxed PtyWriter owned by Vt, which outlives
    // the terminal; callbacks fire synchronously inside vt_write.
    let writer = unsafe { &*(userdata as *const PtyWriter) };
    let bytes = unsafe { std::slice::from_raw_parts(data, len) };
    let mut w = writer.lock().unwrap();
    let _ = w.write_all(bytes);
    let _ = w.flush();
}

/// One ghostty terminal plus its render-state machinery and the tracked
/// grid ref main.rs uses as a scroll odometer.
pub struct Vt {
    term: ffi::Terminal,
    state: ffi::RenderState,
    rows: ffi::RenderStateRowIterator,
    cells: ffi::RenderStateRowCells,
    pin: ffi::TrackedGridRef,
    _writer: Box<PtyWriter>,
}

impl Vt {
    pub fn new(cols: u16, rows: u16, max_scrollback: usize, writer: PtyWriter) -> Self {
        unsafe {
            let mut term: ffi::Terminal = std::ptr::null_mut();
            let r = ffi::ghostty_terminal_new(
                std::ptr::null(),
                &mut term,
                ffi::TerminalOptions {
                    cols,
                    rows,
                    max_scrollback,
                },
            );
            assert!(
                r == ffi::Result::SUCCESS && !term.is_null(),
                "ghostty_terminal_new"
            );

            let writer = Box::new(writer);
            let ud = (&*writer as *const PtyWriter).cast::<c_void>();
            let r = ffi::ghostty_terminal_set(term, ffi::TerminalOption::USERDATA, ud);
            assert_eq!(r, ffi::Result::SUCCESS, "set USERDATA");
            let cb: unsafe extern "C" fn(ffi::Terminal, *mut c_void, *const u8, usize) =
                write_pty_cb;
            let r =
                ffi::ghostty_terminal_set(term, ffi::TerminalOption::WRITE_PTY, cb as *const c_void);
            assert_eq!(r, ffi::Result::SUCCESS, "set WRITE_PTY");

            let mut state: ffi::RenderState = std::ptr::null_mut();
            let r = ffi::ghostty_render_state_new(std::ptr::null(), &mut state);
            assert!(r == ffi::Result::SUCCESS && !state.is_null(), "render state");
            let mut rows_it: ffi::RenderStateRowIterator = std::ptr::null_mut();
            let r = ffi::ghostty_render_state_row_iterator_new(std::ptr::null(), &mut rows_it);
            assert!(r == ffi::Result::SUCCESS && !rows_it.is_null(), "row iter");
            let mut cells: ffi::RenderStateRowCells = std::ptr::null_mut();
            let r = ffi::ghostty_render_state_row_cells_new(std::ptr::null(), &mut cells);
            assert!(r == ffi::Result::SUCCESS && !cells.is_null(), "cells iter");

            Vt {
                term,
                state,
                rows: rows_it,
                cells,
                pin: std::ptr::null_mut(),
                _writer: writer,
            }
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        unsafe { ffi::ghostty_terminal_vt_write(self.term, bytes.as_ptr(), bytes.len()) }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        unsafe {
            let _ = ffi::ghostty_terminal_resize(self.term, cols, rows, 0, 0);
        }
    }

    /// All per-tick terminal scalars in one crossing.
    pub fn scalars(&self) -> Scalars {
        let mut screen: ffi::TerminalScreen::Type = 0;
        let mut cols: u16 = 0;
        let mut rows: u16 = 0;
        let mut sb: usize = 0;
        let mut cx: u16 = 0;
        let mut cy: u16 = 0;
        let keys = [
            ffi::TerminalData::ACTIVE_SCREEN,
            ffi::TerminalData::COLS,
            ffi::TerminalData::ROWS,
            ffi::TerminalData::SCROLLBACK_ROWS,
            ffi::TerminalData::CURSOR_X,
            ffi::TerminalData::CURSOR_Y,
        ];
        let mut vals = [
            (&mut screen as *mut ffi::TerminalScreen::Type).cast::<c_void>(),
            (&mut cols as *mut u16).cast(),
            (&mut rows as *mut u16).cast(),
            (&mut sb as *mut usize).cast(),
            (&mut cx as *mut u16).cast(),
            (&mut cy as *mut u16).cast(),
        ];
        unsafe {
            let r = ffi::ghostty_terminal_get_multi(
                self.term,
                keys.len(),
                keys.as_ptr(),
                vals.as_mut_ptr(),
                std::ptr::null_mut(),
            );
            debug_assert_eq!(r, ffi::Result::SUCCESS);
        }
        Scalars {
            alt: screen == ffi::TerminalScreen::ALTERNATE,
            cols,
            rows,
            scrollback_rows: sb as u64,
            cursor_x: cx,
            cursor_y: cy,
        }
    }

    // --- scroll odometer ----------------------------------------------------

    pub fn has_pin(&self) -> bool {
        !self.pin.is_null()
    }

    /// Screen-space y of the pinned cell, or None if the pin was lost.
    pub fn pin_screen_y(&self) -> Option<u64> {
        if self.pin.is_null() {
            return None;
        }
        let mut pc = ffi::PointCoordinate { x: 0, y: 0 };
        let r = unsafe {
            ffi::ghostty_tracked_grid_ref_point(self.pin, ffi::PointTag::SCREEN, &mut pc)
        };
        (r == ffi::Result::SUCCESS).then_some(pc.y as u64)
    }

    /// Move (or create) the pin at the active top-left. On failure the pin
    /// is dropped, which sends the caller down the rebuild path next tick.
    pub fn repin(&mut self) {
        unsafe {
            if self.pin.is_null() {
                let mut p: ffi::TrackedGridRef = std::ptr::null_mut();
                let r = ffi::ghostty_terminal_grid_ref_track(self.term, active_origin(), &mut p);
                if r == ffi::Result::SUCCESS && !p.is_null() {
                    self.pin = p;
                }
            } else if ffi::ghostty_tracked_grid_ref_set(self.pin, self.term, active_origin())
                != ffi::Result::SUCCESS
            {
                ffi::ghostty_tracked_grid_ref_free(self.pin);
                self.pin = std::ptr::null_mut();
            }
        }
    }

    // --- viewport pass ------------------------------------------------------

    /// Snapshot the terminal into the render state and prime the row
    /// iterator. Returns whether the whole frame is dirty.
    pub fn begin_frame(&mut self) -> bool {
        unsafe {
            let r = ffi::ghostty_render_state_update(self.state, self.term);
            debug_assert_eq!(r, ffi::Result::SUCCESS);
            let mut dirty: ffi::RenderStateDirty::Type = 0;
            let keys = [
                ffi::RenderStateData::DIRTY,
                ffi::RenderStateData::ROW_ITERATOR,
            ];
            let mut vals = [
                (&mut dirty as *mut ffi::RenderStateDirty::Type).cast::<c_void>(),
                (&mut self.rows as *mut ffi::RenderStateRowIterator).cast(),
            ];
            let r = ffi::ghostty_render_state_get_multi(
                self.state,
                keys.len(),
                keys.as_ptr(),
                vals.as_mut_ptr(),
                std::ptr::null_mut(),
            );
            debug_assert_eq!(r, ffi::Result::SUCCESS);
            dirty == ffi::RenderStateDirty::FULL
        }
    }

    /// Reset the global dirty state after a frame.
    pub fn end_frame(&mut self) {
        let clean: ffi::RenderStateDirty::Type = ffi::RenderStateDirty::FALSE;
        unsafe {
            let _ = ffi::ghostty_render_state_set(
                self.state,
                ffi::RenderStateOption::DIRTY,
                (&clean as *const ffi::RenderStateDirty::Type).cast(),
            );
        }
    }

    /// Step to the next viewport row; primes the cell iterator and returns
    /// the row's dirty flag. One crossing for the step, one for both fields.
    pub fn next_row(&mut self) -> Option<bool> {
        unsafe {
            if !ffi::ghostty_render_state_row_iterator_next(self.rows) {
                return None;
            }
            let mut dirty = false;
            let keys = [
                ffi::RenderStateRowData::DIRTY,
                ffi::RenderStateRowData::CELLS,
            ];
            let mut vals = [
                (&mut dirty as *mut bool).cast::<c_void>(),
                (&mut self.cells as *mut ffi::RenderStateRowCells).cast(),
            ];
            let r = ffi::ghostty_render_state_row_get_multi(
                self.rows,
                keys.len(),
                keys.as_ptr(),
                vals.as_mut_ptr(),
                std::ptr::null_mut(),
            );
            debug_assert_eq!(r, ffi::Result::SUCCESS);
            Some(dirty)
        }
    }

    pub fn mark_row_clean(&mut self) {
        let v = false;
        unsafe {
            let _ = ffi::ghostty_render_state_row_set(
                self.rows,
                ffi::RenderStateRowOption::DIRTY,
                (&v as *const bool).cast(),
            );
        }
    }

    /// Step to the next cell of the current row and describe it: one
    /// crossing for the step, one for raw+style, one to decode the cell.
    pub fn next_cell(&mut self, f: &mut CellFacts) -> bool {
        unsafe {
            if !ffi::ghostty_render_state_row_cells_next(self.cells) {
                return false;
            }
            let mut raw: ffi::Cell = 0;
            f.style.size = std::mem::size_of::<ffi::Style>();
            {
                let keys = [
                    ffi::RenderStateRowCellsData::RAW,
                    ffi::RenderStateRowCellsData::STYLE,
                ];
                let mut vals = [
                    (&mut raw as *mut ffi::Cell).cast::<c_void>(),
                    (&mut f.style as *mut ffi::Style).cast(),
                ];
                let r = ffi::ghostty_render_state_row_cells_get_multi(
                    self.cells,
                    keys.len(),
                    keys.as_ptr(),
                    vals.as_mut_ptr(),
                    std::ptr::null_mut(),
                );
                debug_assert_eq!(r, ffi::Result::SUCCESS);
            }
            {
                let keys = [
                    ffi::CellData::CODEPOINT,
                    ffi::CellData::CONTENT_TAG,
                    ffi::CellData::WIDE,
                ];
                let mut vals = [
                    (&mut f.codepoint as *mut u32).cast::<c_void>(),
                    (&mut f.tag as *mut ffi::CellContentTag::Type).cast(),
                    (&mut f.wide as *mut ffi::CellWide::Type).cast(),
                ];
                let r = ffi::ghostty_cell_get_multi(
                    raw,
                    keys.len(),
                    keys.as_ptr(),
                    vals.as_mut_ptr(),
                    std::ptr::null_mut(),
                );
                debug_assert_eq!(r, ffi::Result::SUCCESS);
            }
            f.raw = raw;
            true
        }
    }

    /// Append the current cell's grapheme cluster to `out` (viewport path).
    pub fn cell_graphemes(&mut self, out: &mut String) {
        unsafe {
            let mut len: u32 = 0;
            let r = ffi::ghostty_render_state_row_cells_get(
                self.cells,
                ffi::RenderStateRowCellsData::GRAPHEMES_LEN,
                (&mut len as *mut u32).cast(),
            );
            if r != ffi::Result::SUCCESS || len == 0 {
                return;
            }
            let mut buf = [0u32; 64];
            if len as usize > buf.len() {
                return;
            }
            let r = ffi::ghostty_render_state_row_cells_get(
                self.cells,
                ffi::RenderStateRowCellsData::GRAPHEMES_BUF,
                buf.as_mut_ptr().cast(),
            );
            if r == ffi::Result::SUCCESS {
                for cp in &buf[..len as usize] {
                    if let Some(c) = char::from_u32(*cp) {
                        out.push(c);
                    }
                }
            }
        }
    }

    // --- scrollback pass ----------------------------------------------------

    /// Resolve one grid ref for a screen-space row. Cells are then read by
    /// stepping the ref's public x locally, one page lookup per cell.
    pub fn history_row(&self, screen_y: u64) -> Option<HistRow> {
        let point = ffi::Point {
            tag: ffi::PointTag::SCREEN,
            value: ffi::PointValue {
                coordinate: ffi::PointCoordinate {
                    x: 0,
                    y: screen_y as u32,
                },
            },
        };
        let mut gr: ffi::GridRef = unsafe { std::mem::zeroed() };
        gr.size = std::mem::size_of::<ffi::GridRef>();
        let r = unsafe { ffi::ghostty_terminal_grid_ref(self.term, point, &mut gr) };
        (r == ffi::Result::SUCCESS && !gr.node.is_null()).then_some(HistRow { gr })
    }
}

impl Drop for Vt {
    fn drop(&mut self) {
        unsafe {
            ffi::ghostty_render_state_row_cells_free(self.cells);
            ffi::ghostty_render_state_row_iterator_free(self.rows);
            ffi::ghostty_render_state_free(self.state);
            if !self.pin.is_null() {
                ffi::ghostty_tracked_grid_ref_free(self.pin);
            }
            ffi::ghostty_terminal_free(self.term);
        }
    }
}

/// A grid ref pinned on one scrollback row. Valid only until the terminal
/// is next mutated; main.rs reads history rows inside a single quiescent
/// frame production, which satisfies that.
pub struct HistRow {
    gr: ffi::GridRef,
}

impl HistRow {
    /// Describe the cell at column x: one crossing for the page lookup,
    /// one to decode the cell, one more only for styled cells.
    pub fn cell(&mut self, x: u16, f: &mut CellFacts) -> bool {
        self.gr.x = x;
        unsafe {
            let mut raw: ffi::Cell = 0;
            if ffi::ghostty_grid_ref_cell(&self.gr, &mut raw) != ffi::Result::SUCCESS {
                return false;
            }
            let mut has_styling = false;
            {
                let keys = [
                    ffi::CellData::CODEPOINT,
                    ffi::CellData::CONTENT_TAG,
                    ffi::CellData::WIDE,
                    ffi::CellData::HAS_STYLING,
                ];
                let mut vals = [
                    (&mut f.codepoint as *mut u32).cast::<c_void>(),
                    (&mut f.tag as *mut ffi::CellContentTag::Type).cast(),
                    (&mut f.wide as *mut ffi::CellWide::Type).cast(),
                    (&mut has_styling as *mut bool).cast(),
                ];
                let r = ffi::ghostty_cell_get_multi(
                    raw,
                    keys.len(),
                    keys.as_ptr(),
                    vals.as_mut_ptr(),
                    std::ptr::null_mut(),
                );
                if r != ffi::Result::SUCCESS {
                    return false;
                }
            }
            f.style = style_default();
            if has_styling {
                let _ = ffi::ghostty_grid_ref_style(&self.gr, &mut f.style);
            }
            f.raw = raw;
            true
        }
    }

    /// Append the current cell's grapheme cluster to `out`.
    pub fn graphemes(&self, out: &mut String) {
        let mut buf = [0u32; 64];
        let mut len: usize = 0;
        let r = unsafe {
            ffi::ghostty_grid_ref_graphemes(&self.gr, buf.as_mut_ptr(), buf.len(), &mut len)
        };
        if r == ffi::Result::SUCCESS {
            for cp in &buf[..len.min(buf.len())] {
                if let Some(c) = char::from_u32(*cp) {
                    out.push(c);
                }
            }
        }
    }
}
