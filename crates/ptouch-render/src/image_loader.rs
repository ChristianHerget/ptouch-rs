// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Image loading and conversion to 1-bit bitmap.
//!
//! Loads images in any format supported by the `image` crate (PNG, JPEG,
//! GIF, BMP, TIFF, WebP, etc.) plus SVG via `resvg`. Optionally scales to
//! a target height and produces a [`LabelBitmap`] using either threshold
//! binarization (Otsu's method) or Floyd-Steinberg dithering.

use std::io::Read;
use std::path::Path;

use image::imageops::FilterType;
use image::{DynamicImage, GrayImage};

use crate::Result;
use crate::bitmap::LabelBitmap;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Binarization algorithm selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BinarizeMode {
    /// Automatically choose between threshold and dithering based on
    /// histogram analysis.
    #[default]
    Auto,
    /// Otsu's threshold -- best for text, line art, barcodes.
    Threshold,
    /// Floyd-Steinberg dithering -- best for photos and gradients.
    Dither,
}

/// Options controlling how an image is loaded and converted to bitmap.
#[derive(Debug, Clone)]
pub struct ImageLoadOptions {
    /// Binarization algorithm to use.
    pub binarize: BinarizeMode,
    /// If set, scale the image so its height matches this value (maintaining
    /// aspect ratio). Scaling is done in grayscale before binarization.
    pub target_height: Option<u32>,
    /// Auto-invert images with dark backgrounds so the minority colour
    /// becomes the foreground. Defaults to true.
    pub auto_invert: bool,
}

impl Default for ImageLoadOptions {
    fn default() -> Self {
        Self {
            binarize: BinarizeMode::default(),
            target_height: None,
            auto_invert: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Load an image from a file path and convert to a 1-bit [`LabelBitmap`].
///
/// SVG files are detected by extension and rendered via `resvg`. All other
/// formats are auto-detected from magic bytes via the `image` crate (PNG,
/// JPEG, GIF, BMP, TIFF, WebP, ICO, PNM, TGA, QOI, HDR, OpenEXR, etc.).
pub fn load_image(path: &Path, options: &ImageLoadOptions) -> Result<LabelBitmap> {
    let is_svg = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg") || ext.eq_ignore_ascii_case("svgz"));

    let img = if is_svg {
        load_svg_file(path, options.target_height)?
    } else {
        image::ImageReader::open(path)?
            .with_guessed_format()?
            .decode()?
    };
    Ok(convert_to_bitmap(img, options))
}

/// Load an image from a reader and convert to a 1-bit [`LabelBitmap`].
///
/// The reader contents are buffered for format detection. Raster magic is
/// checked first; XML and gzip data are parsed as SVG/SVGZ without requiring
/// the original path, so embedded layout images remain self-contained.
pub fn load_image_from_reader<R: Read>(
    mut reader: R,
    options: &ImageLoadOptions,
) -> Result<LabelBitmap> {
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    let img = if let Ok(format) = image::guess_format(&buf) {
        // A recognized but corrupt raster must retain its decoder error; do
        // not hide that error by retrying it as a different image format.
        image::ImageReader::with_format(std::io::Cursor::new(&buf), format).decode()?
    } else if buf.starts_with(&[0x1f, 0x8b])
        || buf
            .strip_prefix(&[0xef, 0xbb, 0xbf])
            .unwrap_or(&buf)
            .trim_ascii_start()
            .starts_with(b"<")
    {
        // Let the XML parser validate the root element, declarations and
        // comments; searching for an <svg substring could misclassify data.
        render_svg_data(&buf, options.target_height)?
    } else {
        image::ImageReader::new(std::io::Cursor::new(&buf))
            .with_guessed_format()?
            .decode()?
    };
    Ok(convert_to_bitmap(img, options))
}

/// Load an image with its source transparency preserved as binary coverage.
///
/// Alpha values below 128 are transparent; opaque white covers earlier ink.
/// Unlike the legacy bitmap loader, this opt-in path never auto-inverts dark
/// images. It uses the requested binarization while excluding hidden RGB from
/// the grayscale image, so transparent black cannot change the visible icon.
pub fn load_layer_from_reader<R: Read>(
    reader: R,
    options: &ImageLoadOptions,
) -> Result<crate::surface::RasterLayer> {
    const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;
    let mut bytes = Vec::new();
    reader.take(MAX_INPUT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(crate::RenderError::DimensionMismatch(
            "image source is too large".into(),
        ));
    }
    let image = if let Ok(format) = image::guess_format(&bytes) {
        // Keep recognized raster errors, as in the legacy dispatch.
        decode_layer_raster(&bytes, Some(format))?
    } else if bytes.starts_with(&[0x1f, 0x8b]) {
        let svg = decompress_layer_svgz(&bytes, MAX_INPUT_BYTES)?;
        render_svg_data_with_alpha(&svg, options.target_height, true)?
    } else if bytes
        .strip_prefix(&[0xef, 0xbb, 0xbf])
        .unwrap_or(&bytes)
        .trim_ascii_start()
        .starts_with(b"<")
    {
        render_svg_data_with_alpha(&bytes, options.target_height, true)?
    } else {
        decode_layer_raster(&bytes, None)?
    };
    convert_to_layer(image, options)
}

fn decompress_layer_svgz(bytes: &[u8], max_bytes: u64) -> Result<Vec<u8>> {
    let mut decoded = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut decoded)
        .map_err(|error| crate::RenderError::Text(format!("SVG parse error: {error}")))?;
    if decoded.len() as u64 > max_bytes {
        return Err(crate::RenderError::DimensionMismatch(
            "SVGZ source expands beyond the image source limit".into(),
        ));
    }
    Ok(decoded)
}

fn check_layer_size(width: u32, height: u32) -> Result<()> {
    if width == 0
        || height == 0
        || width > 1_000_000
        || height > 1_000_000
        || u64::from(width) * u64::from(height) > 16_777_216
    {
        return Err(crate::RenderError::DimensionMismatch(
            "image raster is too large".into(),
        ));
    }
    Ok(())
}

fn decode_layer_raster(bytes: &[u8], format: Option<image::ImageFormat>) -> Result<DynamicImage> {
    use image::ImageDecoder;

    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes));
    if let Some(format) = format {
        reader.set_format(format);
    } else {
        reader = reader.with_guessed_format()?;
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1_000_000);
    limits.max_image_height = Some(1_000_000);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    // Reject oversized rasters before allocating their decoded pixel buffer.
    check_layer_size(width, height)?;
    if decoder.total_bytes() > 128 * 1024 * 1024 {
        return Err(crate::RenderError::DimensionMismatch(
            "decoded image is too large".into(),
        ));
    }
    Ok(DynamicImage::from_decoder(decoder)?)
}

