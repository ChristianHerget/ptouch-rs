// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Application state for the P-Touch GUI.

use std::sync::mpsc;

use crate::preview::RenderedPreview;
use ptouch_core::protocol::PrintQuality;

pub use ptouch_render::document::LabelElement;

/// Printer connection selected in the GUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrinterTarget {
    Unavailable(String),
    Profile(ptouch_profiles::PrinterProfile),
    TemporaryProfile(ptouch_profiles::PrinterProfile),
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
    pub fn profile(&self) -> Option<&ptouch_profiles::PrinterProfile> {
        match self {
            Self::Profile(p) | Self::TemporaryProfile(p) => Some(p),
            _ => None,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Unavailable(_) => "Profile configuration error".into(),
            Self::Profile(p) => format!("{} ({})", p.alias, p.model),
            Self::TemporaryProfile(p) => format!("{} ({}, temporary)", p.alias, p.model),
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

    pub fn is_network(&self) -> bool {
        self.profile()
            .is_some_and(|p| matches!(p.connection, ptouch_profiles::Connection::Network { .. }))
    }

    pub fn is_bluetooth(&self) -> bool {
        match self {
            Self::Unavailable(_)
            | Self::Profile(_)
            | Self::TemporaryProfile(_)
            | Self::Usb
            | Self::UsbAt(_) => false,
            #[cfg(windows)]
            Self::UsbPrint { .. } => false,
            #[cfg(any(target_os = "macos", test))]
            Self::Bluetooth { .. } => true,
        }
    }
}

/// A selection revision makes delayed responses stale even after A → B → A.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrinterSelection {
    pub target: PrinterTarget,
    pub generation: u64,
}
impl std::ops::Deref for PrinterSelection {
    type Target = PrinterTarget;
    fn deref(&self) -> &PrinterTarget {
        &self.target
    }
}

/// Commands sent from the UI thread to the printer worker.
pub enum PrinterCommand {
    /// Find paired Bluetooth printers supported by this application.
    DiscoverBluetooth,
    DiscoverUsbProfiles,
    /// Poll for a connected printer (query status only, no init).
    Poll(PrinterSelection),
    /// Print raster data.
    Print {
        raster_lines: Vec<Vec<u8>>,
        resolution: ptouch_core::PrintResolution,
        expected_media: (u8, u8),
        job_timeout_secs: u32,
        chain_print: bool,
        auto_cut: bool,
        precut: bool,
        quality: PrintQuality,
        target: PrinterSelection,
        cancellation: ptouch_core::CancellationToken,
    },
    /// Feed tape forward and cut.
    FeedAndCut {
        target: PrinterSelection,
        cancellation: ptouch_core::CancellationToken,
    },
}

/// A worker response and the printer it belongs to. Discovery is global.
pub struct PrinterEvent {
    pub target: Option<PrinterSelection>,
    pub response: PrinterResponse,
}

/// Responses sent from the printer worker back to the UI thread.
pub enum PrinterResponse {
    /// Paired PT-P300BT printers found by macOS.
    BluetoothDevices(Vec<PrinterTarget>),
    /// Detected USB locations for explicit selection.
    UsbDevices(Vec<PrinterTarget>),
    UsbProfiles(Result<Vec<ptouch_core::usb_profiles::UsbCandidate>, String>),
    /// A printer was found and its status queried.
    Connected {
        model_name: String,
        media_width: u8,
        media_type: String,
        max_px: u16,
        dpi: u16,
        media_code: u8,
        band_left: Option<u16>,
        tape_width_px: u16,
    },
    /// No printer found or previously connected printer lost.
    ConnectionError(String),
    /// Print job completed successfully.
    PrintDone,
    SubmittedUnconfirmed,
    /// Feed and cut completed successfully.
    FeedAndCutDone,
    /// An operation failed.
    Error(String),
}

