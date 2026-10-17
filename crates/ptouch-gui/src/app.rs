// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Main application struct and eframe::App implementation.

use std::sync::mpsc;

use log::{error, info};

use ptouch_render::text::TextRenderer;

use crate::panels;
use crate::printer_worker;
use crate::state::{AppState, PrinterEvent, PrinterResponse};

/// The main P-Touch GUI application.
pub struct PtouchApp {
    /// Application state shared across all panels.
    pub state: AppState,
    /// Text renderer instance for generating label bitmaps.
    renderer: TextRenderer,
    /// Receiver for responses from the printer worker thread.
    resp_rx: mpsc::Receiver<PrinterEvent>,
    smoke_frames: Option<u8>,
}

impl PtouchApp {
    /// Create a new application instance.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        smoke_test: bool,
        offline: bool,
        profiles_dir: std::path::PathBuf,
    ) -> Self {
        setup_fallback_fonts(&cc.egui_ctx);

        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (resp_tx, resp_rx) = mpsc::channel();

        let enable_printer = !smoke_test && !offline;
        if enable_printer {
            let ctx = cc.egui_ctx.clone();
            std::thread::Builder::new()
                .name("printer-worker".to_string())
                .spawn(move || {
                    printer_worker::printer_worker(cmd_rx, resp_tx, ctx);
                })
                .expect("failed to spawn printer worker thread");
        }

        let mut app = Self {
            state: AppState {
                offline,
                available_fonts: ptouch_render::font::list_fonts(),
                printer_cmd_tx: enable_printer.then_some(cmd_tx),
                printer_status: (!enable_printer).then(|| "Offline mode".to_string()),
                ..AppState::default()
            },
            renderer: TextRenderer::new(),
            resp_rx,
            smoke_frames: smoke_test.then_some(0),
        };
        app.state.profiles = crate::profile_ui::ProfilesUi::load(profiles_dir);
        app.state.profiles.open = smoke_test;
        if let Some(error) = &app.state.profiles.error {
            app.state.printer_target = crate::state::PrinterTarget::Unavailable(error.clone());
        } else if let Some(profile) = app.state.profiles.default_profile() {
            app.state.printer_target = crate::state::PrinterTarget::Profile(profile);
            app.state.apply_target_preferences();
        }
        if enable_printer {
            app.state.select_target(app.state.printer_target.clone());
        } else if smoke_test {
            app.state.document_version = 1;
            app.state.preview_uses_printer = true;
            // Exercise resolution-aware rendering and tiled texture uploads
            // in the real window renderer, without starting printer I/O.
            app.state.printer_quality = Some(ptouch_core::quality::QualityProfile::for_model(
                "PT-P750W", 180,
            ));
            app.state
                .set_print_quality(ptouch_core::PrintQuality::HighRes);
            app.state.elements.push(crate::state::LabelElement::Text {
                text_box: None,
                content: "Preview Wi-Fi W ".repeat(300),
                font_size: Some(24.0),
                align: ptouch_render::text::TextAlign::Left,
                rotation: 0.0,
                flip_h: false,
                flip_v: false,
            });
            app.state
                .apply_editor_action(crate::editor::EditorAction::Add(
                    crate::state::LabelElement::CutMark,
                ));
            app.state
                .apply_editor_action(crate::editor::EditorAction::Add(
                    crate::state::LabelElement::Padding { pixels: 20 },
                ));
        }
        app
    }

    /// Re-render the preview bitmap from the current element list.
    pub fn update_preview(&mut self, ctx: &egui::Context) {
        self.state.needs_rerender = false;

        self.state.preview = None;
        self.state.preview_bounds.clear();
        self.state.preview_content_bounds.clear();
        self.state.preview_diagnostics.clear();
        let result = self
            .state
            .preview_resolution()
            .map_err(|e| e.to_string())
            .and_then(|resolution| {
                self.state
                    .resolve_layout()
                    .map_err(|error| error.to_string())?;
                ptouch_render::document::render_document_at_resolution(
                    &self.state.document(),
                    self.state.tape_width_px,
                    resolution,
                    &mut self.renderer,
                )
                .map(|rendered| (rendered, resolution))
                .map_err(|e| e.to_string())
            });
        match result {
            Ok((rendered, resolution)) => {
                self.state.preview_bounds = rendered.bounds;
                self.state.preview_content_bounds = rendered.content_bounds;
                self.state.preview_diagnostics = rendered.diagnostics;
                if self.state.status_message.starts_with("Render error:") {
                    self.state.status_message = "Preview updated".into();
                }
                if let Some(bitmap) = rendered.bitmap.filter(|b| b.width() > 0 && b.height() > 0) {
                    let mut preview = crate::preview::RenderedPreview::new(ctx, bitmap, resolution);
                    preview.element_spans = vec![None; self.state.elements.len()];
                    for bound in &self.state.preview_bounds {
                        preview.element_spans[bound.element_index] =
                            Some(bound.x..bound.x + bound.width);
                    }
                    self.state.preview = Some(preview);
                }
            }
            Err(error) => {
                error!("Render failed: {error}");
                self.state.status_message = format!("Render error: {error}");
                self.state.preview_diagnostics.push(error);
            }
        }
        ctx.request_repaint();
        info!("Preview updated");
    }

    fn drain_printer_responses(&mut self) {
        // Drain all pending responses from the printer worker
        while let Ok(event) = self.resp_rx.try_recv() {
            if event
                .target
                .as_ref()
                .is_some_and(|target| target != &self.state.selection())
            {
                continue;
            }
            match event.response {
                PrinterResponse::UsbProfiles(result) => {
                    self.state.profiles.usb_discovered(result);
                }
                PrinterResponse::UsbDevices(devices) => {
                    self.state.usb_targets = devices;
                }
                PrinterResponse::BluetoothDevices(devices) => {
                    self.state.bluetooth_targets = devices;
                }
                PrinterResponse::Connected {
                    model_name,
                    media_width,
                    media_type,
                    max_px,
                    dpi,
                    media_code,
                    band_left,
                    tape_width_px,
                } => {
                    let previous_resolution = self.state.preview_resolution().ok();
                    let geometry_changed = (
                        self.state.band_left,
                        self.state.tape_width_mm,
                        self.state.printer_dpi,
                        self.state.media_code,
                    ) != (band_left, media_width, dpi, media_code);
                    self.state.printer_connected = true;
                    self.state.preview_uses_printer = true;
                    self.state.connecting = false;
                    self.state.printer_max_px = max_px;
                    self.state.printer_dpi = dpi;
                    let quality = ptouch_core::quality::QualityProfile::for_model(&model_name, dpi);
                    self.state.printer_quality = Some(quality);
                    self.state.printer_model_id = model_name.clone();
                    self.state.media_code = media_code;
                    self.state.band_left = band_left;
                    // A stale non-standard quality from a previous printer
                    // would make every print fail with the selector hidden.
                    if quality.mode(self.state.print_quality).is_err() {
                        self.state
                            .set_print_quality(ptouch_core::protocol::PrintQuality::Standard);
                    }
                    self.state.printer_model =
                        Some(format!("{}: {} mm {}", model_name, media_width, media_type));
                    self.state.printer_status = Some("Connected".to_string());
                    if media_width > 0 {
                        self.state.tape_width_mm = media_width;
                    }
                    // Use the printer's status-derived printable width. This
                    // differs from the transfer width on the PT-P300BT.
                    let old_px = self.state.tape_width_px;
                    self.state.tape_width_px = u32::from(tape_width_px);
                    if self.state.tape_width_px != old_px
                        || geometry_changed
                        || self.state.preview_resolution().ok() != previous_resolution
                    {
                        self.state.mark_dirty();
                    }
                }
                PrinterResponse::ConnectionError(message) => {
                    self.state.printer_status = Some(message);
                    self.state.printer_model = None;
                    self.state.printer_connected = false;
                    self.state.connecting = false;
                    // Keep printer_max_px, printer_dpi, and quality state:
                    // the canvas must not resize on a transient disconnect,
                    // and printing is gated on printer_connected anyway.
                }
                PrinterResponse::SubmittedUnconfirmed => {
                    self.state.operation_in_progress = false;
                    self.state.cancellation = None;
                    self.state.status_message =
                        "Submitted, completion unconfirmed. Check the printer before retrying."
                            .into();
                }
                PrinterResponse::PrintDone => {
                    self.state.operation_in_progress = false;
                    self.state.cancellation = None;
                    self.state.status_message = "Print complete".to_string();
                }
                PrinterResponse::FeedAndCutDone => {
                    self.state.operation_in_progress = false;
                    self.state.cancellation = None;
                    self.state.status_message = "Feed & cut done".to_string();
                }
                PrinterResponse::Error(msg) => {
                    self.state.operation_in_progress = false;
                    self.state.cancellation = None;
                    self.state.status_message = msg;
                }
            }
        }
    }
}