/// Load a PNG image from a file path (backward-compatible wrapper).
pub fn load_png(path: &Path) -> Result<LabelBitmap> {
    load_image(path, &ImageLoadOptions::default())
}

/// Load a PNG image from a reader (backward-compatible wrapper).
pub fn load_png_from_reader<R: Read>(reader: R) -> Result<LabelBitmap> {
    load_image_from_reader(reader, &ImageLoadOptions::default())
}

// ---------------------------------------------------------------------------
// SVG rendering
// ---------------------------------------------------------------------------

/// Render an SVG file to a `DynamicImage`.
///
/// If `target_height` is set, the SVG is scaled so its height matches that
/// value. Otherwise it is rendered at 180 DPI (matching Brother P-Touch
/// printer resolution).
fn load_svg_file(path: &Path, target_height: Option<u32>) -> Result<DynamicImage> {
    let svg_data = std::fs::read(path)?;
    render_svg_data(&svg_data, target_height)
}

/// Render SVG data bytes to a `DynamicImage`.
fn render_svg_data(data: &[u8], target_height: Option<u32>) -> Result<DynamicImage> {
    render_svg_data_with_alpha(data, target_height, false)
}

fn render_svg_data_with_alpha(
    data: &[u8],
    target_height: Option<u32>,
    preserve_alpha: bool,
) -> Result<DynamicImage> {
    use resvg::tiny_skia;
    use resvg::usvg;

    let mut opt = usvg::Options {
        dpi: 180.0,
        ..usvg::Options::default()
    };

    let fontdb = std::sync::Arc::make_mut(&mut opt.fontdb);
    fontdb.load_system_fonts();

    let tree = if preserve_alpha {
        // The alpha entry point already expanded SVGZ within its byte budget.
        // Parsing text directly prevents nested gzip from bypassing that cap.
        let text = std::str::from_utf8(data)
            .map_err(|error| crate::RenderError::Text(format!("SVG parse error: {error}")))?;
        usvg::Tree::from_str(text, &opt)
    } else {
        usvg::Tree::from_data(data, &opt)
    }
    .map_err(|e| crate::RenderError::Text(format!("SVG parse error: {}", e)))?;

    let svg_size = tree.size();
    // Preserve fractional SVG viewports when calculating the scale. Truncating
    // first can turn a valid subpixel viewport into an empty bitmap.
    let scale = target_height
        .filter(|height| *height > 0)
        .map_or(1.0, |height| {
            f64::from(height) / f64::from(svg_size.height())
        });
    let width = (f64::from(svg_size.width()) * scale).round().max(1.0);
    let height = (f64::from(svg_size.height()) * scale).round().max(1.0);
    // The RGBA pixmap uses four bytes per pixel, before grayscale conversion
    // and dithering buffers. Reject excessive output before allocating it.
    if !scale.is_finite()
        || scale > f64::from(f32::MAX)
        || !width.is_finite()
        || !height.is_finite()
        || width > 1_000_000.0
        || height > 1_000_000.0
        || width * height > 16_777_216.0
    {
        return Err(crate::RenderError::DimensionMismatch(
            "SVG raster is too large".into(),
        ));
    }
    let (w, h) = (width as u32, height as u32);
    let pixmap = tiny_skia::Pixmap::new(w, h).ok_or_else(|| {
        crate::RenderError::Text(format!("Failed to create {}x{} pixmap for SVG", w, h))
    })?;

    // The old path keeps its exact white-matte behavior. Alpha mode starts on
    // the pixmap's transparent background and preserves its source coverage.
    let mut pixmap = pixmap;
    if !preserve_alpha {
        pixmap.fill(tiny_skia::Color::WHITE);
    }

    let transform = tiny_skia::Transform::from_scale(scale as f32, scale as f32);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    let pixels = if preserve_alpha {
        // tiny-skia stores premultiplied RGBA, unlike DynamicImage. Without
        // demultiplication a partly transparent white stroke becomes dark.
        pixmap.take_demultiplied()
    } else {
        pixmap.take()
    };
    let rgba = image::RgbaImage::from_raw(w, h, pixels).ok_or_else(|| {
        crate::RenderError::Text("Failed to convert SVG pixmap to image".to_string())
    })?;

    Ok(DynamicImage::ImageRgba8(rgba))
}

