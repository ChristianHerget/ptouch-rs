// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Left sidebar panel: printer info, tape selection, and element list.

use log::info;

use ptouch_core::tape;

use crate::state::{AppState, PrinterCommand};

/// Render the left sidebar.
pub fn show_sidebar(ui: &mut egui::Ui, state: &mut AppState) {
    let mut scroll = egui::ScrollArea::vertical().show(ui, |ui| {
        show_printer_section(ui, state);
        ui.separator();
        show_tape_section(ui, state);
        ui.separator();
        show_print_options(ui, state);
        ui.separator();
        show_mirror_section(ui, state);
        ui.separator();
        show_elements_section(ui, state);
    });
    if state.element_drag(ui.ctx()).is_some()
        && let Some(pointer) = ui.ctx().pointer_interact_pos()
        && scroll.inner_rect.contains(pointer)
    {
        let delta = crate::preview::edge_pan(pointer, scroll.inner_rect, ui.input(|i| i.stable_dt));
        if delta.y != 0.0 {
            scroll.state.offset.y = (scroll.state.offset.y - delta.y).clamp(
                0.0,
                (scroll.content_size.y - scroll.inner_rect.height()).max(0.0),
            );
            scroll.state.store(ui.ctx(), scroll.id);
            ui.ctx().request_repaint();
        }
    }
}

/// Printer connection section.
fn show_printer_section(ui: &mut egui::Ui, state: &mut AppState) {
    ui.heading("Printer");
    ui.add_space(4.0);

    let old_target = state.printer_target.clone();
    ui.add_enabled_ui(!state.is_printer_busy(), |ui| {
        egui::ComboBox::from_label("Connection")
            .selected_text(state.printer_target.label())
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut state.printer_target,
                    crate::state::PrinterTarget::Usb,
                    "USB (automatic)",
                );
                if let Some(store) = &state.profiles.store {
                    for entry in store.entries.values() {
                        let target = crate::state::PrinterTarget::Profile(entry.profile.clone());
                        ui.selectable_value(
                            &mut state.printer_target,
                            target.clone(),
                            target.label(),
                        );
                    }
                }
                for target in state.usb_targets.iter().chain(&state.bluetooth_targets) {
                    ui.selectable_value(&mut state.printer_target, target.clone(), target.label());
                }
            });
    });
    if state.printer_target != old_target {
        state.select_target(state.printer_target.clone());
    }

    if ui
        .add_enabled(
            !state.is_printer_busy(),
            egui::Button::new("Manage printers…"),
        )
        .clicked()
    {
        state.profiles.open = true;
    }
    if let Some(error) = &state.profiles.error {
        ui.colored_label(egui::Color32::RED, error);
    }
    let model_text = state.printer_model.as_deref().unwrap_or("Not connected");
    ui.label(format!("Model: {}", model_text));

    let status_text = state.printer_status.as_deref().unwrap_or("Scanning...");
    ui.label(format!("Status: {}", status_text));

    ui.add_space(4.0);
    if ui
        .add_enabled(!state.is_printer_busy(), egui::Button::new("Refresh"))
        .clicked()
        && let Some(ref tx) = state.printer_cmd_tx
    {
        state.connecting = true;
        let _ = tx.send(PrinterCommand::Poll(state.selection()));
        let _ = tx.send(PrinterCommand::DiscoverBluetooth);
        info!("Manual printer refresh requested");
    }
}

/// Tape width selection section.
fn show_tape_section(ui: &mut egui::Ui, state: &mut AppState) {
    ui.heading("Tape");
    ui.add_space(4.0);

    let tapes = tape::supported_tapes(state.printer_dpi);
    let current_label = format!("{} mm ({} px)", state.tape_width_mm, state.tape_width_px);

    ui.add_enabled_ui(
        !state.printer_connected && !state.printer_target.is_bluetooth(),
        |ui| {
            egui::ComboBox::from_label("Width")
                .selected_text(&current_label)
                .show_ui(ui, |ui| {
                    for t in tapes {
                        let label = format!("{} mm ({} px)", t.width_mm, t.pixels);
                        if ui
                            .selectable_value(&mut state.tape_width_mm, t.width_mm, &label)
                            .clicked()
                        {
                            state.update_tape_pixels();
                            state.mark_dirty();
                            info!("Tape changed: {} mm", t.width_mm);
                        }
                    }
                });
        },
    );
}