impl PtouchApp {
    fn show_editor(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.drain_printer_responses();
        crate::editor::begin_frame(&ctx, &mut self.state);

        ui.add_enabled_ui(!self.state.symbol_picker.is_open(), |ui| {
            // Top toolbar
            let toolbar_action = egui::Panel::top("toolbar")
                .show(ui, |ui| panels::toolbar::show_toolbar(ui, &mut self.state))
                .inner;

            // Bottom status bar
            egui::Panel::bottom("status_bar").show(ui, |ui| {
                panels::status_bar::show_status_bar(ui, &self.state);
            });

            // Reordering or opening a file invalidates geometry by element index.
            // Properties must freeze the current object's geometry on first edit.
            if self.state.needs_rerender {
                self.update_preview(&ctx);
            }

            // Right properties panel
            egui::Panel::right("properties")
                .default_size(250.0)
                .resizable(true)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        panels::properties::show_properties(ui, &mut self.state);
                    });
                });

            if self.state.needs_rerender {
                self.update_preview(&ctx);
            }

            // Commit the old selection before list and creation actions.
            // Left sidebar
            egui::Panel::left("sidebar")
                .default_size(200.0)
                .resizable(true)
                .show(ui, |ui| {
                    panels::sidebar::show_sidebar(ui, &mut self.state);
                });

            if self.state.needs_rerender {
                self.update_preview(&ctx);
            }
            if let Some(action) = toolbar_action {
                panels::toolbar::execute_action(&mut self.state, action);
            }
            if self.state.needs_rerender {
                self.update_preview(&ctx);
            }

            // Central canvas
            egui::CentralPanel::default().show(ui, |ui| {
                let _ = panels::canvas::show_canvas(ui, &mut self.state);
            });
        });

        crate::profile_ui::show(&ctx, &mut self.state);
        crate::symbols::show(&ctx, &mut self.state);
        crate::editor::end_frame(&ctx, &mut self.state);
        if let Some(drag) = self.state.element_drag(&ctx) {
            crate::preview::paint_drag_ghost(&ctx, self.state.elements[drag.index].display_name());
        }

        // Re-render preview if dirty
        if self.state.needs_rerender {
            self.update_preview(&ctx);
        }

        // Exit only after multiple frames exercised the actual window renderer.
        if let Some(frames) = &mut self.smoke_frames {
            *frames += 1;
            if *frames == 1 {
                self.state.profiles.open = false;
                crate::editor::apply_action(
                    &ctx,
                    &mut self.state,
                    crate::editor::EditorAction::Undo,
                );
                self.state.view.select(crate::preview::ZoomMode::ActualSize);
                self.state.select_element(Some(0));
            } else if *frames == 2 {
                crate::editor::apply_action(
                    &ctx,
                    &mut self.state,
                    crate::editor::EditorAction::Redo,
                );
                self.state.view.select(crate::preview::ZoomMode::Pixels);
            }
            if *frames == 3 {
                // Exercise the free-layout canvas and native SVG rendering too.
                self.state.elements.clear();
                self.state.document_version = ptouch_render::document::DEFAULT_DOCUMENT_VERSION;
                self.state.add_text();
                let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="4"><path d="M1 3L7 1" stroke="black"/></svg>"#;
                self.state.add_image_bytes(None, svg.to_vec());
                if let ptouch_render::document::ObjectContent::Text { styles, .. } =
                    &mut self.state.elements[0].as_object_mut().unwrap().content
                {
                    styles.push(ptouch_render::styled_text::StyleSpan {
                        start: 0,
                        end: 5,
                        style: ptouch_render::styled_text::TextStyle {
                            bold: true,
                            underline: true,
                            ..Default::default()
                        },
                    });
                }
                self.state.select_element(Some(0));
                self.state.view.select(crate::preview::ZoomMode::Fit);
            } else if *frames == 4 {
                self.state
                    .select_canvas_target(Some(crate::state::CanvasTarget {
                        element: 0,
                        edit: crate::state::CanvasEditTarget::Slot,
                    }));
                let symbol = crate::symbols::catalogue()
                    .icons
                    .iter()
                    .find(|symbol| symbol.id == "ethernet")
                    .unwrap();
                self.state.add_image_bytes(None, symbol.svg());
                self.state.open_symbol_picker(Some(2));
            }
            if *frames >= 7 {
                println!("PTOUCH_GUI_SMOKE_OK");
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                self.smoke_frames = None;
            }
            ctx.request_repaint();
        }

        // Periodic repaint so we pick up worker responses even when idle
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }
}

