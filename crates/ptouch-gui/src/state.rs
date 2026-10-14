// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Application state for the P-Touch GUI.

use std::sync::mpsc;

use ptouch_core::protocol::PrintQuality;
use ptouch_render::bitmap::LabelBitmap;
use ptouch_render::document::{
    DOCUMENT_VERSION, ElementBounds, LabelDocument, LabelObject, ObjectContent, PositionMode,
    TextBox,
};
use ptouch_render::text::{TextAlign, TextRenderer};

pub use ptouch_render::document::LabelElement;

/// Printer connection selected in the GUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrinterTarget {
    Usb,
    UsbAt(ptouch_core::transport::UsbLocation),
    #[cfg(windows)]
    UsbPrint {
        instance_id: String,
    },
    #[cfg(any(target_os = "macos", test))]
    Bluetooth {
        name: String,
        address: String,
    },
}

impl PrinterTarget {
    pub fn label(&self) -> String {
        match self {
            Self::Usb => "USB (automatic)".to_string(),
            Self::UsbAt(location) => format!("USB {location}"),
            #[cfg(windows)]
            Self::UsbPrint { instance_id } => format!("PT-P710BT ({instance_id})"),
            #[cfg(any(target_os = "macos", test))]
            Self::Bluetooth { name, address } => format!("{name} ({address})"),
        }
    }

    pub fn usb_location(&self) -> Option<ptouch_core::transport::UsbLocation> {
        match self {
            Self::UsbAt(location) => Some(*location),
            _ => None,
        }
    }

    pub fn is_bluetooth(&self) -> bool {
        match self {
            Self::Usb | Self::UsbAt(_) => false,
            #[cfg(windows)]
            Self::UsbPrint { .. } => false,
            #[cfg(any(target_os = "macos", test))]
            Self::Bluetooth { .. } => true,
        }
    }
}

/// Commands sent from the UI thread to the printer worker.
pub enum PrinterCommand {
    /// Find paired Bluetooth printers supported by this application.
    DiscoverBluetooth,
    /// Poll for a connected printer (query status only, no init).
    Poll(PrinterTarget),
    /// Print raster data.
    Print {
        raster_lines: Vec<Vec<u8>>,
        chain_print: bool,
        auto_cut: bool,
        quality: PrintQuality,
        target: PrinterTarget,
        cancellation: ptouch_core::CancellationToken,
    },
    /// Feed tape forward and cut.
    FeedAndCut {
        target: PrinterTarget,
        cancellation: ptouch_core::CancellationToken,
    },
}

/// A worker response and the printer it belongs to. Discovery is global.
pub struct PrinterEvent {
    pub target: Option<PrinterTarget>,
    pub response: PrinterResponse,
}

/// Responses sent from the printer worker back to the UI thread.
pub enum PrinterResponse {
    /// Paired PT-P300BT printers found by macOS.
    BluetoothDevices(Vec<PrinterTarget>),
    /// Detected USB locations for explicit selection.
    UsbDevices(Vec<PrinterTarget>),
    /// A printer was found and its status queried.
    Connected {
        model_name: String,
        media_width: u8,
        media_type: String,
        max_px: u16,
        dpi: u16,
        quality_modes: bool,
        tape_width_px: u16,
    },
    /// No printer found or previously connected printer lost.
    ConnectionError(String),
    /// Print job completed successfully.
    PrintDone,
    /// Feed and cut completed successfully.
    FeedAndCutDone,
    /// An operation failed.
    Error(String),
}