/// Central application state shared across all panels.
pub struct AppState {
    pub editor: crate::editor::Editor,
    pub profiles: crate::profile_ui::ProfilesUi,
    pub printer_model_id: String,
    pub media_code: u8,
    pub job_timeout_secs: u32,
    pub band_left: Option<u16>,
    /// List of label elements in composition order.
    pub elements: Vec<LabelElement>,
    /// Index of the currently selected element, if any.
    pub selected_element: Option<usize>,
    /// Current tape width in millimeters.
    pub tape_width_mm: u8,
    /// Current tape width in pixels (derived from tape_width_mm).
    pub tape_width_px: u32,
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
    /// Print pixels, their resolution, and display textures from one render.
    pub preview: Option<RenderedPreview>,
    /// Flag indicating the preview needs to be re-rendered.
    pub needs_rerender: bool,
    pub view: crate::preview::PreviewView,
    /// Invalidates drag payloads and cached element coordinates after edits.
    pub document_revision: u64,
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
    pub precut: bool,
    /// Whether a printer is currently connected (detected by background poll).
    pub printer_connected: bool,
    /// Target selected for polling and printing.
    pub printer_target: PrinterTarget,
    pub(crate) selection_generation: u64,
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
    pub printer_quality: Option<ptouch_core::quality::QualityProfile>,
    /// Selected print quality for the next print job.
    pub print_quality: PrintQuality,
    /// Channel sender for commands to the printer worker thread.
    pub printer_cmd_tx: Option<mpsc::Sender<PrinterCommand>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            editor: crate::editor::Editor::default(),
            profiles: crate::profile_ui::ProfilesUi::default(),
            printer_model_id: String::new(),
            media_code: 0,
            job_timeout_secs: 600,
            band_left: None,
            elements: Vec::new(),
            selected_element: None,
            tape_width_mm: 12,
            tape_width_px: 76,
            font_name: "DejaVuSans".to_string(),
            font_margin: 0,
            overall_flip_h: false,
            overall_flip_v: false,
            available_fonts: Vec::new(),
            preview: None,
            needs_rerender: true,
            view: crate::preview::PreviewView::default(),
            document_revision: 0,
            printer_status: None,
            printer_model: None,
            status_message: "Ready".to_string(),
            rotation_input: String::new(),
            font_search: String::new(),
            auto_cut: true,
            precut: true,
            printer_connected: false,
            printer_target: PrinterTarget::Usb,
            selection_generation: 0,
            bluetooth_targets: Vec::new(),
            operation_in_progress: false,
            cancellation: None,
            usb_targets: Vec::new(),
            connecting: false,
            printer_max_px: 0,
            printer_dpi: 180,
            printer_quality: None,
            print_quality: PrintQuality::Standard,
            printer_cmd_tx: None,
        }
    }
}

impl AppState {
    pub fn selection(&self) -> PrinterSelection {
        PrinterSelection {
            target: self.printer_target.clone(),
            generation: self.selection_generation,
        }
    }

    pub fn select_target(&mut self, target: PrinterTarget) {
        self.selection_generation = self.selection_generation.wrapping_add(1);
        self.printer_target = target;
        self.apply_target_preferences();
        self.printer_connected = false;
        self.printer_model = None;
        self.printer_model_id.clear();
        self.printer_quality = None;
        self.band_left = None;
        self.connecting = !matches!(self.printer_target, PrinterTarget::Unavailable(_));
        self.printer_status = Some(match &self.printer_target {
            PrinterTarget::Unavailable(reason) => reason.clone(),
            _ => "Connecting…".into(),
        });
        self.mark_dirty();
        if let Some(tx) = &self.printer_cmd_tx {
            let _ = tx.send(PrinterCommand::Poll(self.selection()));
        }
    }

    pub fn apply_target_preferences(&mut self) {
        self.auto_cut = !self.printer_target.is_bluetooth();
        self.print_quality = PrintQuality::Standard;
        if let Some(p) = self.printer_target.profile() {
            self.auto_cut = p.final_cut;
            self.print_quality = match p.quality {
                ptouch_profiles::Quality::Standard => PrintQuality::Standard,
                ptouch_profiles::Quality::High => PrintQuality::HighRes,
                ptouch_profiles::Quality::Draft => PrintQuality::Draft,
            };
        }
    }
    pub fn is_printer_busy(&self) -> bool {
        self.operation_in_progress || self.connecting
    }

    /// Update the tape width in pixels based on the current tape_width_mm
    /// and the connected printer's resolution.
    pub fn update_tape_pixels(&mut self) {
        if let Some(tape) = ptouch_core::tape::find_tape(self.tape_width_mm, self.printer_dpi) {
            let px = u32::from(tape.pixels);
            self.tape_width_px = if self.printer_max_px > 0 {
                px.min(u32::from(self.printer_max_px))
            } else {
                px
            };
        }
    }

    /// Mark the preview as needing re-render.
    pub fn mark_dirty(&mut self) {
        self.needs_rerender = true;
        self.document_revision = self.document_revision.wrapping_add(1);
    }

