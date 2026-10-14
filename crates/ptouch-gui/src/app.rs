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
        profiles_dir: std::path::PathBuf,
    ) -> Self {
        setup_fallback_fonts(&cc.egui_ctx);

        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (resp_tx, resp_rx) = mpsc::channel();

        if !smoke_test {
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
                available_fonts: ptouch_render::font::list_fonts(),
                printer_cmd_tx: (!smoke_test).then_some(cmd_tx),
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
        if !smoke_test {
            app.state.select_target(app.state.printer_target.clone());
        } else {
            // Exercise resolution-aware rendering and tiled texture uploads
            // in the real window renderer, without starting printer I/O.
            app.state.printer_quality = Some(ptouch_core::quality::QualityProfile::for_model(
                "PT-P750W", 180,
            ));
            app.state
                .set_print_quality(ptouch_core::PrintQuality::HighRes);
            app.state.elements.push(crate::state::LabelElement::Text {
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

        if self.state.elements.is_empty() {
            self.state.preview = None;
            ctx.request_repaint();
            return;
        }

        let result = self
            .state
            .preview_resolution()
            .map_err(|e| e.to_string())
            .and_then(|resolution| {
                ptouch_render::document::render_elements_with_layout_at_resolution(
                    &self.state.elements,
                    self.state.tape_width_px,
                    &self.state.font_name,
                    self.state.font_margin,
                    &mut self.renderer,
                    resolution,
                )
                .map(|rendered| rendered.map(|rendered| (rendered, resolution)))
                .map_err(|e| e.to_string())
            });

        self.state.preview = match result {
            Ok(result) => result
                .filter(|(rendered, _)| rendered.bitmap.width() > 0 && rendered.bitmap.height() > 0)
                .map(|(rendered, resolution)| {
                    // Apply whole-label mirroring once, after composition.
                    crate::preview::RenderedPreview::from_rendered(
                        ctx,
                        rendered.mirrored(self.state.overall_flip_h, self.state.overall_flip_v),
                        resolution,
                    )
                }),
            Err(error) => {
                error!("Render failed: {error}");
                self.state.status_message = format!("Render error: {error}");
                None
            }
        };
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
                        self.state.print_quality = ptouch_core::protocol::PrintQuality::Standard;
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

impl eframe::App for PtouchApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_printer_responses();
        crate::editor::begin_frame(&ctx, &mut self.state);

        // Top toolbar
        egui::Panel::top("toolbar").show(ui, |ui| {
            panels::toolbar::show_toolbar(ui, &mut self.state);
        });

        // Bottom status bar
        egui::Panel::bottom("status_bar").show(ui, |ui| {
            panels::status_bar::show_status_bar(ui, &self.state);
        });

        // Left sidebar
        egui::Panel::left("sidebar")
            .default_size(200.0)
            .resizable(true)
            .show(ui, |ui| {
                panels::sidebar::show_sidebar(ui, &mut self.state);
            });

        // Right properties panel
        egui::Panel::right("properties")
            .default_size(250.0)
            .resizable(true)
            .show(ui, |ui| {
                panels::properties::show_properties(ui, &mut self.state);
            });

        if self.state.needs_rerender {
            self.update_preview(&ctx);
        }

        // Central canvas
        egui::CentralPanel::default().show(ui, |ui| {
            let _ = panels::canvas::show_canvas(ui, &mut self.state);
        });

        crate::profile_ui::show(&ctx, &mut self.state);
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
            if *frames >= 3 {
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
                );
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
}