impl eframe::App for PtouchApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show_editor(ui);
    }
}

/// Register fallback fonts for CJK text and emoji rendering.
fn setup_fallback_fonts(ctx: &egui::Context) {
    use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};

    let lowest_both = vec![
        InsertFontFamily {
            family: egui::FontFamily::Proportional,
            priority: FontPriority::Lowest,
        },
        InsertFontFamily {
            family: egui::FontFamily::Monospace,
            priority: FontPriority::Lowest,
        },
    ];

    // CJK fallback (DroidSansFallback, Apache-2.0)
    ctx.add_font(FontInsert {
        name: "cjk_fallback".into(),
        data: egui::FontData::from_static(include_bytes!("../assets/fonts/DroidSansFallback.ttf")),
        families: lowest_both.clone(),
    });

    // Emoji fallback (NotoEmoji, OFL-1.1)
    ctx.add_font(FontInsert {
        name: "emoji_fallback".into(),
        data: egui::FontData::from_static(include_bytes!("../assets/fonts/NotoEmoji.ttf")),
        families: lowest_both,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PrinterTarget;

    fn source_document(rotation: f32) -> ptouch_render::document::LabelDocument {
        use ptouch_render::document::{LabelDocument, LabelElement};
        let image = image::GrayImage::from_fn(96, 64, |x, y| {
            image::Luma([if (x + y) % 7 == 0 { 0 } else { 255 }])
        });
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(image)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let mut raster = LabelElement::image_from_bytes(None, png.into_inner());
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="32"><path d="M1 31L47 1" stroke="black"/></svg>"#;
        let mut vector = LabelElement::image_from_bytes(None, svg.to_vec());
        for element in [&mut raster, &mut vector] {
            if let LabelElement::Image {
                target_height,
                rotation: angle,
                flip_h,
                ..
            } = element
            {
                *target_height = Some(32);
                *angle = rotation;
                *flip_h = true;
            }
        }
        LabelDocument {
            version: ptouch_render::document::DOCUMENT_VERSION,
            tape_width_mm: 12,
            dpi: 180,
            font_name: "sans-serif".into(),
            font_margin: 1,
            flip_h: true,
            flip_v: true,
            elements: vec![
                LabelElement::Text {
                    text_box: None,
                    content: "Wi-Fi W\nshort".into(),
                    font_size: Some(24.0),
                    align: ptouch_render::text::TextAlign::Center,
                    rotation,
                    flip_h: true,
                    flip_v: false,
                },
                raster,
                vector,
                LabelElement::Padding { pixels: 7 },
                LabelElement::CutMark,
            ],
        }
    }

    #[test]
    fn loaded_sources_preview_every_quality_with_correct_geometry_and_mirroring() {
        use ptouch_core::quality::QualityProfile;
        use ptouch_render::document::{LabelDocument, render_elements_at_resolution};
        let ctx = egui::Context::default();
        for rotation in [0.0, 37.0, 90.0] {
            let saved = source_document(rotation).to_toml_string().unwrap();
            for (name, head, height) in [
                ("PT-P750W", 180, 70),
                ("PT-9500PC", 360, 150),
                ("PT-9700PC", 360, 150),
            ] {
                let profile = QualityProfile::for_model(name, head);
                let (mut app, _) = app(AppState {
                    printer_connected: true,
                    printer_dpi: head,
                    tape_width_mm: 12,
                    tape_width_px: height,
                    printer_quality: Some(profile),
                    ..Default::default()
                });
                // Loading changes the design, never the selected quality.
                app.state
                    .set_print_quality(ptouch_core::PrintQuality::HighRes);
                crate::panels::toolbar::apply_layout(
                    &mut app.state,
                    LabelDocument::from_toml_str(&saved).unwrap(),
                )
                .unwrap();
                assert_eq!(app.state.print_quality, ptouch_core::PrintQuality::HighRes);
                assert!(app.state.needs_rerender);
                let mut standard_width = 0;
                // Return to standard after testing high/draft as well.
                for mode in profile.modes.iter().chain(&profile.modes[..1]) {
                    app.state.set_print_quality(mode.quality);
                    if let Some(preview) = &app.state.preview
                        && preview.resolution != mode.resolution
                    {
                        assert!(app.state.needs_rerender);
                        assert!(app.state.output_preview().is_none());
                    }
                    app.update_preview(&ctx);
                    let preview = app.state.output_preview().unwrap();
                    assert_eq!(preview.resolution, mode.resolution);
                    assert_eq!(preview.bitmap.height(), height);
                    let expected = render_elements_at_resolution(
                        &app.state.elements,
                        height,
                        &app.state.font_name,
                        app.state.font_margin,
                        &mut TextRenderer::new(),
                        mode.resolution,
                    )
                    .unwrap()
                    .unwrap()
                    .mirrored(true, true);
                    assert_eq!(preview.bitmap.width(), expected.width());
                    assert_eq!(preview.bitmap.data(), expected.data());
                    if mode.quality == ptouch_core::PrintQuality::Standard {
                        standard_width = preview.bitmap.width();
                    }
                    if rotation == 0.0 {
                        assert!((preview.size().x - standard_width as f32).abs() <= 2.0);
                    }
                    if mode.quality == ptouch_core::PrintQuality::HighRes && rotation == 0.0 {
                        assert!((0..preview.bitmap.width() / 2).any(|x| {
                            (0..height).any(|y| {
                                preview.bitmap.get_pixel(2 * x, y)
                                    != preview.bitmap.get_pixel(2 * x + 1, y)
                            })
                        }));
                    }
                }
                assert_eq!(app.state.elements.len(), 5);
            }
            assert_eq!(
                LabelDocument::from_toml_str(&saved)
                    .unwrap()
                    .to_toml_string()
                    .unwrap(),
                saved
            );
        }
    }

    #[test]
    fn resolution_only_status_changes_refresh_and_disconnects_keep_preview() {
        use ptouch_core::{PrintQuality, quality::QualityProfile};
        let (mut app, tx) = app(AppState {
            tape_width_mm: 24,
            tape_width_px: 128,
            printer_max_px: 128,
            media_code: 1,
            print_quality: PrintQuality::HighRes,
            elements: source_document(0.0).elements,
            ..Default::default()
        });
        let ctx = egui::Context::default();
        app.update_preview(&ctx);
        assert_eq!(app.state.preview.as_ref().unwrap().resolution.feed_dpi, 180);
        let mut status = usb_status();
        if let PrinterResponse::Connected { model_name, .. } = &mut status.response {
            *model_name = "PT-P750W".into();
        }
        tx.send(status).unwrap();
        app.drain_printer_responses();
        assert!(app.state.needs_rerender);
        app.update_preview(&ctx);
        let pixels = app.state.preview.as_ref().unwrap().bitmap.data().to_vec();
        assert_eq!(app.state.preview.as_ref().unwrap().resolution.feed_dpi, 360);
        tx.send(PrinterEvent {
            target: Some(app.state.selection()),
            response: PrinterResponse::ConnectionError("Disconnected".into()),
        })
        .unwrap();
        app.drain_printer_responses();
        assert!(!app.state.needs_rerender);
        assert_eq!(app.state.output_preview().unwrap().bitmap.data(), pixels);
        tx.send(usb_status()).unwrap();
        app.drain_printer_responses();
        assert_eq!(app.state.print_quality, PrintQuality::Standard);
        assert!(app.state.needs_rerender);
        app.update_preview(&ctx);
        assert_eq!(app.state.output_preview().unwrap().resolution.feed_dpi, 180);
        assert!(
            QualityProfile::for_model("PT-P700", 180)
                .mode(PrintQuality::HighRes)
                .is_err()
        );
    }

    #[test]
    fn render_failures_and_empty_designs_clear_previous_output() {
        use ptouch_core::{PrintQuality, quality::QualityProfile};
        let (mut app, _) = app(AppState {
            elements: source_document(0.0).elements,
            printer_connected: true,
            ..Default::default()
        });
        let ctx = egui::Context::default();
        app.update_preview(&ctx);
        assert!(app.state.output_preview().is_some());
        app.state.printer_quality = Some(QualityProfile::for_model("PT-P700", 180));
        app.state.set_print_quality(PrintQuality::HighRes);
        app.update_preview(&ctx);
        assert!(app.state.preview.is_none());
        assert!(app.state.status_message.starts_with("Render error:"));
        app.state.set_print_quality(PrintQuality::Standard);
        app.update_preview(&ctx);
        assert!(app.state.output_preview().is_some());
        app.state.elements.clear();
        app.state.mark_dirty();
        app.update_preview(&ctx);
        assert!(app.state.preview.is_none());
    }

    fn app(state: AppState) -> (PtouchApp, mpsc::Sender<PrinterEvent>) {
        let (tx, rx) = mpsc::channel();
        (
            PtouchApp {
                state,
                renderer: TextRenderer::new(),
                resp_rx: rx,
                smoke_frames: None,
            },
            tx,
        )
    }

    fn usb_status() -> PrinterEvent {
        PrinterEvent {
            target: Some(crate::state::PrinterSelection {
                target: PrinterTarget::Usb,
                generation: 0,
            }),
            response: PrinterResponse::Connected {
                model_name: "PT-P700".into(),
                media_width: 24,
                media_type: "Laminated tape".into(),
                max_px: 128,
                dpi: 180,
                media_code: 1,
                band_left: None,
                tape_width_px: 128,
            },
        }
    }

    #[test]
    fn reselecting_same_target_rejects_previous_generation_responses() {
        let (mut app, tx) = app(AppState::default());
        let stale = usb_status();
        app.state.select_target(PrinterTarget::Usb);
        tx.send(stale).unwrap();
        app.drain_printer_responses();
        assert!(!app.state.printer_connected);
        assert!(app.state.connecting);
        let mut fresh = usb_status();
        fresh.target = Some(app.state.selection());
        tx.send(fresh).unwrap();
        app.drain_printer_responses();
        assert!(app.state.printer_connected);
    }

    #[test]
    fn late_usb_poll_does_not_connect_selected_bluetooth_printer() {
        let (mut app, tx) = app(AppState {
            printer_target: PrinterTarget::Bluetooth {
                name: "PT-P300BT".into(),
                address: "AA:BB:CC:DD:EE:FF".into(),
            },
            connecting: true,
            ..AppState::default()
        });
        tx.send(usb_status()).unwrap();
        app.drain_printer_responses();
        assert!(!app.state.printer_connected);
        assert!(app.state.connecting);
        assert!(app.state.printer_model.is_none());
        tx.send(PrinterEvent {
            target: Some(crate::state::PrinterSelection {
                target: PrinterTarget::Usb,
                generation: 0,
            }),
            response: PrinterResponse::ConnectionError("Device not found".into()),
        })
        .unwrap();
        app.drain_printer_responses();
        assert!(app.state.connecting);

        tx.send(PrinterEvent {
            target: Some(app.state.selection()),
            response: PrinterResponse::Connected {
                model_name: "PT-P300BT".into(),
                media_width: 12,
                media_type: "Laminated tape".into(),
                max_px: 128,
                dpi: 180,
                media_code: 1,
                band_left: None,
                tape_width_px: 64,
            },
        })
        .unwrap();
        app.drain_printer_responses();
        assert!(app.state.printer_connected);
        assert!(!app.state.is_printer_busy());
        assert_eq!(app.state.tape_width_px, 64);
    }

    #[test]
    fn status_reply_does_not_finish_pending_print() {
        let (mut app, tx) = app(AppState {
            printer_connected: true,
            operation_in_progress: true,
            ..AppState::default()
        });
        tx.send(usb_status()).unwrap();
        app.drain_printer_responses();
        assert!(app.state.operation_in_progress);
        assert!(
            app.state
                .printer_model
                .as_ref()
                .unwrap()
                .starts_with("PT-P700")
        );
        tx.send(PrinterEvent {
            target: Some(crate::state::PrinterSelection {
                target: PrinterTarget::Usb,
                generation: 0,
            }),
            response: PrinterResponse::ConnectionError("Device not found".into()),
        })
        .unwrap();
        app.drain_printer_responses();
        assert!(!app.state.printer_connected);
        assert_eq!(
            app.state.printer_status.as_deref(),
            Some("Device not found")
        );
        assert!(app.state.operation_in_progress);
        tx.send(PrinterEvent {
            target: Some(crate::state::PrinterSelection {
                target: PrinterTarget::Usb,
                generation: 0,
            }),
            response: PrinterResponse::PrintDone,
        })
        .unwrap();
        app.drain_printer_responses();
        assert!(!app.state.operation_in_progress);
    }

    #[test]
    fn usb_selection_ignores_another_devices_poll_and_keeps_error_detail() {
        let target =
            PrinterTarget::UsbAt(ptouch_core::transport::UsbLocation { bus: 1, address: 5 });
        let (mut app, tx) = app(AppState {
            printer_target: target.clone(),
            ..AppState::default()
        });
        tx.send(usb_status()).unwrap();
        app.drain_printer_responses();
        assert!(!app.state.printer_connected);
        tx.send(PrinterEvent {
            target: Some(crate::state::PrinterSelection {
                target,
                generation: 0,
            }),
            response: PrinterResponse::ConnectionError("USB open: Access denied".into()),
        })
        .unwrap();
        app.drain_printer_responses();
        assert_eq!(
            app.state.printer_status.as_deref(),
            Some("USB open: Access denied")
        );
    }

    #[test]
    fn changing_target_dpi_rerenders_even_when_printable_pixels_match() {
        let (mut app, tx) = app(AppState {
            design_dpi: 360,
            tape_width_px: 128,
            needs_rerender: false,
            ..AppState::default()
        });
        tx.send(usb_status()).unwrap();
        app.drain_printer_responses();
        assert_eq!(app.state.target_dpi(), 180);
        assert_eq!(app.state.design_dpi, 360);
        assert!(app.state.needs_rerender);
        app.state.needs_rerender = false;
        tx.send(PrinterEvent {
            target: Some(app.state.selection()),
            response: PrinterResponse::ConnectionError("Disconnected".into()),
        })
        .unwrap();
        app.drain_printer_responses();
        assert_eq!(app.state.target_dpi(), 180);
        assert!(!app.state.needs_rerender);
        assert!(app.state.preview_uses_printer);
    }

    #[test]
    fn preview_matches_shared_render_and_preserves_original_bitmap_after_downsampling() {
        use ptouch_render::document::{LabelElement, TextBox, render_document};
        use ptouch_render::text::TextAlign;
        let (mut app, _tx) = app(AppState {
            overall_flip_h: true,
            elements: vec![LabelElement::Text {
                content: "Port 1".into(),
                font_size: None,
                align: TextAlign::Center,
                rotation: 0.0,
                flip_h: false,
                flip_v: false,
                text_box: Some(TextBox {
                    x_mm: 5.0,
                    width_mm: 300.0,
                    height_mm: 8.0,
                    ..TextBox::default()
                }),
            }],
            ..AppState::default()
        });
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput {
            max_texture_side: Some(1024),
            ..Default::default()
        });
        app.update_preview(&ctx);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        let expected = render_document(
            &app.state.document(),
            app.state.tape_width_px,
            app.state.target_dpi(),
            &mut TextRenderer::new(),
        )
        .unwrap();
        let bitmap = &app.state.preview.as_ref().unwrap().bitmap;
        assert_eq!(
            bitmap.to_rgba_image(),
            expected.bitmap.unwrap().to_rgba_image()
        );
        assert!(bitmap.width() > 1024);
        assert!(app.state.preview.as_ref().unwrap().tiles.len() > 1);
        assert!(
            app.state
                .preview
                .as_ref()
                .unwrap()
                .tiles
                .iter()
                .all(|tile| tile.texture.size()[0] <= 1024)
        );
        let bound = &app.state.preview_bounds[0];
        assert_eq!(
            (bound.x, bound.width),
            (expected.bounds[0].x, expected.bounds[0].width)
        );
        assert!(bound.width > 1024);
    }

    #[test]
    fn reordered_auto_fit_object_uses_fresh_geometry_before_first_keyboard_edit() {
        use eframe::App;
        use ptouch_render::document::{LabelElement, LabelObject, TextBox};

        let first_slot = TextBox {
            x_mm: 5.0,
            y_mm: 1.0,
            width_mm: 6.0,
            height_mm: 4.0,
            ..TextBox::default()
        };
        let (mut app, _tx) = app(AppState {
            document_version: 3,
            selected_element: Some(0),
            elements: vec![
                LabelElement::Object(LabelObject::text("A", first_slot.clone())),
                LabelElement::Object(LabelObject::text(
                    "BBBBBBBBBBBB",
                    TextBox {
                        x_mm: 20.0,
                        width_mm: 18.0,
                        ..first_slot.clone()
                    },
                )),
            ],
            ..AppState::default()
        });
        let context = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let mut run_frame = |app: &mut PtouchApp, events| {
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1400.0, 1000.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| app.ui(ui, &mut frame),
            );
            output.textures_delta.clear();
        };
        run_frame(&mut app, vec![]);
        let expected = app.state.resolved_content_box(0).unwrap();
        let stale_index_geometry = app.state.resolved_content_box(1).unwrap();
        assert!((expected.width_mm - stale_index_geometry.width_mm).abs() > 1.0);
        assert_eq!(app.state.document_version, 3);

        // This is the model mutation made by a sidebar reorder. The cached
        // preview still describes the previous order when this UI frame begins.
        app.state.elements.swap(0, 1);
        app.state.selected_element = Some(1);
        app.state.mark_dirty();
        run_frame(
            &mut app,
            vec![egui::Event::Key {
                key: egui::Key::ArrowRight,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
        );
        let edited = app.state.elements[1].as_object().unwrap();
        let pinned = edited.content_box.as_ref().unwrap();
        assert_eq!(
            (pinned.width_mm, pinned.height_mm),
            (expected.width_mm, expected.height_mm)
        );
        assert!((pinned.offset_x_mm - expected.offset_x_mm - 0.1).abs() < 1e-6);
        assert_eq!(pinned.offset_y_mm, expected.offset_y_mm);
        assert_eq!(edited.frame, first_slot);
        assert!(
            app.state.elements[0]
                .as_object()
                .unwrap()
                .content_box
                .is_none()
        );
        assert_eq!(app.state.document_version, 4);
    }
    #[test]
    fn v5_spacing_and_symbols_reopen_and_preview_at_every_quality() {
        styled_layout_preview_at_every_quality(false, false);
    }

    #[test]
    fn v6_styles_spacing_and_symbols_reopen_and_preview_at_every_quality() {
        styled_layout_preview_at_every_quality(true, false);
    }

    #[test]
    fn invalid_v7_layout_clears_previous_print_and_export_pixels() {
        let ctx = egui::Context::default();
        let (mut app, _) = app(AppState::default());
        app.state.add_text();
        app.update_preview(&ctx);
        assert!(app.state.output_preview().is_some());
        app.state.elements[0]
            .as_object_mut()
            .unwrap()
            .content_box
            .as_mut()
            .unwrap()
            .offset_x_mm = 1.0;
        app.state.mark_dirty();
        app.update_preview(&ctx);
        assert!(app.state.preview.is_none());
        assert!(app.state.output_preview().is_none());
        assert!(
            app.state
                .status_message
                .contains("free objects use frame X/Y")
        );
    }

    #[test]
    fn v7_styles_templates_and_symbols_reopen_and_preview_at_every_quality() {
        styled_layout_preview_at_every_quality(true, true);
    }

    fn styled_layout_preview_at_every_quality(styled: bool, v7: bool) {
        use ptouch_render::document::{
            LabelDocument, ObjectContent, render_document_at_resolution,
        };
        let mut state = AppState::default();
        state.add_text();
        if let ObjectContent::Text {
            content,
            line_spacing,
            styles,
            ..
        } = &mut state.elements[0].as_object_mut().unwrap().content
        {
            *content = "{{port}}\nLAN".into();
            *line_spacing = 0.8;
            if styled {
                styles.push(ptouch_render::styled_text::StyleSpan {
                    start: 0,
                    end: 1,
                    style: ptouch_render::styled_text::TextStyle {
                        font_size: Some(9.0),
                        bold: true,
                        underline: true,
                        ..Default::default()
                    },
                });
            }
        }
        let icon = crate::symbols::catalogue()
            .icons
            .iter()
            .find(|icon| icon.id == "ethernet")
            .unwrap();
        assert!(state.add_image_bytes(None, icon.svg()));
        // Both physical printer profiles must have room for the saved frames.
        for element in &mut state.elements {
            let object = element.as_object_mut().unwrap();
            object.frame.height_mm = 10.0;
            object.frame.y_mm = 0.0;
            object.content_box.as_mut().unwrap().height_mm = 10.0;
            if !v7 {
                object.layout = None;
            }
        }
        state.document_version = 4;
        if v7 {
            use ptouch_render::object_layout::{Placement, WidthSizing};
            state.elements[0]
                .as_object_mut()
                .unwrap()
                .layout
                .as_mut()
                .unwrap()
                .placement = Placement::Flow;
            let symbol = state.elements[1].as_object_mut().unwrap();
            symbol.layout.as_mut().unwrap().placement = Placement::Flow;
            symbol.layout.as_mut().unwrap().width = WidthSizing::Fixed;
            symbol.frame.position = ptouch_render::document::PositionMode::Relative;
            symbol.frame.gap_mm = 2.5;
        }
        let saved = state.document().to_toml_string().unwrap();
        let mut document = LabelDocument::from_toml_str(&saved).unwrap();
        assert_eq!(
            document.version,
            if v7 {
                7
            } else if styled {
                6
            } else {
                5
            }
        );
        assert_eq!(
            document.elements[1].display_name(),
            state.elements[1].display_name()
        );
        document.apply_values(&std::collections::BTreeMap::from([(
            "port".into(),
            "PORT 1".into(),
        )]));
        let ctx = egui::Context::default();
        let mut renderer = TextRenderer::new();
        for (model, head, height) in [("PT-P750W", 180, 76), ("PT-9700PC", 360, 150)] {
            let profile = ptouch_core::quality::QualityProfile::for_model(model, head);
            let (mut app, _) = app(AppState {
                printer_connected: true,
                printer_dpi: head,
                tape_width_px: height,
                printer_quality: Some(profile),
                ..Default::default()
            });
            crate::panels::toolbar::apply_layout(&mut app.state, document.clone()).unwrap();
            for mode in profile.modes {
                app.state.set_print_quality(mode.quality);
                app.update_preview(&ctx);
                let preview = app
                    .state
                    .output_preview()
                    .unwrap_or_else(|| panic!("{}", app.state.status_message));
                let expected = render_document_at_resolution(
                    &document,
                    height,
                    mode.resolution,
                    &mut renderer,
                )
                .unwrap()
                .bitmap
                .unwrap();
                assert_eq!(preview.bitmap.data(), expected.data());
                assert_eq!(preview.bitmap.width(), expected.width());
                assert_eq!(preview.bitmap.height(), expected.height());
            }
        }
    }

    #[test]
    fn property_text_undo_and_redo_preserve_keyboard_focus() {
        let text = |app: &PtouchApp| match &app.state.elements[0].as_object().unwrap().content {
            ptouch_render::document::ObjectContent::Text { content, .. } => content.clone(),
            _ => panic!("expected text"),
        };
        let mut state = AppState::default();
        state.add_text();
        let (mut app, _) = app(state);
        let ctx = egui::Context::default();
        let frame = |app: &mut PtouchApp, events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 680.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| app.show_editor(ui),
            );
            output.textures_delta.clear();
            output
        };
        frame(&mut app, vec![]);
        let output = frame(&mut app, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == "Label" => {
                    Some(text.pos + text.galley.size() / 2.0)
                }
                _ => None,
            })
            .unwrap();
        frame(
            &mut app,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        frame(&mut app, vec![egui::Event::Text("X".into())]);
        let input = ctx.memory(|memory| memory.focused()).unwrap();
        assert_ne!(text(&app), "Label");
        for (redo, expected) in [(false, false), (true, true)] {
            frame(
                &mut app,
                vec![egui::Event::Key {
                    key: egui::Key::Z,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: if redo {
                        egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT)
                    } else {
                        egui::Modifiers::COMMAND
                    },
                }],
            );
            assert_eq!(ctx.memory(|memory| memory.focused()), Some(input));
            assert_eq!(text(&app).contains('X'), expected);
        }
    }

    #[test]
    fn pending_numeric_edits_do_not_leak_through_toolbar_or_list_selection() {
        use ptouch_render::document::{LabelElement, LabelObject, TextBox};
        for action in ["select", "add", "print"] {
            let (command_tx, command_rx) = mpsc::channel();
            let mut state = AppState {
                printer_connected: action == "print",
                printer_quality: Some(ptouch_core::quality::QualityProfile::for_model(
                    "PT-P750W", 180,
                )),
                printer_cmd_tx: Some(command_tx),
                selected_element: Some(0),
                canvas_edit_target: crate::state::CanvasEditTarget::Slot,
                ..Default::default()
            };
            state.elements.push(LabelElement::Object(LabelObject::text(
                "PORT",
                TextBox {
                    width_mm: 14.5,
                    ..Default::default()
                },
            )));
            state.elements.push(LabelElement::Object(LabelObject::text(
                "NEXT",
                TextBox {
                    x_mm: 30.0,
                    width_mm: 20.0,
                    ..Default::default()
                },
            )));
            let (mut app, _tx) = app(state);
            let ctx = egui::Context::default();
            let mut frame = |events| {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1280.0, 680.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ui| app.show_editor(ui),
                );
                output.textures_delta.clear();
                output
            };
            let center = |output: &egui::FullOutput, text: &str| {
                output
                    .shapes
                    .iter()
                    .find_map(|s| {
                        if let egui::Shape::Text(t) = &s.shape
                            && t.galley.job.text == text
                        {
                            Some(t.pos + t.galley.size() / 2.0)
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "missing text: {text}; visible: {:?}",
                            output
                                .shapes
                                .iter()
                                .filter_map(|s| if let egui::Shape::Text(t) = &s.shape {
                                    Some(&t.galley.job.text)
                                } else {
                                    None
                                })
                                .collect::<Vec<_>>()
                        )
                    })
            };
            let click = |pos| {
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]
            };
            frame(vec![]);
            let output = frame(vec![]);
            let input = center(&output, "14.5");
            let action_pos = center(
                &output,
                match action {
                    "add" => "Add Text",
                    "print" => "Print",
                    _ => "2. Text: NEXT",
                },
            );
            frame(click(input));
            frame(vec![]);
            frame(vec![egui::Event::Text("14,6".into())]);
            frame(click(action_pos));
            frame(vec![]);
            assert_eq!(
                app.state.elements[0].as_object().unwrap().frame.width_mm,
                14.6
            );
            assert_eq!(
                app.state.elements.len(),
                if action == "add" { 3 } else { 2 }
            );
            assert_eq!(
                app.state.selected_element,
                Some(match action {
                    "add" => 2,
                    "print" => 0,
                    _ => 1,
                })
            );
            assert_eq!(
                app.state.elements[1].as_object().unwrap().frame.width_mm,
                20.0
            );
            if action == "print" {
                let crate::state::PrinterCommand::Print { raster_lines, .. } =
                    command_rx.try_recv().unwrap()
                else {
                    panic!("expected a print command");
                };
                let preview = app.state.output_preview().unwrap();
                assert_eq!(
                    raster_lines,
                    ptouch_render::raster::bitmap_to_raster_lines(
                        &preview.bitmap,
                        app.state.printer_max_px,
                    )
                );
            }
        }
    }
}