// ---------------------------------------------------------------------------
// Conversion pipeline
// ---------------------------------------------------------------------------

fn convert_to_layer(
    img: DynamicImage,
    options: &ImageLoadOptions,
) -> Result<crate::surface::RasterLayer> {
    check_layer_size(img.width(), img.height())?;
    let mut pixels = img.into_luma_alpha8();
    if let Some(target_height) = options.target_height
        && target_height > 0
        && target_height != pixels.height()
    {
        let width = (f64::from(pixels.width()) * f64::from(target_height)
            / f64::from(pixels.height()))
        .round()
        .max(1.0);
        if width > f64::from(u32::MAX) {
            return Err(crate::RenderError::DimensionMismatch(
                "image raster is too large".into(),
            ));
        }
        check_layer_size(width as u32, target_height)?;
        // Resize the source color and coverage together. Nearest sampling
        // avoids mixing arbitrary hidden RGB into a visible icon edge.
        pixels = image::imageops::resize(&pixels, width as u32, target_height, FilterType::Nearest);
    }
    let (width, height) = pixels.dimensions();
    let mut gray = GrayImage::new(width, height);
    let mut coverage = LabelBitmap::new(width, height);
    for (x, y, pixel) in pixels.enumerate_pixels() {
        let visible = pixel.0[1] >= 128;
        gray.put_pixel(x, y, image::Luma([if visible { pixel.0[0] } else { 255 }]));
        if visible {
            coverage.set_pixel(x, y, true);
        }
    }
    let mode = match options.binarize {
        BinarizeMode::Auto => detect_binarize_mode(&gray),
        other => other,
    };
    let mut ink = match mode {
        BinarizeMode::Dither | BinarizeMode::Auto => floyd_steinberg_dither(&gray, false),
        BinarizeMode::Threshold => threshold_binarize(&gray, otsu_threshold(&gray), false),
    };
    for y in 0..height {
        for x in 0..width {
            if !coverage.get_pixel(x, y) {
                ink.set_pixel(x, y, false);
            }
        }
    }
    Ok(crate::surface::RasterLayer {
        ink,
        coverage: Some(coverage),
    })
}

/// Convert a `DynamicImage` to a 1-bit `LabelBitmap`.
///
/// Pipeline: grayscale -> optional scale -> auto-invert detect -> binarize.
fn convert_to_bitmap(img: DynamicImage, options: &ImageLoadOptions) -> LabelBitmap {
    let mut gray = img.to_luma8();
    let (w, h) = gray.dimensions();

    if w == 0 || h == 0 {
        return LabelBitmap::new(0, 0);
    }

    // Scale to target height if requested, preserving aspect ratio.
    if let Some(target_h) = options.target_height
        && target_h > 0
        && target_h != h
    {
        let scale = target_h as f64 / h as f64;
        let new_w = ((w as f64 * scale).round() as u32).max(1);
        gray = image::imageops::resize(&gray, new_w, target_h, FilterType::Lanczos3);
    }

    // Determine if the image should be inverted (dark background).
    let inverted = if options.auto_invert {
        let total: u64 = gray.pixels().map(|p| p.0[0] as u64).sum();
        let count = (gray.width() as u64) * (gray.height() as u64);
        let mean = (total / count) as u8;
        mean < 128
    } else {
        false
    };

    // Pick binarization mode.
    let mode = match options.binarize {
        BinarizeMode::Auto => detect_binarize_mode(&gray),
        other => other,
    };

    match mode {
        BinarizeMode::Dither | BinarizeMode::Auto => floyd_steinberg_dither(&gray, inverted),
        BinarizeMode::Threshold => {
            let thresh = otsu_threshold(&gray);
            threshold_binarize(&gray, thresh, inverted)
        }
    }
}