/// Print options: auto-cut toggle and quality selection.
fn show_print_options(ui: &mut egui::Ui, state: &mut AppState) {
    ui.heading("Print Options");
    ui.horizontal(|ui| {
        ui.label("Job timeout (s)");
        ui.add(egui::DragValue::new(&mut state.job_timeout_secs).range(1..=86400));
    });
    ui.add_space(4.0);
    ui.add_enabled_ui(!state.printer_target.is_bluetooth(), |ui| {
        ui.horizontal(|ui| {
            let label = if state.auto_cut { "Auto cut" } else { "No cut" };
            ui.label(label);
            ui.add(crate::widgets::toggle(&mut state.auto_cut));
        })
    });
    if state.printer_target.is_bluetooth() {
        ui.label("Manual cutter");
    }

    if state.printer_target.is_network() {
        ui.checkbox(&mut state.precut, "Short margins (leading cut)");
    }

    if let Some(profile) = state.printer_quality {
        let mut quality = state.print_quality;
        let selected = profile.mode(state.print_quality).ok();
        egui::ComboBox::from_label("Quality")
            .selected_text(
                selected
                    .map(|m| m.label())
                    .unwrap_or_else(|| "Unsupported quality".into()),
            )
            .show_ui(ui, |ui| {
                for mode in profile.modes {
                    ui.selectable_value(&mut quality, mode.quality, mode.label())
                        .on_hover_text(format!(
                            "DPI: across the head × along the tape. {}",
                            mode.requirement()
                        ));
                }
            });
        state.set_print_quality(quality);
    }
}

/// Whole-label mirror section: flip the entire composed label.
///
/// This applies once after all elements are composed and is separate from the
/// per-element flip on the properties panel.
fn show_mirror_section(ui: &mut egui::Ui, state: &mut AppState) {
    ui.heading("Mirror");
    ui.add_space(4.0);
    let before = state.content_snapshot();
    let horizontal = ui
        .checkbox(&mut state.overall_flip_h, "Flip horizontal (left-right)")
        .changed();
    let vertical = ui
        .checkbox(&mut state.overall_flip_v, "Flip vertical (top-bottom)")
        .changed();
    if horizontal || vertical {
        state.record_content(before, "Mirror label", None, ui.input(|i| i.time));
    }
}

