// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>
// SPDX-FileCopyrightText: Dominic Radermacher and the ptouch-print contributors
//
// Portions derived from ptouch-print, licensed GPL-3.0-or-later:
// https://git.familie-radermacher.ch/linux/ptouch-print.git

//! Convert a [`LabelBitmap`] to raster lines suitable for Brother P-Touch
//! printers.
//!
//! The printer receives data column-by-column. Each raster line corresponds
//! to one vertical column of the label, and the image is centered vertically
//! on the tape.
//!
//! The bit layout within each raster line uses reversed byte order with
//! LSB-first bit ordering:
//!
//! ```text
//! rasterline[(size-1)-(pixel/8)] |= 1 << (pixel % 8)
//! ```

use crate::bitmap::LabelBitmap;

/// Set a single pixel in a raster line buffer.
///
/// - `rasterline`: byte buffer representing one vertical column
/// - `size`: total number of bytes in the raster line
/// - `pixel`: pixel index (0 = bottom of physical tape)
#[inline]
fn rasterline_setpixel(rasterline: &mut [u8], pixel: usize) {
    let size = rasterline.len();
    let byte_pos = pixel / 8;
    if byte_pos < size {
        rasterline[size - 1 - byte_pos] |= 1u8 << (pixel % 8);
    }
}

/// Convert a [`LabelBitmap`] into raster lines for the printer.
///
/// - `bitmap`: the rendered label bitmap
/// - `max_px`: maximum pixel height of the tape (from device/tape info)
///
/// Returns a `Vec` of raster lines. Each raster line is a `Vec<u8>` of
/// length `max_px / 8` bytes. One raster line per horizontal column of
/// the bitmap.
///
/// The image is centered vertically on the tape:
/// ```text
/// offset = (max_pixels / 2) - (image_height / 2)
/// ```
///
/// Within each column, pixels are read bottom-to-top from the bitmap
/// (y is flipped) to match the Brother P-Touch raster orientation.
pub fn bitmap_to_raster_lines(bitmap: &LabelBitmap, max_px: u16) -> Vec<Vec<u8>> {
    // Center the image vertically on the tape
    let offset = ((max_px as usize) / 2).saturating_sub(bitmap.height() as usize / 2);
    raster_lines_with_offset(bitmap, max_px, offset)
}

/// Like [`bitmap_to_raster_lines`], but places the band explicitly instead of
/// centring it: `left_px` is the number of unused pins before the band, counted
/// from pin 0 (the MSB of the first byte), as in the PT-P900 geometry table.
///
/// The top row of the bitmap lands on pin `left_px`.
///
/// E850-verified: which end of the head is the top of the tape is
/// confirmed by the P-touch Editor "F" capture (2026-10-06): top row at the
/// lowest pin, first raster line at the left edge of the label.
pub fn bitmap_to_raster_lines_at(bitmap: &LabelBitmap, max_px: u16, left_px: u16) -> Vec<Vec<u8>> {
    // Pixel indices count from the last pin (see `rasterline_setpixel`), so the
    // distance from the far end is head width - left margin - band height.
    let offset = (max_px as usize)
        .saturating_sub(left_px as usize)
        .saturating_sub(bitmap.height() as usize);
    raster_lines_with_offset(bitmap, max_px, offset)
}

