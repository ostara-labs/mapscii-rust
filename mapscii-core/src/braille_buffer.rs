//! Pixel buffer that maps 2×4 pixel blocks to Unicode Braille characters.
//!
//! Each terminal cell represents a 2-wide by 4-tall grid of pixels using
//! Unicode Braille patterns (U+2800..U+28FF). Foreground and background
//! colors are tracked per cell using xterm-256 color indices.
//!
//! Translated from the original mapscii `BrailleBuffer.js`.

use ratatui::buffer::Buffer as RatatuiBuffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use unicode_width::UnicodeWidthStr;

use crate::utils;

// -- Braille dot layout ----------------------------------------------------
// Each cell is 2 columns x 4 rows of dots.  Bit positions in U+2800:
//
//   col 0   col 1
//   0x01    0x08   row 0
//   0x02    0x10   row 1
//   0x04    0x20   row 2
//   0x40    0x80   row 3

const BRAILLE_MAP: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

/// ASCII block-art fallback characters and the braille masks they approximate.
const ASCII_MAP: &[(&str, &[u8])] = &[
    ("\u{2580}", &[1 + 2 + 16 + 32]),    // upper half
    ("\u{2584}", &[4 + 8 + 64 + 128]),   // lower half
    ("\u{25A0}", &[2 + 4 + 32 + 64]),    // middle
    ("\u{258C}", &[1 + 2 + 4 + 8]),      // left half
    ("\u{2590}", &[16 + 32 + 64 + 128]), // right half
    ("\u{2588}", &[255]),                // full block
];

/// xterm-256 color index (0 = no color / default).
pub type ColorIdx = u8;

// -- xterm-256 palette -----------------------------------------------------