/// Element list section with reorder and delete controls.
fn show_elements_section(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        ui.heading("Elements");
        crate::editor::history_buttons(ui, state);
    });
    ui.add_space(4.0);
    if state.elements.is_empty() {
        ui.label("(no elements)");
        return;
    }
    let mut action = None;
    let mut rows = Vec::new();
    let mut selection = None;
    for idx in 0..state.elements.len() {
        let is_selected = state.selected_element == Some(idx);
        let label = format!("{}. {}", idx + 1, state.elements[idx].display_name());
        let row = ui.push_id(("element_row", idx), |ui| {
            ui.horizontal(|ui| {
                let handle = ui
                    .add(egui::Label::new("⠿").sense(egui::Sense::drag()))
                    .on_hover_cursor(egui::CursorIcon::Grab)
                    .on_hover_text("Drag to reorder; Alt+Up/Down moves the selected element");
                state.editor.surface(&handle);
                handle.dnd_set_drag_payload(crate::preview::ElementDrag {
                    index: idx,
                    revision: state.document_revision,
                });
                if handle.drag_started() {
                    handle.request_focus();
                    selection = Some(idx);
                }
                let width = (ui.available_width() - 30.0).max(1.0);
                let response = ui
                    .add_sized(
                        [width, ui.spacing().interact_size.y],
                        egui::Button::selectable(is_selected, &label).truncate(),
                    )
                    .on_hover_text(&label);
                state.editor.surface(&response);
                if response.clicked() {
                    response.request_focus();
                    selection = Some(idx);
                }
                let context = crate::editor::context_menu(&response, state, Some(idx));
                selection = context.selection.or(selection);
                action = context.action.or(action.take());
                let (menu_response, menu) = crate::editor::row_menu(ui, state, idx);
                state.editor.surface(&menu_response);
                selection = menu.selection.or(selection);
                action = menu.action.or(action.take());
            })
        });
        rows.push(row.inner.response.rect);
    }
    if let Some(index) = selection {
        state.select_element(Some(index));
        ui.ctx().request_repaint();
    }
    if let (Some(drag), Some(pointer), Some(first), Some(last)) = (
        state.element_drag(ui.ctx()),
        ui.ctx().pointer_interact_pos(),
        rows.first(),
        rows.last(),
    ) {
        let area = egui::Rect::from_min_max(
            egui::pos2(ui.max_rect().left(), first.top() - 4.0),
            egui::pos2(ui.max_rect().right(), last.bottom() + 8.0),
        )
        .intersect(ui.clip_rect());
        if ui.rect_contains_pointer(area) {
            let before = rows
                .iter()
                .position(|row| pointer.y < row.center().y)
                .unwrap_or(rows.len());
            let y = rows
                .get(before)
                .map_or(last.bottom() + 3.0, |row| row.top() - 2.0);
            ui.painter().line_segment(
                [egui::pos2(area.left(), y), egui::pos2(area.right(), y)],
                egui::Stroke::new(2.0, ui.visuals().selection.stroke.color),
            );
            if ui.input(|i| i.pointer.primary_released()) {
                action = Some(crate::editor::EditorAction::Move {
                    from: drag.index,
                    before,
                });
            }
        }
    }
    if let Some(action) = action {
        crate::editor::apply_action(ui.ctx(), state, action);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::LabelElement;

    fn frame(
        ctx: &egui::Context,
        state: &mut AppState,
        events: Vec<egui::Event>,
    ) -> Vec<egui::Pos2> {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(300.0, 400.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| show_elements_section(ui, state));
            },
        );
        let handles = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == "⠿" => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .collect();
        output.textures_delta.clear();
        handles
    }

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            pressed,
            button: egui::PointerButton::Primary,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn fixture() -> AppState {
        AppState {
            elements: [10, 20, 30]
                .into_iter()
                .map(|pixels| LabelElement::Padding { pixels })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn list_handles_move_elements_on_drop_and_keep_selection() {
        let ctx = egui::Context::default();
        let mut state = fixture();
        frame(&ctx, &mut state, vec![]);
        let handles = frame(&ctx, &mut state, vec![]);
        assert_eq!(handles.len(), 3);
        let source = handles[0];
        let target = handles[2] + egui::vec2(0.0, 7.0);
        frame(
            &ctx,
            &mut state,
            vec![egui::Event::PointerMoved(source), button(source, true)],
        );
        frame(&ctx, &mut state, vec![egui::Event::PointerMoved(target)]);
        assert!(state.element_drag(&ctx).is_some());
        assert_eq!(state.document_revision, 0);
        frame(&ctx, &mut state, vec![button(target, false)]);
        assert!(matches!(
            state.elements[2],
            LabelElement::Padding { pixels: 10 }
        ));
        assert_eq!(state.selected_element, Some(2));
        assert_eq!(state.document_revision, 1);
    }

    #[test]
    fn escape_does_not_restart_a_list_drag_on_the_next_frame() {
        let ctx = egui::Context::default();
        let mut state = fixture();
        frame(&ctx, &mut state, vec![]);
        let handles = frame(&ctx, &mut state, vec![]);
        let source = handles[0];
        let target = handles[2];
        frame(
            &ctx,
            &mut state,
            vec![egui::Event::PointerMoved(source), button(source, true)],
        );
        frame(&ctx, &mut state, vec![egui::Event::PointerMoved(target)]);
        assert!(state.element_drag(&ctx).is_some());
        frame(
            &ctx,
            &mut state,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        frame(
            &ctx,
            &mut state,
            vec![egui::Event::PointerMoved(target + egui::vec2(0.0, 5.0))],
        );
        assert!(state.element_drag(&ctx).is_none());
        frame(&ctx, &mut state, vec![button(target, false)]);
        assert_eq!(state.document_revision, 0);
        assert!(matches!(
            state.elements[0],
            LabelElement::Padding { pixels: 10 }
        ));
    }

    #[test]
    fn list_accepts_a_shared_payload_from_the_preview() {
        let ctx = egui::Context::default();
        let mut state = fixture();
        frame(&ctx, &mut state, vec![]);
        let handles = frame(&ctx, &mut state, vec![]);
        let target = handles[0] - egui::vec2(0.0, 4.0);
        frame(
            &ctx,
            &mut state,
            vec![egui::Event::PointerMoved(target), button(target, true)],
        );
        egui::DragAndDrop::set_payload(
            &ctx,
            crate::preview::ElementDrag {
                index: 2,
                revision: 0,
            },
        );
        frame(&ctx, &mut state, vec![button(target, false)]);
        assert!(matches!(
            state.elements[0],
            LabelElement::Padding { pixels: 30 }
        ));
        assert_eq!(state.selected_element, Some(0));
    }
}