// ---------------------------------------------------------------------------
// Binarization algorithms
// ---------------------------------------------------------------------------

/// Compute the optimal threshold using Otsu's method.
///
/// Finds the threshold that maximizes between-class variance in a 256-bin
/// histogram.
fn otsu_threshold(gray: &GrayImage) -> u8 {
    let mut hist = [0u64; 256];
    for p in gray.pixels() {
        hist[p.0[0] as usize] += 1;
    }

    let total = (gray.width() as u64) * (gray.height() as u64);
    if total == 0 {
        return 128;
    }

    let mut sum_all: u64 = 0;
    for (i, &count) in hist.iter().enumerate() {
        sum_all += i as u64 * count;
    }

    let mut best_thresh: u8 = 0;
    let mut best_variance: f64 = 0.0;
    let mut w0: u64 = 0;
    let mut sum0: u64 = 0;

    for (t, &count) in hist.iter().enumerate() {
        w0 += count;
        if w0 == 0 {
            continue;
        }
        let w1 = total - w0;
        if w1 == 0 {
            break;
        }

        sum0 += t as u64 * count;
        let sum1 = sum_all - sum0;

        let mu0 = sum0 as f64 / w0 as f64;
        let mu1 = sum1 as f64 / w1 as f64;
        let diff = mu0 - mu1;
        let variance = w0 as f64 * w1 as f64 * diff * diff;

        if variance > best_variance {
            best_variance = variance;
            best_thresh = t as u8;
        }
    }

    best_thresh
}

/// Apply a fixed threshold to produce a bitmap.
fn threshold_binarize(gray: &GrayImage, thresh: u8, inverted: bool) -> LabelBitmap {
    let (w, h) = gray.dimensions();
    let mut bmp = LabelBitmap::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let luma = gray.get_pixel(x, y).0[0];
            let is_fg = if inverted {
                luma > thresh
            } else {
                luma <= thresh
            };
            if is_fg {
                bmp.set_pixel(x, y, true);
            }
        }
    }
    bmp
}

/// Floyd-Steinberg error-diffusion dithering.
///
/// Produces a bitmap where local dot density approximates the original
/// grayscale intensity -- ideal for photos and smooth gradients.
fn floyd_steinberg_dither(gray: &GrayImage, inverted: bool) -> LabelBitmap {
    let (w, h) = gray.dimensions();
    let w = w as usize;
    let h = h as usize;

    // Work buffer in f32 to accumulate fractional errors.
    let mut buf: Vec<f32> = gray.pixels().map(|p| p.0[0] as f32).collect();

    let mut bmp = LabelBitmap::new(w as u32, h as u32);

    for y in 0..h {
        for x in 0..w {
            let idx = y * w + x;
            let old = buf[idx].clamp(0.0, 255.0);
            let new = if old < 128.0 { 0.0 } else { 255.0 };
            let err = old - new;

            // Pixel is foreground if it quantized to black (0) for normal,
            // or to white (255) for inverted images.
            let is_fg = if inverted { new == 255.0 } else { new == 0.0 };
            if is_fg {
                bmp.set_pixel(x as u32, y as u32, true);
            }

            // Distribute error to neighbors.
            if x + 1 < w {
                buf[idx + 1] += err * (7.0 / 16.0);
            }
            if y + 1 < h {
                if x > 0 {
                    buf[(y + 1) * w + (x - 1)] += err * (3.0 / 16.0);
                }
                buf[(y + 1) * w + x] += err * (5.0 / 16.0);
                if x + 1 < w {
                    buf[(y + 1) * w + (x + 1)] += err * (1.0 / 16.0);
                }
            }
        }
    }

    bmp
}