fn raster_lines_with_offset(bitmap: &LabelBitmap, max_px: u16, offset: usize) -> Vec<Vec<u8>> {
    let raster_size = (max_px as usize) / 8;
    let bmp_height = bitmap.height() as usize;
    let bmp_width = bitmap.width() as usize;

    let mut lines = Vec::with_capacity(bmp_width);

    for k in 0..bmp_width {
        let mut rasterline = vec![0u8; raster_size];

        for i in 0..bmp_height {
            // Read from the bitmap with Y flipped (bottom-to-top)
            let bmp_y = bmp_height - 1 - i;
            if bitmap.get_pixel(k as u32, bmp_y as u32) {
                rasterline_setpixel(&mut rasterline, offset + i);
            }
        }

        lines.push(rasterline);
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rasterline_setpixel_basic() {
        let mut line = vec![0u8; 4];
        // pixel 0 should set bit 0 of the last byte
        rasterline_setpixel(&mut line, 0);
        assert_eq!(line, [0, 0, 0, 1]);

        let mut line = vec![0u8; 4];
        // pixel 8 should set bit 0 of byte at index size-2
        rasterline_setpixel(&mut line, 8);
        assert_eq!(line, [0, 0, 1, 0]);

        let mut line = vec![0u8; 4];
        // pixel 7 should set bit 7 of the last byte
        rasterline_setpixel(&mut line, 7);
        assert_eq!(line, [0, 0, 0, 128]);
    }

    #[test]
    fn test_bitmap_to_raster_single_column() {
        // Create a 1-pixel wide, 8-pixel tall bitmap with top pixel set
        let mut bmp = LabelBitmap::new(1, 8);
        bmp.set_pixel(0, 0, true); // top pixel

        let lines = bitmap_to_raster_lines(&bmp, 16);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), 2); // 16/8 = 2 bytes

        // offset = 16/2 - 8/2 = 4
        // i iterates 0..8, bmp_y = 7 - i
        // When i=7, bmp_y=0, which is set -> pixel at offset+7 = 11
        // pixel 11 -> byte index = 2-1-11/8 = 2-1-1 = 0, bit = 11%8 = 3
        assert_eq!(lines[0][0], 0b0000_1000); // bit 3 set in byte 0
    }

    /// Index of the first and last set pin (pin 0 = MSB of byte 0).
    fn ink_span(line: &[u8]) -> Option<(usize, usize)> {
        let pins: Vec<usize> = (0..line.len() * 8)
            .filter(|p| line[p / 8] & (0x80 >> (p % 8)) != 0)
            .collect();
        Some((*pins.first()?, *pins.last()?))
    }

    #[test]
    fn test_band_placed_at_left_offset_12mm() {
        // Full-height column on a 560 pin head, TZe 12mm: pins 213..=362.
        let mut bmp = LabelBitmap::new(1, 150);
        for y in 0..150 {
            bmp.set_pixel(0, y, true);
        }
        let lines = bitmap_to_raster_lines_at(&bmp, 560, 213);
        assert_eq!(lines[0].len(), 70);
        assert_eq!(ink_span(&lines[0]), Some((213, 362)));
    }

    #[test]
    fn test_band_placed_at_left_offset_36mm_and_9mm() {
        for (left, pins) in [(61u16, 454u32), (235, 106), (240, 96)] {
            let mut bmp = LabelBitmap::new(1, pins);
            for y in 0..pins {
                bmp.set_pixel(0, y, true);
            }
            let lines = bitmap_to_raster_lines_at(&bmp, 560, left);
            assert_eq!(
                ink_span(&lines[0]),
                Some((left as usize, left as usize + pins as usize - 1))
            );
        }
    }

    #[test]
    fn test_top_row_lands_on_lowest_pin() {
        let mut bmp = LabelBitmap::new(1, 106);
        bmp.set_pixel(0, 0, true); // top row only
        let lines = bitmap_to_raster_lines_at(&bmp, 560, 235);
        assert_eq!(ink_span(&lines[0]), Some((235, 235)));
    }

    #[test]
    fn test_centred_and_explicit_agree_for_a_centred_band() {
        let mut bmp = LabelBitmap::new(2, 100);
        bmp.set_pixel(0, 3, true);
        bmp.set_pixel(1, 90, true);
        // 128 pin head, 100 pin band centred: left margin = 14.
        assert_eq!(
            bitmap_to_raster_lines(&bmp, 128),
            bitmap_to_raster_lines_at(&bmp, 128, 14)
        );
    }

    #[test]
    fn test_empty_bitmap_produces_empty_rasters() {
        let bmp = LabelBitmap::new(0, 8);
        let lines = bitmap_to_raster_lines(&bmp, 16);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_raster_line_count_matches_width() {
        let bmp = LabelBitmap::new(100, 64);
        let lines = bitmap_to_raster_lines(&bmp, 128);
        assert_eq!(lines.len(), 100);
        for line in &lines {
            assert_eq!(line.len(), 16); // 128/8
        }
    }

    #[test]
    fn test_setpixel_out_of_bounds_is_ignored() {
        let mut line = vec![0u8; 4]; // 32 pixels max
        rasterline_setpixel(&mut line, 32); // exactly out of bounds
        rasterline_setpixel(&mut line, 100); // far out of bounds
        assert_eq!(line, [0, 0, 0, 0]); // nothing written
    }

    #[test]
    fn test_bitmap_taller_than_max_px() {
        // Bitmap height (192) exceeds max_px (128) -- must not panic
        let mut bmp = LabelBitmap::new(2, 192);
        bmp.set_pixel(0, 0, true);
        bmp.set_pixel(1, 191, true);
        let lines = bitmap_to_raster_lines(&bmp, 128);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), 16); // 128/8
    }
}