/// Central application state shared across all panels.
pub struct AppState {
    /// Preserve an opened legacy format until the user explicitly converts it.
    pub document_version: u32,
    /// List of label elements in composition order.
    pub elements: Vec<LabelElement>,
    /// Index of the currently selected element, if any.
    pub selected_element: Option<usize>,
    /// Current tape width in millimeters.
    pub tape_width_mm: u8,
    /// Current tape width in pixels (derived from tape_width_mm).
    pub tape_width_px: u32,
    /// Saved design resolution, used when working offline.
    pub design_dpi: u16,
    /// Keep the last printer geometry through a transient disconnect.
    pub preview_uses_printer: bool,
    /// Font name used for text rendering.
    pub font_name: String,
    /// Font top/bottom margin in pixels.
    pub font_margin: u32,
    /// Mirror the whole composed label left-right (horizontal).
    pub overall_flip_h: bool,
    /// Mirror the whole composed label top-bottom (vertical).
    pub overall_flip_v: bool,
    /// Cached list of available system font family names.
    pub available_fonts: Vec<String>,
    /// The rendered preview bitmap (1-bit).
    pub preview_bitmap: Option<LabelBitmap>,
    /// The preview texture uploaded to the GPU.
    pub preview_texture: Option<egui::TextureHandle>,
    /// Bounds in original print pixels, independent of preview downsampling.
    pub preview_bounds: Vec<ElementBounds>,
    pub preview_diagnostics: Vec<String>,
    pub show_bounds: bool,
    /// Flag indicating the preview needs to be re-rendered.
    pub needs_rerender: bool,
    /// Current zoom level (1.0 = 100%).
    pub zoom: f32,
    /// Whether zoom should auto-fit to the canvas.
    pub zoom_fit: bool,
    /// Printer connection status message.
    pub printer_status: Option<String>,
    /// Detected printer model name.
    pub printer_model: Option<String>,
    /// Status bar message for transient feedback.
    pub status_message: String,
    /// Buffer for manual rotation angle input in properties panel.
    pub rotation_input: String,
    /// Buffer for font search/filter in properties panel.
    pub font_search: String,
    /// Auto-cut after printing. When false, chain print mode (no cut).
    pub auto_cut: bool,
    /// Whether a printer is currently connected (detected by background poll).
    pub printer_connected: bool,
    /// Target selected for polling and printing.
    pub printer_target: PrinterTarget,
    /// Paired PT-P300BT targets discovered on macOS.
    pub bluetooth_targets: Vec<PrinterTarget>,
    /// Whether a printer operation (print, feed & cut) is in progress.
    pub operation_in_progress: bool,
    pub cancellation: Option<ptouch_core::CancellationToken>,
    pub usb_targets: Vec<PrinterTarget>,
    /// Whether the selected printer's status is being requested.
    pub connecting: bool,
    /// Maximum printable pixels of the last connected printer (0 initially).
    pub printer_max_px: u16,
    /// Print resolution of the last connected printer (180 initially).
    /// Kept across disconnects so the canvas does not resize on a
    /// transient USB glitch.
    pub printer_dpi: u16,
    /// Whether the last connected printer supports print quality modes.
    pub printer_quality_modes: bool,
    /// Selected print quality for the next print job.
    pub print_quality: PrintQuality,
    /// Channel sender for commands to the printer worker thread.
    pub printer_cmd_tx: Option<mpsc::Sender<PrinterCommand>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            document_version: DOCUMENT_VERSION,
            elements: Vec::new(),
            selected_element: None,
            tape_width_mm: 12,
            tape_width_px: 76,
            design_dpi: 180,
            preview_uses_printer: false,
            font_name: "DejaVuSans".to_string(),
            font_margin: 0,
            overall_flip_h: false,
            overall_flip_v: false,
            available_fonts: Vec::new(),
            preview_bitmap: None,
            preview_texture: None,
            preview_bounds: Vec::new(),
            preview_diagnostics: Vec::new(),
            show_bounds: true,
            needs_rerender: true,
            zoom: 1.0,
            zoom_fit: true,
            printer_status: None,
            printer_model: None,
            status_message: "Ready".to_string(),
            rotation_input: String::new(),
            font_search: String::new(),
            auto_cut: true,
            printer_connected: false,
            printer_target: PrinterTarget::Usb,
            bluetooth_targets: Vec::new(),
            operation_in_progress: false,
            cancellation: None,
            usb_targets: Vec::new(),
            connecting: false,
            printer_max_px: 0,
            printer_dpi: 180,
            printer_quality_modes: false,
            print_quality: PrintQuality::Standard,
            printer_cmd_tx: None,
        }
    }
}

impl AppState {
    pub fn target_dpi(&self) -> u16 {
        if self.printer_connected || self.preview_uses_printer {
            self.printer_dpi
        } else {
            self.design_dpi
        }
    }

    pub fn document(&self) -> LabelDocument {
        LabelDocument {
            version: self.document_version,
            tape_width_mm: self.tape_width_mm,
            dpi: self.design_dpi,
            font_name: self.font_name.clone(),
            font_margin: self.font_margin,
            flip_h: self.overall_flip_h,
            flip_v: self.overall_flip_v,
            elements: self.elements.clone(),
        }
    }

    pub fn is_free_layout(&self) -> bool {
        self.document_version >= 3
            && self
                .elements
                .iter()
                .all(|element| element.as_object().is_some())
    }

