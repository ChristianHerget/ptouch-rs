// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Top toolbar panel with element addition and action buttons.

use std::path::PathBuf;

use log::{error, info};

use ptouch_render::document::LabelDocument;
use ptouch_render::raster;
use ptouch_render::text::TextAlign;

use crate::state::{AppState, LabelElement, PrinterCommand};

/// Render the top toolbar.
pub fn show_toolbar(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        // -- Element addition buttons --
        if ui.button("Add Text").clicked() {
            state.apply_editor_action(crate::editor::EditorAction::Add(LabelElement::Text {
                content: "Label".to_string(),
                font_size: None,
                align: TextAlign::Left,
                rotation: 0.0,
                flip_h: false,
                flip_v: false,
            }));
            info!("Added text element");
        }

        if ui.button("Add Image").clicked()
            && let Some(path) = crate::widgets::image_file_dialog().pick_file()
        {
            // Read the original source bytes so the image is embedded in the
            // label and stays self-contained when saved to a layout file.
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let element = LabelElement::image_from_bytes(Some(path.clone()), bytes);
                    // Reject files that do not decode, so a saved layout can
                    // always be reopened.
                    if matches!(element, LabelElement::Image { bitmap: None, .. }) {
                        error!("Unsupported or corrupt image: {}", path.display());
                        state.status_message = format!("Image load error: {}", path.display());
                    } else {
                        info!("Loaded image: {}", path.display());
                        state.apply_editor_action(crate::editor::EditorAction::Add(element));
                    }
                }
                Err(e) => {
                    error!("Failed to read image: {}", e);
                    state.status_message = format!("Image read error: {}", e);
                }
            }
        }

        if ui.button("Cut Mark").clicked() {
            state.apply_editor_action(crate::editor::EditorAction::Add(LabelElement::CutMark));
            info!("Added cut mark");
        }

        if ui.button("Padding").clicked() {
            state.apply_editor_action(crate::editor::EditorAction::Add(LabelElement::Padding {
                pixels: 20,
            }));
            info!("Added padding element");
        }

        ui.separator();

        // -- Action buttons --
        let connected = state.printer_connected;
        let busy = state.is_printer_busy();
        let has_bitmap = state.output_preview().is_some();

        if ui
            .add_enabled(connected && !busy && has_bitmap, egui::Button::new("Print"))
            .clicked()
        {
            do_print(state);
        }

        if ui
            .add_enabled(
                connected
                    && !busy
                    && !state.printer_target.is_bluetooth()
                    && !state.printer_target.is_network(),
                egui::Button::new("Feed & Cut"),
            )
            .clicked()
            && let Some(ref tx) = state.printer_cmd_tx
        {
            let cancellation = ptouch_core::CancellationToken::default();
            state.cancellation = Some(cancellation.clone());
            let _ = tx.send(PrinterCommand::FeedAndCut {
                target: state.selection(),
                cancellation,
            });
            state.operation_in_progress = true;
            state.status_message = "Feeding & cutting...".to_string();
        }

        if state.operation_in_progress
            && !state.printer_target.is_bluetooth()
            && ui.button("Cancel").clicked()
            && let Some(token) = &state.cancellation
        {
            token.cancel();
            state.status_message = "Cancelling; sent data may still print".into();
        }

        if ui
            .add_enabled(has_bitmap, egui::Button::new("Export Image"))
            .clicked()
        {
            do_export_image(state);
        }

        ui.separator();

        if ui.button("Save Layout").clicked() {
            do_save_layout(state);
        }

        if ui.button("Open Layout").clicked() {
            do_open_layout(state);
        }
    });
}

/// Submit exactly the pixels shown by the current preview.
fn do_print(state: &mut AppState) {
    if !state.printer_connected || state.is_printer_busy() || state.printer_quality.is_none() {
        return;
    }
    let Some(preview) = state.output_preview() else {
        return;
    };
    let resolution = preview.resolution;
    let raster_lines = if let Some(left) = state.band_left {
        raster::bitmap_to_raster_lines_at(&preview.bitmap, state.printer_max_px, left)
    } else {
        raster::bitmap_to_raster_lines(&preview.bitmap, state.printer_max_px)
    };
    if let Some(ref tx) = state.printer_cmd_tx {
        let cancellation = ptouch_core::CancellationToken::default();
        state.cancellation = Some(cancellation.clone());
        let _ = tx.send(PrinterCommand::Print {
            raster_lines,
            resolution,
            expected_media: (state.tape_width_mm, state.media_code),
            job_timeout_secs: state.job_timeout_secs,
            chain_print: !state.auto_cut,
            auto_cut: state.auto_cut,
            precut: state.precut,
            quality: state.print_quality,
            target: state.selection(),
            cancellation,
        });
        state.operation_in_progress = true;
        state.status_message = "Printing...".to_string();
    }
}

