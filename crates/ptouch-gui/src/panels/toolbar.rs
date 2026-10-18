// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Top toolbar panel with element addition and action buttons.

use std::path::PathBuf;

use log::{error, info};

use ptouch_render::document::LabelDocument;
use ptouch_render::raster;

use crate::state::{AppState, LabelElement, PrinterCommand};

/// Actions run after properties commit pending keyboard edits.
#[derive(Clone, Copy)]
pub enum ToolbarAction {
    Convert,
    Info,
    Print,
    Feed,
    Cancel,
    Export,
    ExportCorrected,
    Save,
    Open,
}

/// Collect document actions while rendering the top toolbar.
pub fn show_toolbar(ui: &mut egui::Ui, state: &mut AppState) -> Option<ToolbarAction> {
    let mut action = None;
    if !state.is_free_layout() {
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("Legacy objects (format v{})", state.document_version));
            if ui.button("Convert to free layout")
                .on_hover_text("Fix every object's current position in millimeters. Natural font sizing and rotated image fitting may change; review the preview before saving.")
                .clicked()
            {
                action = Some(ToolbarAction::Convert);
            }
            ui.small("Legacy spacing stays unchanged until converted.");
        });
    }
    ui.horizontal_wrapped(|ui| {
        crate::editor::history_buttons(ui, state);
        ui.separator();

        // -- Action buttons --
        let connected = state.printer_connected;
        let busy = state.is_printer_busy();
        let has_bitmap = state.output_preview().is_some();

        if ui
            .add_enabled(connected && !busy && has_bitmap, egui::Button::new("Print"))
            .clicked()
        {
            action = Some(ToolbarAction::Print);
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
        {
            action = Some(ToolbarAction::Feed);
        }

        if state.operation_in_progress
            && !state.printer_target.is_bluetooth()
            && ui.button("Cancel").clicked()
        {
            action = Some(ToolbarAction::Cancel);
        }

        if ui
            .add_enabled(has_bitmap, egui::Button::new("Export Image"))
            .clicked()
        {
            action = Some(ToolbarAction::Export);
        }

        if ui
            .add_enabled(has_bitmap, egui::Button::new("Export corrected image"))
            .clicked()
        {
            action = Some(ToolbarAction::ExportCorrected);
        }

        ui.separator();

        if ui.button("Save Layout").clicked() {
            action = Some(ToolbarAction::Save);
        }

        if ui.button("Open Layout").clicked() {
            action = Some(ToolbarAction::Open);
        }
        if ui.button("Info").clicked() {
            action = Some(ToolbarAction::Info);
        }
    });
    action
}

pub fn execute_action(state: &mut AppState, action: ToolbarAction) {
    match action {
        ToolbarAction::Info => state.info_open = true,
        ToolbarAction::Convert => match state.convert_to_free_layout() {
            Ok(()) => {
                state.status_message =
                    "Converted to free layout. Review text sizing and rotated images before saving."
                        .into()
            }
            Err(error) => state.status_message = format!("Conversion error: {error}"),
        },
        ToolbarAction::Print => do_print(state),
        ToolbarAction::Feed => {
            if !state.printer_connected
                || state.is_printer_busy()
                || state.printer_target.is_bluetooth()
                || state.printer_target.is_network()
            {
                return;
            }
            if let Some(tx) = &state.printer_cmd_tx {
                let cancellation = ptouch_core::CancellationToken::default();
                state.cancellation = Some(cancellation.clone());
                let _ = tx.send(PrinterCommand::FeedAndCut {
                    target: state.selection(),
                    cancellation,
                });
                state.operation_in_progress = true;
                state.status_message = "Feeding & cutting...".into();
            }
        }
        ToolbarAction::Cancel => {
            if let Some(token) = &state.cancellation {
                token.cancel();
                state.status_message = "Cancelling; sent data may still print".into();
            }
        }
        ToolbarAction::Export => do_export_image(state, false),
        ToolbarAction::ExportCorrected => do_export_image(state, true),
        ToolbarAction::Save => do_save_layout(state),
        ToolbarAction::Open => do_open_layout(state),
    }
}