    /// Keep the editable angle buffer in sync when selection changes surfaces.
    pub fn select_element(&mut self, selection: Option<usize>) {
        if self.selected_element != selection {
            self.editor.history.finish_group();
            self.editor.clear_text_cursor();
        }
        self.selected_element = selection.filter(|&index| index < self.elements.len());
        self.rotation_input = match self.selected_element.map(|index| &self.elements[index]) {
            Some(LabelElement::Text { rotation, .. } | LabelElement::Image { rotation, .. }) => {
                rotation.to_string()
            }
            _ => String::new(),
        };
    }

    pub fn element_drag(&self, ctx: &egui::Context) -> Option<crate::preview::ElementDrag> {
        let drag = *egui::DragAndDrop::payload::<crate::preview::ElementDrag>(ctx)?;
        if drag.revision != self.document_revision || drag.index >= self.elements.len() {
            egui::DragAndDrop::clear_payload(ctx);
            return None;
        }
        Some(drag)
    }

    pub fn set_print_quality(&mut self, quality: PrintQuality) {
        if self.print_quality != quality {
            self.print_quality = quality;
            self.mark_dirty();
        }
    }

    pub fn preview_resolution(&self) -> ptouch_core::Result<ptouch_core::PrintResolution> {
        match self.printer_quality {
            Some(profile) => Ok(profile.mode(self.print_quality)?.resolution),
            None => Ok(ptouch_core::PrintResolution::square(self.printer_dpi)),
        }
    }

    /// Never print or export a bitmap belonging to an earlier selection.
    pub fn output_preview(&self) -> Option<&RenderedPreview> {
        self.preview.as_ref().filter(|preview| {
            !self.needs_rerender && self.preview_resolution().ok() == Some(preview.resolution)
        })
    }
}

#[cfg(test)]
mod editor_tests {
    use super::*;

    fn widths(state: &AppState) -> Vec<u32> {
        state
            .elements
            .iter()
            .map(|element| match element {
                LabelElement::Padding { pixels } => *pixels,
                _ => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn moves_use_original_list_boundaries_and_retain_selection() {
        let mut state = AppState {
            elements: [10, 20, 30, 40]
                .into_iter()
                .map(|pixels| LabelElement::Padding { pixels })
                .collect(),
            ..Default::default()
        };
        state.apply_editor_action(crate::editor::EditorAction::Move { from: 0, before: 4 });
        assert_eq!(widths(&state), [20, 30, 40, 10]);
        assert_eq!(state.selected_element, Some(3));
        assert_eq!(state.document_revision, 1);
        state.apply_editor_action(crate::editor::EditorAction::Move { from: 3, before: 0 });
        assert_eq!(widths(&state), [10, 20, 30, 40]);
        assert_eq!(state.selected_element, Some(0));
        state.apply_editor_action(crate::editor::EditorAction::Move { from: 1, before: 3 });
        assert_eq!(widths(&state), [10, 30, 20, 40]);
        assert_eq!(state.selected_element, Some(2));
        let revision = state.document_revision;
        for (from, before) in [(2, 2), (2, 3), (4, 0), (0, 5)] {
            state.apply_editor_action(crate::editor::EditorAction::Move { from, before });
        }
        assert_eq!(state.document_revision, revision);
        assert_eq!(widths(&state), [10, 30, 20, 40]);
    }

    #[test]
    fn selection_updates_properties_without_invalidating_print_pixels() {
        let mut state = AppState {
            elements: vec![LabelElement::Text {
                content: "Angle".into(),
                font_size: None,
                align: ptouch_render::text::TextAlign::Left,
                rotation: 37.5,
                flip_h: false,
                flip_v: false,
            }],
            ..Default::default()
        };
        state.select_element(Some(0));
        assert_eq!(state.rotation_input, "37.5");
        assert_eq!(state.document_revision, 0);
        state.select_element(None);
        assert!(state.rotation_input.is_empty());
    }

    #[test]
    fn editing_invalidates_drag_payloads() {
        let ctx = egui::Context::default();
        let mut state = AppState {
            elements: vec![LabelElement::CutMark],
            ..Default::default()
        };
        egui::DragAndDrop::set_payload(
            &ctx,
            crate::preview::ElementDrag {
                index: 0,
                revision: state.document_revision,
            },
        );
        assert!(state.element_drag(&ctx).is_some());
        state.mark_dirty();
        assert!(state.element_drag(&ctx).is_none());
        assert!(!egui::DragAndDrop::has_any_payload(&ctx));
    }
}