/// Save the current design to a `.ptl` layout file (TOML with embedded images).
fn do_save_layout(state: &mut AppState) {
    if state.elements.is_empty() {
        state.status_message = "Nothing to save".to_string();
        return;
    }

    let document = LabelDocument {
        version: ptouch_render::document::DOCUMENT_VERSION,
        tape_width_mm: state.tape_width_mm,
        dpi: state.printer_dpi,
        font_name: state.font_name.clone(),
        font_margin: state.font_margin,
        flip_h: state.overall_flip_h,
        flip_v: state.overall_flip_v,
        elements: state.elements.clone(),
    };

    let text = match document.to_toml_string() {
        Ok(text) => text,
        Err(e) => {
            state.status_message = format!("Save error: {}", e);
            error!("Layout serialize error: {}", e);
            return;
        }
    };

    if let Some(path) = crate::widgets::layout_file_dialog()
        .set_file_name("label.ptl")
        .save_file()
    {
        let save_path: PathBuf = if path.extension().is_none() {
            path.with_extension("ptl")
        } else {
            path
        };
        match std::fs::write(&save_path, text) {
            Ok(()) => {
                state.status_message = format!("Saved to {}", save_path.display());
                info!("Saved layout: {}", save_path.display());
            }
            Err(e) => {
                state.status_message = format!("Save error: {}", e);
                error!("Layout write error: {}", e);
            }
        }
    }
}

/// Open a `.ptl` layout file, replacing the current design.
fn do_open_layout(state: &mut AppState) {
    let Some(path) = crate::widgets::layout_file_dialog().pick_file() else {
        return;
    };

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            state.status_message = format!("Open error: {}", e);
            error!("Layout read error: {}", e);
            return;
        }
    };

    match LabelDocument::from_toml_str(&text) {
        Ok(document) => {
            apply_layout(state, document);
            state.status_message = format!("Opened {}", path.display());
            info!("Opened layout: {}", path.display());
        }
        Err(e) => {
            state.status_message = format!("Open error: {}", e);
            error!("Layout parse error: {}", e);
        }
    }
}

pub(crate) fn apply_layout(state: &mut AppState, document: LabelDocument) {
    state.editor = crate::editor::Editor::default();
    if !state.printer_connected {
        state.tape_width_mm = document.tape_width_mm;
        state.printer_dpi = document.dpi;
        state.printer_max_px = 0;
        state.band_left = None;
        state.update_tape_pixels();
    }
    state.font_name = document.font_name;
    state.font_margin = document.font_margin;
    state.overall_flip_h = document.flip_h;
    state.overall_flip_v = document.flip_v;
    state.elements = document.elements;
    state.select_element(None);
    state.mark_dirty();
}

