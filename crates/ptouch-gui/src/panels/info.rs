// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Version and short, task-oriented help without touching the document.

pub fn show(ctx: &egui::Context, open: &mut bool) {
    egui::Window::new("Info")
        .open(open)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .collapsible(false)
        .resizable(false)
        .default_width(440.0)
        .show(ctx, |ui| {
            ui.heading("P-Touch Label Printer");
            ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
            ui.separator();
            for tip in [
                "Select an object on the canvas or in Elements.",
                "Drag to move; use the square handle to scale and the round handle to rotate.",
                "Free positioning keeps other objects in place. In sequence, allocation width and gaps place the following objects.",
                "Adjust layout changes the allocation. Gap before is empty space outside it. Align within allocation positions the content inside it.",
                "For a centred spacer line: set allocation width to 2.8 mm, Gap before to 0, then centre the content horizontally.",
                "Layout order moves a host with its attachments. Layers changes only which objects cover others.",
                "Absolute sequence starts keep their X position. Use After previous and set Gap before to join a continuous sequence.",
                "Undo: Ctrl+Z. Redo: Ctrl+Shift+Z or Ctrl+Y (Cmd on macOS).",
            ] {
                ui.label(tip);
                ui.add_space(4.0);
            }
            ui.hyperlink_to("Full guide", "https://github.com/vowstar/ptouch-rs#gui");
        });
}