/// Detect whether the image is better suited for threshold or dithering.
///
/// Builds a histogram and checks for bimodality: if there are two distinct
/// peaks with a deep valley between them, threshold works well. Otherwise
/// dithering preserves more detail.
fn detect_binarize_mode(gray: &GrayImage) -> BinarizeMode {
    let mut hist = [0u32; 256];
    for p in gray.pixels() {
        hist[p.0[0] as usize] += 1;
    }

    // Smooth histogram with a simple box filter to reduce noise.
    let mut smooth = [0u32; 256];
    for (i, slot) in smooth.iter_mut().enumerate() {
        let lo = i.saturating_sub(2);
        let hi = (i + 2).min(255);
        let window = &hist[lo..=hi];
        *slot = window.iter().sum::<u32>() / window.len() as u32;
    }

    // Find the two highest peaks (at least 30 bins apart).
    let mut peak1_idx = 0;
    let mut peak1_val = 0u32;
    for (i, &v) in smooth.iter().enumerate() {
        if v > peak1_val {
            peak1_val = v;
            peak1_idx = i;
        }
    }

    let mut peak2_idx = 0;
    let mut peak2_val = 0u32;
    for (i, &v) in smooth.iter().enumerate() {
        if (i as isize - peak1_idx as isize).unsigned_abs() >= 30 && v > peak2_val {
            peak2_val = v;
            peak2_idx = i;
        }
    }

    // No second peak found -> dither.
    if peak2_val == 0 {
        return BinarizeMode::Dither;
    }

    // Find the minimum valley between the two peaks.
    let (lo, hi) = if peak1_idx < peak2_idx {
        (peak1_idx, peak2_idx)
    } else {
        (peak2_idx, peak1_idx)
    };

    let valley_min = smooth[lo..=hi].iter().copied().min().unwrap_or(0);
    let smaller_peak = peak1_val.min(peak2_val);

    // If the valley is less than 40% of the smaller peak, the distribution
    // is clearly bimodal -> threshold works well.
    if smaller_peak > 0 && valley_min < smaller_peak * 2 / 5 {
        BinarizeMode::Threshold
    } else {
        BinarizeMode::Dither
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use image::{GrayImage, Luma};

    const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect x="4" y="2" width="4" height="4" fill="black"/></svg>"#;

    fn svgz() -> Vec<u8> {
        // gzip-compressed SVG above, with a fixed zero timestamp.
        base64::engine::general_purpose::STANDARD
            .decode("H4sIAAAAAAAC/0XMSw7CMAyE4atYc4DalK5QnLtAKUlEeKi1cLk96absRvo/TVg+idZHfS6KbPY+Mbt758fuNSfuRYSbAHm5Wlb0AspTSdkUB0EM8zQarYoB9G15h8PftXkrtSou9TzewTFsl/EH0/DhBHoAAAA=")
            .unwrap()
    }

    fn rgba_png(pixels: image::RgbaImage) -> Vec<u8> {
        let mut png = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(pixels)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        png.into_inner()
    }

    #[test]
    fn alpha_layer_distinguishes_hidden_rgb_opaque_white_and_binary_opacity() {
        let pixels = [
            [0, 0, 0, 0],
            [255, 255, 255, 0],
            [255, 255, 255, 255],
            [0, 0, 0, 127],
            [0, 0, 0, 128],
            [255, 255, 255, 128],
            [0, 0, 0, 255],
        ];
        let png = rgba_png(image::RgbaImage::from_fn(7, 1, |x, _| {
            image::Rgba(pixels[x as usize])
        }));
        for binarize in [
            BinarizeMode::Auto,
            BinarizeMode::Threshold,
            BinarizeMode::Dither,
        ] {
            let layer = load_layer_from_reader(
                png.as_slice(),
                &ImageLoadOptions {
                    binarize,
                    ..Default::default()
                },
            )
            .unwrap();
            let coverage = layer.coverage.unwrap();
            for (x, (covered, ink)) in [
                (false, false),
                (false, false),
                (true, false),
                (false, false),
                (true, true),
                (true, false),
                (true, true),
            ]
            .into_iter()
            .enumerate()
            {
                assert_eq!(
                    coverage.get_pixel(x as u32, 0),
                    covered,
                    "{binarize:?} x={x}"
                );
                assert_eq!(layer.ink.get_pixel(x as u32, 0), ink, "{binarize:?} x={x}");
            }
        }
    }

    #[test]
    fn alpha_layer_ignores_hidden_rgb_and_never_auto_inverts_black_icons() {
        let black = rgba_png(image::RgbaImage::from_pixel(
            4,
            4,
            image::Rgba([0, 0, 0, 255]),
        ));
        let layer = load_layer_from_reader(black.as_slice(), &ImageLoadOptions::default()).unwrap();
        assert!(layer.ink.get_pixel(1, 1));
        assert!(layer.coverage.unwrap().get_pixel(1, 1));

        for binarize in [
            BinarizeMode::Auto,
            BinarizeMode::Threshold,
            BinarizeMode::Dither,
        ] {
            let options = ImageLoadOptions {
                binarize,
                target_height: Some(20),
                ..Default::default()
            };
            let render = |hidden| {
                let image = image::RgbaImage::from_fn(9, 5, |x, y| {
                    if x < 4 {
                        image::Rgba([hidden, hidden, hidden, 0])
                    } else {
                        let luma = (x * 21 + y * 13) as u8;
                        image::Rgba([luma, luma, luma, 255])
                    }
                });
                load_layer_from_reader(rgba_png(image).as_slice(), &options).unwrap()
            };
            let black = render(0);
            let white = render(255);
            assert_eq!((black.ink.width(), black.ink.height()), (36, 20));
            assert_eq!(black.ink.data(), white.ink.data(), "{binarize:?}");
            assert_eq!(
                black.coverage.unwrap().data(),
                white.coverage.unwrap().data()
            );
        }
    }

    #[test]
    fn alpha_svg_and_svgz_preserve_transparent_pixels_and_demultiply_white() {
        let options = ImageLoadOptions::default();
        for bytes in [SVG.to_vec(), svgz()] {
            let layer = load_layer_from_reader(bytes.as_slice(), &options).unwrap();
            let coverage = layer.coverage.unwrap();
            assert!(!coverage.get_pixel(0, 0));
            assert!(!layer.ink.get_pixel(0, 0));
            assert!(coverage.get_pixel(5, 3));
            assert!(layer.ink.get_pixel(5, 3));
        }
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="2"><rect width="2" height="2" fill="white" opacity="0.5"/><rect x="2" width="2" height="2" fill="black" opacity="0.5"/><rect x="4" width="2" height="2" fill="black" opacity="0.49"/></svg>"#;
        for binarize in [
            BinarizeMode::Auto,
            BinarizeMode::Threshold,
            BinarizeMode::Dither,
        ] {
            let layer = load_layer_from_reader(
                svg.as_slice(),
                &ImageLoadOptions {
                    binarize,
                    ..Default::default()
                },
            )
            .unwrap();
            let coverage = layer.coverage.unwrap();
            assert!(coverage.get_pixel(0, 0));
            assert!(
                !layer.ink.get_pixel(0, 0),
                "white must be demultiplied: {binarize:?}"
            );
            assert!(coverage.get_pixel(2, 0));
            assert!(layer.ink.get_pixel(2, 0));
            assert!(!coverage.get_pixel(4, 0));
            assert!(!coverage.get_pixel(6, 0));
        }
    }

    #[test]
    fn alpha_svgz_expansion_is_bounded_and_nested_gzip_is_not_reparsed() {
        use std::io::Write;

        let gzip = |bytes: &[u8]| {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap()
        };
        let compressed = gzip(&[b' '; 1024]);
        assert!(compressed.len() < 128);
        assert!(matches!(
            decompress_layer_svgz(&compressed, 128),
            Err(crate::RenderError::DimensionMismatch(_))
        ));
        assert_eq!(
            decompress_layer_svgz(&compressed, 1024).unwrap().len(),
            1024
        );
        assert!(matches!(
            load_layer_from_reader(gzip(&gzip(SVG)).as_slice(), &ImageLoadOptions::default()),
            Err(crate::RenderError::Text(message)) if message.starts_with("SVG parse error:")
        ));
    }

    #[test]
    fn alpha_loader_keeps_decoder_errors_and_checks_source_and_scaled_raster_sizes() {
        let options = ImageLoadOptions::default();
        assert!(matches!(
            load_layer_from_reader(b"\x89PNG\r\n\x1a\n<svg>".as_slice(), &options),
            Err(crate::RenderError::Image(_))
        ));
        for data in [b"<svg".as_slice(), b"<not-svg/>", &[0x1f, 0x8b, 0]] {
            assert!(matches!(
                load_layer_from_reader(data, &options),
                Err(crate::RenderError::Text(message)) if message.starts_with("SVG parse error:")
            ));
        }
        let huge_svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="50000" height="50000"/>"#;
        assert!(matches!(
            load_layer_from_reader(huge_svg.as_slice(), &options),
            Err(crate::RenderError::DimensionMismatch(_))
        ));
        assert!(matches!(
            load_layer_from_reader(b"P5\n20000 20000\n255\n".as_slice(), &options),
            Err(crate::RenderError::DimensionMismatch(_))
        ));
        assert!(matches!(
            load_layer_from_reader(
                SVG,
                &ImageLoadOptions {
                    target_height: Some(u32::MAX),
                    ..Default::default()
                }
            ),
            Err(crate::RenderError::DimensionMismatch(_))
        ));
        let png = rgba_png(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([0, 0, 0, 255]),
        ));
        assert!(matches!(
            load_layer_from_reader(
                png.as_slice(),
                &ImageLoadOptions {
                    target_height: Some(u32::MAX),
                    ..Default::default()
                }
            ),
            Err(crate::RenderError::DimensionMismatch(_))
        ));
        assert!(matches!(
            load_layer_from_reader(std::io::repeat(0).take(64 * 1024 * 1024 + 1), &options),
            Err(crate::RenderError::DimensionMismatch(_))
        ));
    }

    #[test]
    fn reader_detects_svg_xml_prefixes_and_svgz_without_a_path() {
        let mut prefixed =
            b"\xef\xbb\xbf<?xml version=\"1.0\"?>\n<!-- embedded logo -->\n".to_vec();
        prefixed.extend_from_slice(SVG);
        let options = ImageLoadOptions {
            target_height: Some(20),
            ..Default::default()
        };
        for bytes in [SVG.to_vec(), prefixed, svgz()] {
            let bitmap = load_image_from_reader(bytes.as_slice(), &options).unwrap();
            assert_eq!((bitmap.width(), bitmap.height()), (40, 20));
            assert!(bitmap.get_pixel(10, 8));
            assert!(!bitmap.get_pixel(0, 0));
        }
    }

    #[test]
    fn corrupt_raster_keeps_decoder_error_and_invalid_svg_is_rejected() {
        let options = ImageLoadOptions::default();
        let truncated_png = b"\x89PNG\r\n\x1a\n<svg>";
        assert!(matches!(
            load_image_from_reader(truncated_png.as_slice(), &options),
            Err(crate::RenderError::Image(_))
        ));
        for data in [b"<svg".as_slice(), b"<not-svg/>", &[0x1f, 0x8b, 0]] {
            assert!(matches!(
                load_image_from_reader(data, &options),
                Err(crate::RenderError::Text(message)) if message.starts_with("SVG parse error:")
            ));
        }
    }

    #[test]
    fn embedded_png_svg_and_svgz_survive_legacy_and_object_layout_roundtrips() {
        use crate::document::{LabelDocument, LabelElement, LabelObject, TextBox, render_document};
        use crate::text::TextRenderer;

        let mut pixels = GrayImage::from_pixel(20, 10, Luma([255]));
        for y in 2..6 {
            for x in 4..8 {
                pixels.put_pixel(x, y, Luma([0]));
            }
        }
        let mut png = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageLuma8(pixels)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let mut renderer = TextRenderer::new();
        for bytes in [png.into_inner(), SVG.to_vec(), svgz()] {
            for version in [1, 3] {
                let element = if version == 1 {
                    LabelElement::image_from_bytes(None, bytes.clone())
                } else {
                    LabelElement::Object(LabelObject::image_from_bytes(
                        None,
                        bytes.clone(),
                        TextBox {
                            width_mm: 8.0,
                            height_mm: 4.0,
                            ..Default::default()
                        },
                    ))
                };
                let document = LabelDocument {
                    version,
                    tape_width_mm: 12,
                    dpi: 180,
                    font_name: String::new(),
                    font_margin: 0,
                    flip_h: false,
                    flip_v: false,
                    elements: vec![element],
                };
                let saved = document.to_toml_string().unwrap();
                let reopened = LabelDocument::from_toml_str(&saved).unwrap();
                assert_eq!(reopened.to_toml_string().unwrap(), saved);
                let before = render_document(&document, 76, 180, &mut renderer)
                    .unwrap()
                    .bitmap
                    .unwrap();
                let after = render_document(&reopened, 76, 180, &mut renderer)
                    .unwrap()
                    .bitmap
                    .unwrap();
                assert_eq!(
                    (before.width(), before.height()),
                    (after.width(), after.height())
                );
                assert_eq!(before.data(), after.data());
                assert!(after.data().iter().any(|byte| *byte != 0));
            }
        }
    }

    #[test]
    fn svg_raster_limits_apply_after_scaling_and_preserve_fractional_viewports() {
        let huge = br#"<svg xmlns="http://www.w3.org/2000/svg" width="50000" height="50000"/>"#;
        assert!(matches!(
            render_svg_data(huge, None),
            Err(crate::RenderError::DimensionMismatch(_))
        ));
        assert_eq!(render_svg_data(huge, Some(20)).unwrap().width(), 20);
        assert!(matches!(
            render_svg_data(SVG, Some(u32::MAX)),
            Err(crate::RenderError::DimensionMismatch(_))
        ));
        let fractional = br#"<svg xmlns="http://www.w3.org/2000/svg" width="0.5" height="0.25"><rect width="0.5" height="0.25"/></svg>"#;
        let bitmap = load_image_from_reader(
            fractional.as_slice(),
            &ImageLoadOptions {
                target_height: Some(10),
                auto_invert: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!((bitmap.width(), bitmap.height()), (20, 10));
        assert!(bitmap.get_pixel(10, 5));
    }

    #[test]
    fn test_convert_bright_background() {
        let mut img = GrayImage::from_pixel(10, 10, Luma([255u8]));
        for y in 2..5 {
            for x in 2..5 {
                img.put_pixel(x, y, Luma([0u8]));
            }
        }
        let dyn_img = DynamicImage::ImageLuma8(img);
        let bmp = convert_to_bitmap(dyn_img, &ImageLoadOptions::default());
        assert_eq!(bmp.width(), 10);
        assert_eq!(bmp.height(), 10);
        assert!(bmp.get_pixel(2, 2));
        assert!(!bmp.get_pixel(0, 0));
    }

    #[test]
    fn test_convert_dark_background() {
        let mut img = GrayImage::from_pixel(10, 10, Luma([0u8]));
        for y in 2..5 {
            for x in 2..5 {
                img.put_pixel(x, y, Luma([255u8]));
            }
        }
        let dyn_img = DynamicImage::ImageLuma8(img);
        let bmp = convert_to_bitmap(dyn_img, &ImageLoadOptions::default());
        assert!(bmp.get_pixel(2, 2));
        assert!(!bmp.get_pixel(0, 0));
    }

    #[test]
    fn test_load_png_from_reader() {
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 0, 255]));
        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let encoder = image::codecs::png::PngEncoder::new(cursor);
            image::ImageEncoder::write_image(
                encoder,
                img.as_raw(),
                4,
                4,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        }
        let result = load_png_from_reader(std::io::Cursor::new(&buf));
        assert!(result.is_ok());
    }

    #[test]
    fn test_otsu_bimodal() {
        // Two groups with spread: dark (20-80) and bright (180-240).
        // With spread, Otsu has a unique maximum between the groups.
        let mut img = GrayImage::new(200, 100);
        for y in 0..100 {
            for x in 0..200 {
                let val = if y < 50 {
                    20 + (x * 60 / 199) as u8 // 20..80
                } else {
                    180 + (x * 60 / 199) as u8 // 180..240
                };
                img.put_pixel(x, y, Luma([val]));
            }
        }
        let thresh = otsu_threshold(&img);
        assert!(
            (80..=180).contains(&thresh),
            "Expected threshold between 80 and 180, got {}",
            thresh
        );
    }

    #[test]
    fn test_otsu_skewed() {
        // 10% dark foreground (10-50) on bright background (190-230).
        let mut img = GrayImage::new(100, 100);
        for y in 0..100 {
            for x in 0..100 {
                let val = if y < 10 {
                    10 + (x * 40 / 99) as u8 // 10..50
                } else {
                    190 + (x * 40 / 99) as u8 // 190..230
                };
                img.put_pixel(x, y, Luma([val]));
            }
        }
        let thresh = otsu_threshold(&img);
        // Threshold should split between the dark and bright groups.
        assert!(
            (50..=190).contains(&thresh),
            "Expected threshold between 50 and 190, got {}",
            thresh
        );
    }

    #[test]
    fn test_dither_density() {
        // Uniform gray(128) should produce roughly 50% black pixels.
        let img = GrayImage::from_pixel(100, 100, Luma([128u8]));
        let bmp = floyd_steinberg_dither(&img, false);
        let total = (bmp.width() * bmp.height()) as usize;
        let black: usize = (0..bmp.height())
            .flat_map(|y| (0..bmp.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| bmp.get_pixel(x, y))
            .count();
        let ratio = black as f64 / total as f64;
        assert!(
            (0.40..=0.60).contains(&ratio),
            "Expected ~50% black pixels, got {:.1}%",
            ratio * 100.0
        );
    }

    #[test]
    fn test_dither_black() {
        let img = GrayImage::from_pixel(20, 20, Luma([0u8]));
        let bmp = floyd_steinberg_dither(&img, false);
        for y in 0..20 {
            for x in 0..20 {
                assert!(bmp.get_pixel(x, y), "Pixel ({},{}) should be black", x, y);
            }
        }
    }

    #[test]
    fn test_dither_white() {
        let img = GrayImage::from_pixel(20, 20, Luma([255u8]));
        let bmp = floyd_steinberg_dither(&img, false);
        for y in 0..20 {
            for x in 0..20 {
                assert!(!bmp.get_pixel(x, y), "Pixel ({},{}) should be white", x, y);
            }
        }
    }

    #[test]
    fn test_load_with_scaling() {
        // Create a 100x100 PNG in memory, load with target_height=50.
        let img = image::RgbaImage::from_pixel(100, 100, image::Rgba([128, 128, 128, 255]));
        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let encoder = image::codecs::png::PngEncoder::new(cursor);
            image::ImageEncoder::write_image(
                encoder,
                img.as_raw(),
                100,
                100,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        }
        let options = ImageLoadOptions {
            target_height: Some(50),
            ..ImageLoadOptions::default()
        };
        let bmp = load_image_from_reader(std::io::Cursor::new(&buf), &options).unwrap();
        assert_eq!(bmp.height(), 50);
        assert_eq!(bmp.width(), 50);
    }

    #[test]
    fn test_render_svg() {
        // Minimal SVG: small black rectangle on white background.
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
            <rect x="30" y="30" width="40" height="40" fill="black"/>
        </svg>"#;
        let img = render_svg_data(svg, Some(50)).unwrap();
        let bmp = convert_to_bitmap(img, &ImageLoadOptions::default());
        assert_eq!(bmp.height(), 50);
        assert_eq!(bmp.width(), 50);
        // Center should be black (the rectangle).
        assert!(bmp.get_pixel(25, 25));
        // Corner should be white (outside the rectangle).
        assert!(!bmp.get_pixel(0, 0));
    }

    #[test]
    fn test_auto_invert() {
        // Dark background with bright foreground -- auto_invert should
        // mark the bright pixels as foreground.
        let mut img = GrayImage::from_pixel(20, 20, Luma([10u8]));
        for y in 5..15 {
            for x in 5..15 {
                img.put_pixel(x, y, Luma([245u8]));
            }
        }
        let dyn_img = DynamicImage::ImageLuma8(img);
        let bmp = convert_to_bitmap(dyn_img, &ImageLoadOptions::default());
        // Bright square on dark bg -> bright pixels are foreground.
        assert!(bmp.get_pixel(7, 7));
        // Dark background pixel should not be foreground.
        assert!(!bmp.get_pixel(0, 0));
    }
}