/// Build the full 256-entry xterm palette as [r,g,b].
fn build_xterm_palette() -> [[u8; 3]; 256] {
    let mut palette = [[0u8; 3]; 256];

    // Standard 16 colors
    let base16: [[u8; 3]; 16] = [
        [0, 0, 0],
        [128, 0, 0],
        [0, 128, 0],
        [128, 128, 0],
        [0, 0, 128],
        [128, 0, 128],
        [0, 128, 128],
        [192, 192, 192],
        [128, 128, 128],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [0, 0, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];
    palette[..16].copy_from_slice(&base16);

    // 6x6x6 color cube (indices 16..232)
    let levels = [0u8, 95, 135, 175, 215, 255];
    for r in 0..6u8 {
        for g in 0..6u8 {
            for b in 0..6u8 {
                let idx = 16 + (r as usize) * 36 + (g as usize) * 6 + (b as usize);
                palette[idx] = [levels[r as usize], levels[g as usize], levels[b as usize]];
            }
        }
    }

    // Grayscale ramp (indices 232..256)
    for i in 0..24u8 {
        let v = 8 + 10 * i;
        palette[232 + i as usize] = [v, v, v];
    }

    palette
}

/// The xterm-256 palette, built once.
static XTERM_PALETTE: std::sync::LazyLock<[[u8; 3]; 256]> =
    std::sync::LazyLock::new(build_xterm_palette);

/// Find the closest xterm-256 color index for an RGB triple.
pub fn rgb_to_xterm(r: u8, g: u8, b: u8) -> ColorIdx {
    let mut best_idx: u8 = 0;
    let mut best_dist = u32::MAX;

    for (i, &[pr, pg, pb]) in XTERM_PALETTE.iter().enumerate() {
        let dr = (r as i32 - pr as i32).unsigned_abs();
        let dg = (g as i32 - pg as i32).unsigned_abs();
        let db = (b as i32 - pb as i32).unsigned_abs();
        let dist = dr * dr + dg * dg + db * db;
        if dist < best_dist {
            best_dist = dist;
            best_idx = i as u8;
        }
    }
    best_idx
}

// -- BrailleBuffer ---------------------------------------------------------

/// A pixel-addressable buffer that renders to Braille / ASCII block characters.
///
/// Coordinates are in *pixel* space: `width` x `height` pixels.  Each terminal
/// cell occupies 2x4 pixels, so the terminal grid is `width/2` columns by
/// `height/4` rows.
pub struct BrailleBuffer {
    /// Width in pixels (= terminal columns * 2).
    pub width: usize,
    /// Height in pixels (= terminal rows * 4).
    pub height: usize,

    /// One byte per cell: braille dot bitmask.
    pixel_buffer: Vec<u8>,
    /// Per-cell character override (for text labels).
    char_buffer: Vec<Option<String>>,
    /// Per-cell foreground color (xterm-256 index, 0 = default).
    foreground_buffer: Vec<ColorIdx>,
    /// Per-cell background color (xterm-256 index, 0 = default).
    background_buffer: Vec<ColorIdx>,

    /// ASCII fallback table (pixel bitmask -> best block char).
    ascii_to_braille: Vec<char>,

    /// Global background color applied to every cell.
    pub global_background: ColorIdx,
}

impl BrailleBuffer {
    /// Create a new buffer.  `width` must be even, `height` a multiple of 4.
    pub fn new(width: usize, height: usize) -> Self {
        debug_assert!(width % 2 == 0, "width must be even");
        debug_assert!(height % 4 == 0, "height must be multiple of 4");

        let cell_count = (width / 2) * (height / 4);

        let mut buf = Self {
            width,
            height,
            pixel_buffer: vec![0; cell_count],
            char_buffer: vec![None; cell_count],
            foreground_buffer: vec![0; cell_count],
            background_buffer: vec![0; cell_count],
            ascii_to_braille: Vec::with_capacity(256),
            global_background: 0,
        };
        buf.build_ascii_map();
        buf
    }

    /// Number of terminal columns.
    #[inline]
    pub fn cols(&self) -> usize {
        self.width / 2
    }

    /// Number of terminal rows.
    #[inline]
    pub fn rows(&self) -> usize {
        self.height / 4
    }

    /// Total number of cells.
    #[inline]
    pub fn cell_count(&self) -> usize {
        self.cols() * self.rows()
    }

    /// Count cells that have at least one pixel set.
    pub fn pixel_count(&self) -> usize {
        self.pixel_buffer.iter().filter(|&&b| b != 0).count()
    }

    /// Count cells that have a non-zero foreground color.
    pub fn foreground_count(&self) -> usize {
        self.foreground_buffer.iter().filter(|&&c| c != 0).count()
    }

    /// Count cells that have a character override (text label).
    pub fn char_count(&self) -> usize {
        self.char_buffer.iter().filter(|c| c.is_some()).count()
    }

    /// Clear all pixel data, colors, and character overrides.
    pub fn clear(&mut self) {
        self.pixel_buffer.fill(0);
        for c in self.char_buffer.iter_mut() {
            *c = None;
        }
        self.foreground_buffer.fill(0);
        self.background_buffer.fill(0);
    }

    /// Set the global background color.
    pub fn set_global_background(&mut self, color: ColorIdx) {
        self.global_background = color;
    }

    /// Set the background color for the cell containing pixel `(x, y)`.
    pub fn set_background(&mut self, x: i32, y: i32, color: ColorIdx) {
        if x >= 0 && (x as usize) < self.width && y >= 0 && (y as usize) < self.height {
            let idx = self.project(x as usize, y as usize);
            if idx < self.cell_count() {
                self.background_buffer[idx] = color;
            }
        }
    }

    /// Set a pixel at `(x, y)` with the given foreground color.
    pub fn set_pixel(&mut self, x: i32, y: i32, color: ColorIdx) {
        if let Some((idx, mask)) = self.locate(x, y) {
            self.pixel_buffer[idx] |= mask;
            self.foreground_buffer[idx] = color;
        }
    }

    /// Clear a pixel at `(x, y)`.
    pub fn unset_pixel(&mut self, x: i32, y: i32) {
        if let Some((idx, mask)) = self.locate(x, y) {
            self.pixel_buffer[idx] &= !mask;
        }
    }

    /// Place a character at the cell containing pixel `(x, y)`.
    pub fn set_char(&mut self, ch: &str, x: i32, y: i32, color: ColorIdx) {
        if x >= 0 && (x as usize) < self.width && y >= 0 && (y as usize) < self.height {
            let idx = self.project(x as usize, y as usize);
            if idx < self.cell_count() {
                self.char_buffer[idx] = Some(ch.to_string());
                self.foreground_buffer[idx] = color;
            }
        }
    }

    /// Write a text string starting at pixel `(x, y)`.
    /// If `center` is true, the text is horizontally centered on `x`.
    pub fn write_text(&mut self, text: &str, x: i32, y: i32, color: ColorIdx, center: bool) {
        let x = if center {
            x - (text.len() as i32 / 2 + 1)
        } else {
            x
        };
        for (i, ch) in text.chars().enumerate() {
            let mut tmp = [0u8; 4];
            let s = ch.encode_utf8(&mut tmp);
            self.set_char(s, x + (i as i32) * 2, y, color);
        }
    }

    /// Render the buffer into a ratatui `Buffer` at the given `area`.
    pub fn render_to_ratatui_buffer(&self, area: Rect, buf: &mut RatatuiBuffer, use_braille: bool) {
        let cols = self.cols().min(area.width as usize);
        let rows = self.rows().min(area.height as usize);

        for row in 0..rows {
            let mut skip: usize = 0;

            for col in 0..cols {
                let idx = row * self.cols() + col;
                if idx >= self.cell_count() {
                    break;
                }

                let cell_x = area.x + col as u16;
                let cell_y = area.y + row as u16;
                if cell_x >= area.x + area.width || cell_y >= area.y + area.height {
                    break;
                }

                let fg = self.foreground_buffer[idx];
                let bg = self.background_buffer[idx] | self.global_background;

                let style = Style::default()
                    .fg(xterm_to_ratatui_color(fg))
                    .bg(xterm_to_ratatui_color(bg));

                if let Some(ref ch) = self.char_buffer[idx] {
                    let w = ch.width();
                    if skip == 0 && col + w <= cols {
                        buf.set_string(cell_x, cell_y, ch, style);
                        if w > 1 {
                            skip = w - 1;
                        }
                    } else if skip > 0 {
                        skip -= 1;
                    }
                } else if skip > 0 {
                    skip -= 1;
                } else {
                    let pixel = self.pixel_buffer[idx];
                    let ch = if use_braille {
                        char::from_u32(0x2800 + pixel as u32).unwrap_or(' ')
                    } else {
                        self.ascii_to_braille
                            .get(pixel as usize)
                            .copied()
                            .unwrap_or(' ')
                    };
                    let cell = &mut buf[(cell_x, cell_y)];
                    cell.set_char(ch);
                    cell.set_style(style);
                }
            }
        }
    }

    // -- private helpers ---------------------------------------------------

    /// Map pixel coords to cell index.
    #[inline]
    fn project(&self, x: usize, y: usize) -> usize {
        (x >> 1) + (self.width >> 1) * (y >> 2)
    }

    /// Cell index + braille bitmask for pixel `(x, y)`.
    #[inline]
    fn locate(&self, x: i32, y: i32) -> Option<(usize, u8)> {
        if x < 0 || y < 0 {
            return None;
        }
        let ux = x as usize;
        let uy = y as usize;
        if ux >= self.width || uy >= self.height {
            return None;
        }
        let idx = self.project(ux, uy);
        let mask = BRAILLE_MAP[uy & 3][ux & 1];
        Some((idx, mask))
    }

    /// Build the ASCII fallback lookup table.
    fn build_ascii_map(&mut self) {
        let mut masks: Vec<(u8, char)> = Vec::new();
        for &(ch_str, bits) in ASCII_MAP {
            let ch = ch_str.chars().next().unwrap_or(' ');
            for &mask in bits {
                masks.push((mask, ch));
            }
        }

        self.ascii_to_braille = Vec::with_capacity(256);
        self.ascii_to_braille.push(' '); // index 0 = empty

        for i in 1..=255u8 {
            // Convert pixel-buffer bits to braille bit layout
            // Original JS: (i & 7) + ((i & 56) << 1) + ((i & 64) >> 3) + (i & 128)
            let braille = ((i & 7) as u16)
                + (((i & 56) as u16) << 1)
                + (((i & 64) as u16) >> 3)
                + ((i & 128) as u16);

            let mut best_char = ' ';
            let mut best_covered = 0u32;

            for &(mask, ch) in &masks {
                let covered = utils::population((mask as u32) & (braille as u32));
                if covered > best_covered {
                    best_covered = covered;
                    best_char = ch;
                }
            }
            self.ascii_to_braille.push(best_char);
        }
    }
}

/// Convert an xterm-256 color index to a ratatui `Color`.
/// Index 0 is treated as "default" (reset).
fn xterm_to_ratatui_color(idx: ColorIdx) -> Color {
    if idx == 0 {
        Color::Reset
    } else {
        Color::Indexed(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_dimensions() {
        let buf = BrailleBuffer::new(80, 40);
        assert_eq!(buf.cols(), 40);
        assert_eq!(buf.rows(), 10);
        assert_eq!(buf.cell_count(), 400);
    }

    #[test]
    fn test_set_pixel_and_locate() {
        let mut buf = BrailleBuffer::new(4, 8);
        buf.set_pixel(0, 0, 1);
        assert_eq!(buf.pixel_buffer[0], 0x01);
        buf.set_pixel(1, 0, 1);
        assert_eq!(buf.pixel_buffer[0], 0x01 | 0x08);
        buf.set_pixel(0, 3, 1);
        assert_eq!(buf.pixel_buffer[0], 0x01 | 0x08 | 0x40);
    }

    #[test]
    fn test_unset_pixel() {
        let mut buf = BrailleBuffer::new(4, 4);
        buf.set_pixel(0, 0, 1);
        buf.set_pixel(1, 0, 1);
        buf.unset_pixel(0, 0);
        assert_eq!(buf.pixel_buffer[0], 0x08);
    }

    #[test]
    fn test_out_of_bounds() {
        let mut buf = BrailleBuffer::new(4, 4);
        buf.set_pixel(-1, 0, 1);
        buf.set_pixel(0, -1, 1);
        buf.set_pixel(100, 0, 1);
        buf.set_pixel(0, 100, 1);
        assert_eq!(buf.pixel_buffer[0], 0);
    }

    #[test]
    fn test_rgb_to_xterm_black() {
        assert_eq!(rgb_to_xterm(0, 0, 0), 0);
    }

    #[test]
    fn test_rgb_to_xterm_white() {
        assert_eq!(rgb_to_xterm(255, 255, 255), 15);
    }

    #[test]
    fn test_ascii_map_built() {
        let buf = BrailleBuffer::new(4, 4);
        assert_eq!(buf.ascii_to_braille.len(), 256);
        assert_eq!(buf.ascii_to_braille[0], ' ');
        assert_eq!(buf.ascii_to_braille[255], '\u{2588}');
    }
}