/// Creation controls live directly below the object list.
pub fn show_add_controls(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal_wrapped(|ui| {
        if ui.button("Add Text").clicked() {
            state.add_text();
            info!("Added text element");
        }

        if ui.button("Add Image").clicked()
            && let Some(path) = crate::widgets::image_file_dialog().pick_file()
        {
            // Read the original source bytes so the image is embedded in the
            // label and stays self-contained when saved to a layout file.
            match std::fs::read(&path) {
                Ok(bytes) => {
                    if state.add_image_bytes(Some(path.clone()), bytes) {
                        info!("Loaded image: {}", path.display());
                    } else {
                        error!("Unsupported or corrupt image: {}", path.display());
                        state.status_message = format!("Image load error: {}", path.display());
                    }
                }
                Err(e) => {
                    error!("Failed to read image: {}", e);
                    state.status_message = format!("Image read error: {}", e);
                }
            }
        }

        if ui
            .add_enabled(state.is_free_layout(), egui::Button::new("Add Line"))
            .on_hover_text("Add a printed line. Convert legacy layouts to free layout first.")
            .clicked()
        {
            state.add_line();
        }
        ui.add_enabled_ui(state.is_free_layout(), |ui| {
            ui.menu_button("Add Shape", |ui| {
                for (label, kind, circle) in [
                    (
                        "Rectangle",
                        ptouch_render::shape::ShapeKind::Rectangle,
                        false,
                    ),
                    ("Ellipse", ptouch_render::shape::ShapeKind::Ellipse, false),
                    ("Circle", ptouch_render::shape::ShapeKind::Ellipse, true),
                ] {
                    if ui.button(label).clicked() {
                        state.add_shape(kind, circle);
                        ui.close();
                    }
                }
            });
        });
        if ui.button("Add Symbol").clicked() {
            state.open_symbol_picker(None);
        }

        if ui.button("Cut Mark").clicked() {
            state.add_cut_mark();
            info!("Added cut mark");
        }

        if !state.is_free_layout() && ui.button("Padding").clicked() {
            state.apply_editor_action(crate::editor::EditorAction::add(LabelElement::Padding {
                pixels: 20,
            }));
            info!("Added padding element");
        }
    });
}