/// Export the current label preview as an image file.
fn do_export_image(state: &mut AppState) {
    let preview = match state.output_preview() {
        Some(preview) => preview,
        None => {
            state.status_message = "Nothing to export".to_string();
            return;
        }
    };

    if let Some(path) = rfd::FileDialog::new()
        .add_filter("PNG", &["png"])
        .add_filter("JPEG", &["jpg", "jpeg"])
        .add_filter("BMP", &["bmp"])
        .add_filter("GIF", &["gif"])
        .add_filter("TIFF", &["tiff", "tif"])
        .add_filter("WebP", &["webp"])
        .set_file_name("label.png")
        .save_file()
    {
        let save_path: PathBuf = if path.extension().is_none() {
            path.with_extension("png")
        } else {
            path
        };
        match preview.square_pixel_bitmap().save(&save_path) {
            Ok(()) => {
                state.status_message = format!("Saved to {}", save_path.display());
                info!("Exported image: {}", save_path.display());
            }
            Err(e) => {
                state.status_message = format!("Save error: {}", e);
                error!("Image save error: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PrinterTarget;
    use ptouch_render::text::TextRenderer;

    #[test]
    fn print_submits_cached_pixels_and_rejects_stale_previews() {
        use crate::preview::RenderedPreview;
        use ptouch_core::{PrintQuality, PrintResolution, quality::QualityProfile};
        use ptouch_render::bitmap::LabelBitmap;

        for (name, head) in [("PT-P750W", 180), ("PT-9500PC", 360), ("PT-9700PC", 360)] {
            let profile = QualityProfile::for_model(name, head);
            for mode in profile.modes {
                for left in [None, Some(3)] {
                    let (tx, rx) = std::sync::mpsc::channel();
                    // No elements: a fresh render could not produce these dots.
                    let mut bitmap = LabelBitmap::new(17, 9);
                    bitmap.set_pixel(0, 0, true);
                    bitmap.set_pixel(8, 4, true);
                    bitmap.set_pixel(16, 8, true);
                    let expected = match left {
                        Some(left) => raster::bitmap_to_raster_lines_at(&bitmap, 32, left),
                        None => raster::bitmap_to_raster_lines(&bitmap, 32),
                    };
                    let mut state = AppState {
                        printer_connected: true,
                        printer_max_px: 32,
                        printer_dpi: head,
                        printer_quality: Some(profile),
                        print_quality: mode.quality,
                        band_left: left,
                        needs_rerender: false,
                        preview: Some(RenderedPreview::new(
                            &egui::Context::default(),
                            bitmap,
                            mode.resolution,
                        )),
                        printer_cmd_tx: Some(tx),
                        ..Default::default()
                    };
                    state.mark_dirty();
                    assert!(state.output_preview().is_none());
                    do_print(&mut state);
                    assert!(rx.try_recv().is_err());
                    state.needs_rerender = false;
                    state.preview.as_mut().unwrap().resolution = PrintResolution::square(1);
                    assert!(state.output_preview().is_none());
                    do_print(&mut state);
                    assert!(rx.try_recv().is_err());
                    state.preview.as_mut().unwrap().resolution = mode.resolution;
                    do_print(&mut state);
                    match rx.try_recv().unwrap() {
                        PrinterCommand::Print {
                            raster_lines,
                            resolution,
                            quality,
                            ..
                        } => {
                            assert_eq!(raster_lines, expected);
                            assert_eq!(resolution, mode.resolution);
                            assert_eq!(quality, mode.quality);
                        }
                        _ => panic!("expected print command"),
                    }
                    assert!(state.operation_in_progress);
                }
            }
        }
        // Offline resolution remains square at the design DPI, independent
        // of any stale preferred quality before a printer is identified.
        let state = AppState {
            printer_dpi: 360,
            print_quality: PrintQuality::HighRes,
            ..Default::default()
        };
        assert_eq!(
            state.preview_resolution().unwrap(),
            PrintResolution::square(360)
        );
    }

    fn layout(tape_width_mm: u8) -> LabelDocument {
        let document = LabelDocument {
            version: ptouch_render::document::DOCUMENT_VERSION,
            tape_width_mm,
            dpi: 180,
            font_name: "DejaVuSans".into(),
            font_margin: 0,
            flip_h: false,
            flip_v: false,
            elements: vec![LabelElement::CutMark],
        };
        LabelDocument::from_toml_str(&document.to_toml_string().unwrap()).unwrap()
    }

    #[test]
    fn opening_layout_preserves_network_band_before_next_poll() {
        let mut state = AppState {
            printer_connected: true,
            printer_max_px: 128,
            printer_dpi: 180,
            tape_width_mm: 12,
            tape_width_px: 70,
            media_code: 1,
            band_left: Some(29),
            ..AppState::default()
        };
        for width in [12, 24] {
            apply_layout(&mut state, layout(width));
            assert_eq!(
                (
                    state.tape_width_mm,
                    state.tape_width_px,
                    state.band_left,
                    state.media_code
                ),
                (12, 70, Some(29), 1)
            );
            assert!(state.needs_rerender);
            let bitmap = ptouch_render::document::render_elements(
                &state.elements,
                state.tape_width_px,
                &state.font_name,
                0,
                &mut TextRenderer::new(),
            )
            .unwrap()
            .unwrap();
            let lines = raster::bitmap_to_raster_lines_at(&bitmap, 128, 29);
            for line in lines {
                for pin in (0..29).chain(99..128) {
                    assert_eq!(line[pin / 8] & (0x80 >> (pin % 8)), 0);
                }
            }
        }
    }

    #[test]
    fn opening_layout_keeps_bluetooth_tape_geometry() {
        for width in [12, 24] {
            let mut state = AppState {
                printer_target: PrinterTarget::Bluetooth {
                    name: "PT-P300BT".into(),
                    address: "AA:BB:CC:DD:EE:FF".into(),
                },
                printer_connected: true,
                printer_max_px: 128,
                tape_width_mm: 12,
                tape_width_px: 64,
                ..AppState::default()
            };
            apply_layout(&mut state, layout(width));
            assert_eq!((state.tape_width_mm, state.tape_width_px), (12, 64));
            let bitmap = ptouch_render::document::render_elements(
                &state.elements,
                state.tape_width_px,
                &state.font_name,
                state.font_margin,
                &mut TextRenderer::new(),
            )
            .unwrap()
            .unwrap();
            let lines = raster::bitmap_to_raster_lines(&bitmap, state.printer_max_px);
            assert!(
                lines
                    .iter()
                    .all(|line| line[..4].iter().chain(&line[12..]).all(|&byte| byte == 0))
            );
        }
    }

    #[test]
    fn opening_usb_layout_uses_saved_tape_width() {
        let mut state = AppState {
            tape_width_mm: 24,
            tape_width_px: 128,
            ..AppState::default()
        };
        apply_layout(&mut state, layout(12));
        assert_eq!((state.tape_width_mm, state.tape_width_px), (12, 76));
    }
}