    /// Resolve a model rectangle before whole-label mirroring. Millimeter
    /// frames do not depend on the previous preview or raster rounding.
    pub fn element_rect_mm(&self, index: usize) -> Option<(f64, f64, f64, f64)> {
        let mut cursor = 0.0;
        for (i, element) in self.elements.iter().enumerate().take(index + 1) {
            let frame = match element {
                LabelElement::Object(object) => Some(&object.frame),
                LabelElement::Text { text_box, .. } => text_box.as_ref(),
                _ => None,
            };
            let rect = if let Some(frame) = frame {
                let x = match frame.position {
                    PositionMode::Absolute => frame.x_mm,
                    PositionMode::Relative => cursor + frame.gap_mm,
                };
                Some((x, frame.y_mm, frame.width_mm, frame.height_mm))
            } else {
                self.preview_bounds
                    .iter()
                    .find(|bound| bound.element_index == i)
                    .map(|bound| {
                        let x = if self.overall_flip_h {
                            self.preview_bitmap.as_ref().map_or(bound.x, |bitmap| {
                                bitmap.width().saturating_sub(bound.x + bound.width)
                            })
                        } else {
                            bound.x
                        };
                        let y = if self.overall_flip_v {
                            self.tape_width_px.saturating_sub(bound.y + bound.height)
                        } else {
                            bound.y
                        };
                        let mm = 25.4 / f64::from(self.target_dpi());
                        (
                            f64::from(x) * mm,
                            f64::from(y) * mm,
                            f64::from(bound.width) * mm,
                            f64::from(bound.height) * mm,
                        )
                    })
            };
            if let Some((x, _, width, _)) = rect {
                cursor = x + width;
            }
            if i == index {
                return rect;
            }
        }
        None
    }

    pub fn printable_height_mm(&self) -> f64 {
        f64::from(self.tape_width_px) * 25.4 / f64::from(self.target_dpi())
    }

    /// New objects are independent absolute frames, placed after the furthest
    /// existing right edge with a 3 mm gap. This also works before a rerender.
    pub fn default_object_frame(&self, width_mm: f64, height_mm: f64) -> TextBox {
        let right = (0..self.elements.len())
            .filter_map(|index| self.element_rect_mm(index))
            .map(|(x, _, width, _)| x + width)
            .reduce(f64::max);
        TextBox {
            width_mm,
            height_mm: height_mm.min(self.printable_height_mm()),
            x_mm: right.map_or(0.0, |right| right + 3.0),
            ..TextBox::default()
        }
    }

    fn add_element(&mut self, element: LabelElement) {
        self.elements.push(element);
        self.selected_element = Some(self.elements.len() - 1);
        self.mark_dirty();
    }

    pub fn add_text(&mut self) {
        let element = if self.is_free_layout() {
            LabelElement::Object(LabelObject::text(
                "Label".to_string(),
                self.default_object_frame(10.0, 8.0),
            ))
        } else {
            LabelElement::Text {
                content: "Label".to_string(),
                font_size: None,
                align: TextAlign::Left,
                rotation: 0.0,
                flip_h: false,
                flip_v: false,
                text_box: None,
            }
        };
        self.add_element(element);
    }

    pub fn add_image_bytes(&mut self, path: Option<std::path::PathBuf>, bytes: Vec<u8>) -> bool {
        let element = if self.is_free_layout() {
            let mut object = LabelObject::image_from_bytes(
                path,
                bytes,
                self.default_object_frame(10.0, self.printable_height_mm()),
            );
            let ObjectContent::Image {
                bitmap: Some(bitmap),
                ..
            } = &object.content
            else {
                return false;
            };
            // Fit the original aspect ratio inside a useful initial footprint.
            let aspect = f64::from(bitmap.width()) / f64::from(bitmap.height().max(1));
            let height = self.printable_height_mm().min(40.0 / aspect);
            object.frame.width_mm = (height * aspect).max(25.4 / f64::from(self.target_dpi()));
            object.frame.height_mm = height.max(25.4 / f64::from(self.target_dpi()));
            LabelElement::Object(object)
        } else {
            let element = LabelElement::image_from_bytes(path, bytes);
            if matches!(element, LabelElement::Image { bitmap: None, .. }) {
                return false;
            }
            element
        };
        self.add_element(element);
        true
    }

    pub fn add_cut_mark(&mut self) {
        let element = if self.is_free_layout() {
            LabelElement::Object(LabelObject {
                frame: self.default_object_frame(1.27, self.printable_height_mm()),
                rotation: 0.0,
                flip_h: false,
                flip_v: false,
                content: ObjectContent::CutMark,
            })
        } else {
            LabelElement::CutMark
        };
        self.add_element(element);
    }