/// Submit the current preview with the active job correction applied.
fn do_print(state: &mut AppState) {
    if !state.printer_connected || state.is_printer_busy() || state.printer_quality.is_none() {
        return;
    }
    let Some(preview) = state.output_preview() else {
        return;
    };
    let resolution = preview.resolution;
    let corrected = match ptouch_render::calibration::apply_calibration(
        &preview.bitmap,
        resolution.feed_dpi,
        state.calibration_offset(),
    ) {
        Ok(bitmap) => bitmap,
        Err(error) => {
            state.status_message = error.to_string();
            return;
        }
    };
    let raster_lines = if let Some(left) = state.band_left {
        raster::bitmap_to_raster_lines_at(&corrected, state.printer_max_px, left)
    } else {
        raster::bitmap_to_raster_lines(&corrected, state.printer_max_px)
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

    let document = state.document();

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

    match LabelDocument::from_toml_str(&text)
        .map_err(|error| error.to_string())
        .and_then(|document| apply_layout(state, document))
    {
        Ok(()) => {
            state.status_message = format!("Opened {}", path.display());
            info!("Opened layout: {}", path.display());
        }
        Err(e) => {
            state.status_message = format!("Open error: {}", e);
            error!("Layout parse error: {}", e);
        }
    }
}

pub(crate) fn apply_layout(state: &mut AppState, document: LabelDocument) -> Result<(), String> {
    // Reject before touching the current design: an unknown width must not
    // inherit the previous document's raster height or differ from CLI export.
    if ptouch_core::tape::find_tape(document.tape_width_mm, document.dpi).is_none() {
        return Err(format!(
            "Unknown saved tape width: {} mm",
            document.tape_width_mm
        ));
    }
    state.editor = crate::editor::Editor::default();
    state.document_version = document.version;
    state.design_dpi = document.dpi;
    if !state.printer_connected {
        state.preview_uses_printer = false;
    }
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
    state.editor_generation = state.editor_generation.wrapping_add(1);
    state.elements = document.elements;
    state.select_element(None);
    state.selection_revision = state.selection_revision.wrapping_add(1);
    state.mark_dirty();
    Ok(())
}

fn export_bitmap(
    preview: &crate::preview::RenderedPreview,
    offset: Option<f64>,
) -> ptouch_render::Result<ptouch_render::bitmap::LabelBitmap> {
    if offset.is_none() {
        return Ok(preview.square_pixel_bitmap());
    }
    let bitmap = ptouch_render::calibration::apply_calibration(
        &preview.bitmap,
        preview.resolution.feed_dpi,
        offset,
    )?;
    Ok(crate::preview::square_pixel_bitmap(
        &bitmap,
        preview.resolution,
    ))
}

/// Export the current label preview as an image file.
fn do_export_image(state: &mut AppState, corrected: bool) {
    let preview = match state.output_preview() {
        Some(preview) => preview,
        None => {
            state.status_message = "Nothing to export".to_string();
            return;
        }
    };

    let bitmap = match export_bitmap(
        preview,
        corrected.then(|| state.calibration_offset()).flatten(),
    ) {
        Ok(bitmap) => bitmap,
        Err(error) => {
            state.status_message = error.to_string();
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
        match bitmap.save(&save_path) {
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
    fn print_calibration_matches_shared_output_without_editing_the_layout() {
        for (model, head) in [("PT-P750W", 180), ("PT-9700PC", 360)] {
            let quality = ptouch_core::quality::QualityProfile::for_model(model, head);
            for mode in quality.modes {
                for offset in [None, Some(1.0), Some(-0.4), Some(-10.0)] {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let mut bitmap = ptouch_render::bitmap::LabelBitmap::new(80, 32);
                    bitmap.set_pixel(10, 3, true);
                    let mut state = AppState {
                        printer_connected: true,
                        printer_max_px: 128,
                        preview: Some(crate::preview::RenderedPreview::new(
                            &egui::Context::default(),
                            bitmap.clone(),
                            mode.resolution,
                        )),
                        printer_quality: Some(quality),
                        printer_dpi: head,
                        print_quality: mode.quality,
                        needs_rerender: false,
                        calibration_mode: if offset.is_some() {
                            crate::state::CalibrationMode::Custom
                        } else {
                            crate::state::CalibrationMode::Disabled
                        },
                        printer_cmd_tx: Some(tx),
                        ..Default::default()
                    };
                    state.add_text();
                    state.needs_rerender = false;
                    state.calibration.offset_mm = offset.unwrap_or(20.0);
                    let before = state.document().to_toml_string().unwrap();
                    execute_action(&mut state, ToolbarAction::Print);
                    let expected = ptouch_render::calibration::apply_calibration(
                        &bitmap,
                        mode.resolution.feed_dpi,
                        offset,
                    );
                    let exported = export_bitmap(state.output_preview().unwrap(), offset);
                    assert_eq!(exported.is_ok(), expected.is_ok());
                    if let (Ok(exported), Ok(expected)) = (&exported, &expected) {
                        let square = crate::preview::square_pixel_bitmap(expected, mode.resolution);
                        assert_eq!(
                            (exported.width(), exported.height(), exported.data()),
                            (square.width(), square.height(), square.data())
                        );
                    }
                    if let Ok(expected) = expected {
                        let PrinterCommand::Print { raster_lines, .. } = rx.try_recv().unwrap()
                        else {
                            panic!("expected print");
                        };
                        assert_eq!(raster_lines, raster::bitmap_to_raster_lines(&expected, 128));
                        assert!(state.operation_in_progress);
                    } else {
                        assert!(rx.try_recv().is_err());
                        assert!(!state.operation_in_progress);
                        assert!(state.status_message.contains("calibration"));
                    }
                    assert_eq!(state.document().to_toml_string().unwrap(), before);
                }
            }
        }
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
    fn opening_unknown_tape_width_rejects_without_changing_the_current_design() {
        let mut state = AppState::default();
        state.add_text();
        state.needs_rerender = false;
        let before = state.document().to_toml_string().unwrap();
        let mut document = layout(15);
        document.dpi = 360;
        assert_eq!(
            apply_layout(&mut state, document).unwrap_err(),
            "Unknown saved tape width: 15 mm"
        );
        assert_eq!(state.document().to_toml_string().unwrap(), before);
        assert_eq!(state.tape_width_px, 76);
        assert_eq!(state.selected_element, Some(0));
        assert!(!state.needs_rerender);
    }

    #[test]
    fn opening_and_saving_legacy_layout_keeps_version_and_flow() {
        for version in [1, 2] {
            let mut document = layout(12);
            document.version = version;
            document.elements = vec![LabelElement::Padding { pixels: 20 }];
            let mut state = AppState::default();
            apply_layout(&mut state, document).unwrap();
            state.add_text();
            assert!(!state.is_free_layout());
            let saved = state.document();
            assert_eq!(saved.version, version);
            assert!(matches!(
                saved.elements[1],
                LabelElement::Text { text_box: None, .. }
            ));
            let reopened = LabelDocument::from_toml_str(&saved.to_toml_string().unwrap()).unwrap();
            assert_eq!(reopened.version, version);
        }
    }

    #[test]
    fn explicit_conversion_is_saved_and_reopened_as_free_layout() {
        let mut document = layout(12);
        document.version = 1;
        document.elements = vec![LabelElement::Padding { pixels: 20 }, LabelElement::CutMark];
        let mut state = AppState::default();
        apply_layout(&mut state, document).unwrap();
        state.convert_to_free_layout().unwrap();
        let saved = state.document().to_toml_string().unwrap();
        let mut reopened = AppState::default();
        apply_layout(&mut reopened, LabelDocument::from_toml_str(&saved).unwrap()).unwrap();
        assert!(reopened.is_free_layout());
        assert!(
            reopened
                .elements
                .iter()
                .all(|element| element.as_object().is_some())
        );
        assert_eq!(reopened.elements.len(), 2);
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
            apply_layout(&mut state, layout(width)).unwrap();
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
        apply_layout(&mut state, layout(12)).unwrap();
        assert_eq!((state.tape_width_mm, state.tape_width_px), (12, 76));
    }

    #[test]
    fn opening_offline_layout_restores_design_dpi_after_printer_disconnect() {
        let mut state = AppState {
            printer_dpi: 180,
            printer_max_px: 128,
            preview_uses_printer: true,
            ..AppState::default()
        };
        let mut document = layout(24);
        document.dpi = 360;
        apply_layout(&mut state, document).unwrap();
        assert_eq!(state.design_dpi, 360);
        assert_eq!(state.target_dpi(), 360);
        assert_eq!(state.tape_width_px, 320);
        assert_eq!(state.document().dpi, 360);
    }

    #[test]
    fn connected_printer_overrides_preview_dpi_without_changing_saved_design() {
        let mut state = AppState {
            printer_connected: true,
            printer_dpi: 180,
            printer_max_px: 128,
            tape_width_px: 128,
            ..AppState::default()
        };
        let mut document = layout(24);
        document.dpi = 360;
        apply_layout(&mut state, document).unwrap();
        assert_eq!(state.target_dpi(), 180);
        assert_eq!(state.tape_width_px, 128);
        let saved = state.document();
        assert_eq!(saved.dpi, 360);
        let reopened = LabelDocument::from_toml_str(&saved.to_toml_string().unwrap()).unwrap();
        let mut offline = AppState::default();
        apply_layout(&mut offline, reopened).unwrap();
        assert_eq!(offline.target_dpi(), 360);
        assert_eq!(offline.tape_width_px, 150);
    }

    #[test]
    fn opening_layout_offline_releases_previous_bluetooth_geometry() {
        let mut state = AppState {
            printer_target: PrinterTarget::Bluetooth {
                name: "PT-P300BT".into(),
                address: "AA:BB:CC:DD:EE:FF".into(),
            },
            printer_max_px: 128,
            preview_uses_printer: true,
            tape_width_px: 64,
            ..AppState::default()
        };
        let mut document = layout(24);
        document.dpi = 360;
        apply_layout(&mut state, document).unwrap();
        assert_eq!(state.target_dpi(), 360);
        assert_eq!((state.tape_width_mm, state.tape_width_px), (24, 320));
    }

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
            design_dpi: 360,
            print_quality: PrintQuality::HighRes,
            ..Default::default()
        };
        assert_eq!(
            state.preview_resolution().unwrap(),
            PrintResolution::square(360)
        );
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
            apply_layout(&mut state, layout(width)).unwrap();
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
}