    /// Conversion is explicit: opening or saving an older label never changes
    /// its flow layout. The shared renderer resolves positions for all callers.
    pub fn convert_to_free_layout(&mut self) -> ptouch_render::Result<()> {
        let mut document = self.document();
        document.convert_to_free_layout(
            self.tape_width_px,
            self.target_dpi(),
            &mut TextRenderer::new(),
        )?;
        self.document_version = document.version;
        self.elements = document.elements;
        self.mark_dirty();
        Ok(())
    }

    pub fn is_printer_busy(&self) -> bool {
        self.operation_in_progress || self.connecting
    }

    /// Update the tape width in pixels based on the current tape_width_mm
    /// and the connected printer's resolution.
    pub fn update_tape_pixels(&mut self) {
        if let Some(tape) = ptouch_core::tape::find_tape(self.tape_width_mm, self.target_dpi()) {
            let px = u32::from(tape.pixels);
            self.tape_width_px = if (self.printer_connected || self.preview_uses_printer)
                && self.printer_max_px > 0
            {
                px.min(u32::from(self.printer_max_px))
            } else {
                px
            };
        }
    }

    /// Mark the preview as needing re-render.
    pub fn mark_dirty(&mut self) {
        self.needs_rerender = true;
    }

    /// Ensure the selected element index is valid.
    pub fn validate_selection(&mut self) {
        if let Some(idx) = self.selected_element
            && idx >= self.elements.len()
        {
            self.selected_element = if self.elements.is_empty() {
                None
            } else {
                Some(self.elements.len() - 1)
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_objects_have_independent_frames_after_the_furthest_right_edge() {
        let mut state = AppState::default();
        assert_eq!(state.document_version, 3);
        state.add_text();
        state.add_text();
        let first = state.elements[0].as_object_mut().unwrap();
        first.frame.x_mm = 40.0;
        state.add_text();
        let positions: Vec<_> = state
            .elements
            .iter()
            .map(|element| {
                let frame = &element.as_object().unwrap().frame;
                assert_eq!(frame.position, PositionMode::Absolute);
                assert_eq!(
                    (frame.width_mm, frame.height_mm, frame.y_mm),
                    (10.0, 8.0, 0.0)
                );
                frame.x_mm
            })
            .collect();
        assert_eq!(positions, [40.0, 13.0, 53.0]);
        assert!(state.needs_rerender);
    }

    #[test]
    fn mixed_v3_documents_still_offer_explicit_conversion() {
        let mut state = AppState::default();
        state.add_text();
        state.elements.push(LabelElement::CutMark);
        assert!(!state.is_free_layout());
        state.convert_to_free_layout().unwrap();
        assert!(state.is_free_layout());
    }

    #[test]
    fn relative_frames_resolve_in_millimeters_without_preview_rounding() {
        let mut state = AppState::default();
        for _ in 0..300 {
            let frame = TextBox {
                position: PositionMode::Relative,
                gap_mm: 3.0,
                ..TextBox::default()
            };
            state
                .elements
                .push(LabelElement::Object(LabelObject::text("", frame)));
        }
        assert!(state.preview_bounds.is_empty());
        assert_eq!(state.element_rect_mm(299), Some((3890.0, 0.0, 10.0, 8.0)));
        assert_eq!(state.default_object_frame(10.0, 8.0).x_mm, 3903.0);
    }

    #[test]
    fn initial_image_frame_fits_tape_preserves_aspect_and_embeds_bytes() {
        use std::io::Cursor;
        let source = image::GrayImage::from_pixel(200, 100, image::Luma([0]));
        let mut bytes = Cursor::new(Vec::new());
        source
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let bytes = bytes.into_inner();
        let mut state = AppState::default();
        assert!(state.add_image_bytes(None, bytes.clone()));
        let object = state.elements[0].as_object().unwrap();
        assert!((object.frame.width_mm / object.frame.height_mm - 2.0).abs() < 1e-9);
        assert_eq!(object.frame.height_mm, state.printable_height_mm());
        let ObjectContent::Image {
            image_data, bitmap, ..
        } = &object.content
        else {
            panic!()
        };
        assert_eq!(*image_data, bytes);
        assert!(bitmap.is_some());
        let count = state.elements.len();
        assert!(!state.add_image_bytes(None, b"invalid image".to_vec()));
        assert_eq!(state.elements.len(), count);
    }

    #[test]
    fn every_new_object_fits_narrow_tape_height() {
        let mut state = AppState {
            tape_width_px: 32,
            tape_width_mm: 6,
            ..AppState::default()
        };
        state.add_text();
        state.add_cut_mark();
        for element in &state.elements {
            let frame = &element.as_object().unwrap().frame;
            assert!(frame.height_mm <= state.printable_height_mm());
            assert_eq!(frame.y_mm, 0.0);
        }
    }
}
