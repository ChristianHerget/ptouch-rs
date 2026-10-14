// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Label preview, physical dimensions, and direct element interaction.

use crate::preview::{DisplayScale, ElementDrag, PreviewGeometry, ZoomMode, edge_pan, format_mm};
use crate::state::AppState;
use egui::{Align2, Color32, FontId, PointerButton, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2};

const BACKGROUND: Color32 = Color32::from_gray(200);

pub fn show_canvas(ui: &mut egui::Ui, state: &mut AppState) -> Option<PreviewGeometry> {
    let ctx = ui.ctx().clone();
    let mut step = 1.0;
    let mut readout = None;
    ui.add_enabled_ui(state.preview.is_some(), |ui| {
        ui.horizontal_wrapped(|ui| {
            for (name, mode, help) in [
                ("Fit", ZoomMode::Fit, "Fit the complete tape and its dimensions."),
                ("Actual size", ZoomMode::ActualSize,
                 "Approximate physical size based on OS display scaling. Accuracy depends on monitor settings."),
                ("Pixels", ZoomMode::Pixels,
                 "Inspect every printer dot at one or more physical screen pixels, preserving label proportions."),
            ] {
                if ui.selectable_label(state.view.zoom == mode, name).on_hover_text(help).clicked() {
                    state.view.select(mode);
                }
            }
            if ui.button("−").clicked() { step /= 1.25; }
            readout = Some(ui.allocate_response(vec2(78.0, ui.spacing().interact_size.y), Sense::hover())
                .on_hover_text("Zoom relative to estimated physical size; monitor settings affect accuracy."));
            if ui.button("+").clicked() { step *= 1.25; }
        });
    });
    ui.separator();

    let canvas = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(canvas, Sense::click_and_drag());
    state.editor.surface(&response);
    if response.clicked() || response.secondary_clicked() {
        response.request_focus();
    }
    let painter = ui.painter().with_clip_rect(canvas);
    painter.rect_filled(canvas, 0.0, BACKGROUND);
    // Keep annotation space outside the tape's drawing and interaction area.
    let content = Rect::from_min_max(
        canvas.min + vec2(12.0, 12.0),
        pos2(
            (canvas.right() - 72.0).max(canvas.left() + 13.0),
            (canvas.bottom() - 62.0).max(canvas.top() + 13.0),
        ),
    );
    let display = DisplayScale::from_context(&ctx);
    let active_drag = state.element_drag(&ctx);
    let Some(preview) = state.preview.as_ref() else {
        let menu = crate::editor::context_menu(&response, state, None);
        if let Some(action) = menu.action {
            crate::editor::apply_action(&ctx, state, action);
        }
        painter.text(
            canvas.center(),
            Align2::CENTER_CENTER,
            "Add elements to preview",
            FontId::proportional(18.0),
            Color32::from_gray(100),
        );
        return None;
    };
    let mut geometry = state
        .view
        .geometry(preview, state.tape_width_mm, content, display);
    if step != 1.0 {
        state.view.zoom = ZoomMode::Custom((geometry.physical_zoom * step).clamp(0.0001, 10_000.0));
        geometry = state
            .view
            .geometry(preview, state.tape_width_mm, content, display);
    }
    if let Some(readout) = readout {
        ui.painter().text(
            readout.rect.center(),
            Align2::CENTER_CENTER,
            crate::preview::format_zoom(geometry.physical_zoom),
            FontId::proportional(13.0),
            ui.visuals().text_color(),
        );
    }

    let pointer = ctx.pointer_interact_pos();
    let space = ctx.input(|i| i.key_down(egui::Key::Space));
    let pan_id = response.id.with("pan");
    let mut panning = ctx.data(|data| data.get_temp::<bool>(pan_id).unwrap_or(false));
    let mut selection = None;
    let mut reorder = None;
    if response.clicked_by(PointerButton::Primary) && !space {
        selection = Some(pointer.and_then(|p| geometry.hit_test(preview, p)));
    }
    if response.drag_started_by(PointerButton::Primary) {
        let origin = ctx.input(|i| i.pointer.press_origin()).or(pointer);
        let hit = origin.and_then(|p| geometry.hit_test(preview, p));
        if space || hit.is_none() {
            panning = true;
        } else if let Some(index) = hit {
            selection = Some(Some(index));
            egui::DragAndDrop::set_payload(
                &ctx,
                ElementDrag {
                    index,
                    revision: state.document_revision,
                },
            );
        }
    }
    let middle_pressed = ctx.input(|i| i.pointer.button_pressed(PointerButton::Middle));
    if middle_pressed && response.contains_pointer() && pointer.is_some_and(|p| content.contains(p))
    {
        panning = true;
    }
    if panning {
        let held =
            ctx.input(|i| i.pointer.primary_down() || i.pointer.button_down(PointerButton::Middle));
        if held {
            if !middle_pressed {
                state.view.pan += ctx.input(|i| i.pointer.delta());
            }
            geometry = state
                .view
                .geometry(preview, state.tape_width_mm, content, display);
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        } else {
            panning = false;
        }
    }
    ctx.data_mut(|data| data.insert_temp(pan_id, panning));

    let drag = active_drag.or_else(|| egui::DragAndDrop::payload::<ElementDrag>(&ctx).map(|d| *d));
    if let (Some(drag), Some(pointer)) = (drag, pointer)
        && content.contains(pointer)
        && response.contains_pointer()
    {
        let delta = edge_pan(pointer, content, ctx.input(|i| i.stable_dt));
        if delta != Vec2::ZERO {
            state.view.pan += delta;
            geometry = state
                .view
                .geometry(preview, state.tape_width_mm, content, display);
            ctx.request_repaint();
        }
        if ctx.input(|i| i.pointer.primary_released()) {
            reorder = Some((
                drag.index,
                preview.drop_position(geometry.column_at(pointer.x), state.overall_flip_h),
            ));
            egui::DragAndDrop::clear_payload(&ctx);
        }
    }

    let menu_target = pointer
        .filter(|_| response.contains_pointer())
        .and_then(|p| geometry.hit_test(preview, p));
    let menu = crate::editor::context_menu(&response, state, menu_target);
    if let Some(index) = menu.selection {
        selection = Some(Some(index));
    }
    let clipped = painter.with_clip_rect(content);
    clipped.rect_filled(geometry.tape, 0.0, Color32::WHITE);
    let print_painter = clipped.with_clip_rect(content.intersect(geometry.tape));
    for tile in &preview.tiles {
        let rect = preview.tile_rect(tile, geometry.print, geometry.head_scale);
        if rect.intersects(content) {
            print_painter.image(
                tile.texture.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    }
    for y in [geometry.print.top(), geometry.print.bottom()] {
        let mut x = geometry.tape.left().max(content.left());
        let end = geometry.tape.right().min(content.right());
        while x < end {
            clipped.line_segment(
                [pos2(x, y), pos2((x + 4.0).min(end), y)],
                Stroke::new(1.0, Color32::from_gray(165)),
            );
            x += 8.0;
        }
    }
    clipped.rect_stroke(
        geometry.tape,
        0.0,
        Stroke::new(1.0, Color32::from_gray(110)),
        egui::StrokeKind::Outside,
    );

    let hovered = pointer
        .filter(|_| response.contains_pointer())
        .and_then(|p| geometry.hit_test(preview, p));
    let selected = selection.unwrap_or(state.selected_element);
    let accent = Color32::from_rgb(35, 105, 200);
    if let Some(rect) = selected.and_then(|i| geometry.element_rect(preview, i)) {
        clipped.rect_filled(rect, 0.0, accent.gamma_multiply(0.09));
        clipped.rect_stroke(
            rect.shrink(0.75),
            0.0,
            Stroke::new(1.5, accent),
            egui::StrokeKind::Inside,
        );
    }
    if drag.is_none() && !panning {
        if let Some(index) = hovered {
            if selected != Some(index)
                && let Some(rect) = geometry.element_rect(preview, index)
            {
                clipped.rect_stroke(
                    rect,
                    0.0,
                    Stroke::new(1.0, accent),
                    egui::StrokeKind::Inside,
                );
            }
            ctx.set_cursor_icon(if space {
                egui::CursorIcon::Grab
            } else {
                egui::CursorIcon::PointingHand
            });
            response.clone().on_hover_ui_at_pointer(|ui| {
                if let Some(element) = state.elements.get(index) {
                    ui.label(element.display_name());
                }
                if let Some(length) = preview.element_length_mm(index) {
                    ui.label(format!("Length: {}", format_mm(length)));
                }
            });
        } else if response.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Grab);
        }
    }
    if drag.is_some()
        && let Some(pointer) = pointer
        && content.contains(pointer)
        && response.contains_pointer()
    {
        let before = preview.drop_position(geometry.column_at(pointer.x), state.overall_flip_h);
        let x = geometry.column_x(preview.insertion_column(before, state.overall_flip_h));
        clipped.line_segment(
            [
                pos2(x, geometry.tape.top()),
                pos2(x, geometry.tape.bottom()),
            ],
            Stroke::new(3.0, accent),
        );
    }

    draw_dimensions(
        ui,
        canvas,
        &geometry,
        preview
            .resolution
            .length_mm(preview.bitmap.width() as usize),
        state.tape_width_mm,
    );
    painter.text(
        pos2(canvas.left() + 6.0, canvas.bottom() - 3.0),
        Align2::LEFT_BOTTOM,
        "Drag elements to reorder · Space+drag or middle-drag to pan",
        FontId::proportional(11.0),
        Color32::from_gray(75),
    );
    if let Some(selection) = selection
        && state.selected_element != selection
    {
        state.select_element(selection);
        ctx.request_repaint();
    }
    if let Some((from, before)) = reorder {
        crate::editor::apply_action(
            &ctx,
            state,
            crate::editor::EditorAction::Move { from, before },
        );
    }
    if let Some(action) = menu.action {
        crate::editor::apply_action(&ctx, state, action);
    }
    Some(geometry)
}

fn dimension_end(painter: &egui::Painter, point: Pos2, direction: Vec2, clipped: bool) {
    let stroke = Stroke::new(1.0, Color32::from_gray(65));
    let perpendicular = vec2(-direction.y, direction.x);
    if clipped {
        for offset in [-2.0, 2.0] {
            let centre = point + direction * offset;
            painter.line_segment(
                [
                    centre - perpendicular * 3.0 - direction * 2.0,
                    centre + perpendicular * 3.0 + direction * 2.0,
                ],
                stroke,
            );
        }
    } else {
        painter.line_segment(
            [point - perpendicular * 5.0, point + perpendicular * 5.0],
            stroke,
        );
        for side in [-1.0, 1.0] {
            painter.line_segment(
                [
                    point,
                    point + direction * 5.0 + perpendicular * (side * 3.0),
                ],
                stroke,
            );
        }
    }
}

fn draw_dimensions(
    ui: &mut egui::Ui,
    canvas: Rect,
    geometry: &PreviewGeometry,
    length_mm: f64,
    tape_mm: u8,
) {
    let painter = ui.painter().with_clip_rect(canvas);
    let stroke = Stroke::new(1.0, Color32::from_gray(65));
    let left = geometry.tape.left().max(geometry.content.left());
    let right = geometry.tape.right().min(geometry.content.right());
    let top = geometry.tape.top().max(geometry.content.top());
    let bottom = geometry.tape.bottom().min(geometry.content.bottom());
    let y = (bottom + 16.0).min(canvas.bottom() - 40.0);
    painter.line_segment([pos2(left, y), pos2(right, y)], stroke);
    dimension_end(
        &painter,
        pos2(left, y),
        Vec2::X,
        geometry.tape.left() < geometry.content.left(),
    );
    dimension_end(
        &painter,
        pos2(right, y),
        -Vec2::X,
        geometry.tape.right() > geometry.content.right(),
    );
    let text = painter.layout_no_wrap(
        format_mm(length_mm),
        FontId::proportional(12.0),
        stroke.color,
    );
    let text_y = if right - left >= text.size().x + 24.0 {
        y
    } else {
        y + 16.0
    };
    let text_rect = Rect::from_center_size(pos2((left + right) * 0.5, text_y), text.size());
    painter.rect_filled(text_rect.expand2(vec2(4.0, 1.0)), 0.0, BACKGROUND);
    painter.galley(text_rect.min, text, stroke.color);
    ui.interact(
        text_rect.expand(5.0),
        ui.id().with("length_dimension"),
        Sense::hover(),
    )
    .on_hover_text("Preview length; excludes margins added by the printer.");

    let x = (right + 17.0).min(canvas.right() - 52.0);
    painter.line_segment([pos2(x, top), pos2(x, bottom)], stroke);
    dimension_end(
        &painter,
        pos2(x, top),
        Vec2::Y,
        geometry.tape.top() < geometry.content.top(),
    );
    dimension_end(
        &painter,
        pos2(x, bottom),
        -Vec2::Y,
        geometry.tape.bottom() > geometry.content.bottom(),
    );
    let label = painter.text(
        pos2(x + 8.0, (top + bottom) * 0.5),
        Align2::LEFT_CENTER,
        format_mm(f64::from(tape_mm)),
        FontId::proportional(12.0),
        stroke.color,
    );
    ui.interact(
        label.expand(5.0),
        ui.id().with("tape_dimension"),
        Sense::hover(),
    )
    .on_hover_text("Nominal tape width. Dashed lines mark the printable area.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::LabelElement;

    fn frame(
        ctx: &egui::Context,
        state: &mut AppState,
        events: Vec<egui::Event>,
    ) -> PreviewGeometry {
        let mut geometry = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 500.0))),
                events,
                ..Default::default()
            },
            |ui| {
                geometry = egui::CentralPanel::default()
                    .show(ui, |ui| show_canvas(ui, state))
                    .inner;
            },
        );
        output.textures_delta.clear();
        geometry.unwrap()
    }

    fn button(pos: Pos2, pressed: bool, button: PointerButton) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn fixture() -> (egui::Context, AppState) {
        let ctx = egui::Context::default();
        let elements = vec![
            LabelElement::Padding { pixels: 90 },
            LabelElement::CutMark,
            LabelElement::Padding { pixels: 180 },
        ];
        let resolution = ptouch_core::PrintResolution::square(180);
        let rendered = ptouch_render::document::render_elements_with_layout_at_resolution(
            &elements,
            70,
            "sans-serif",
            0,
            &mut ptouch_render::text::TextRenderer::new(),
            resolution,
        )
        .unwrap()
        .unwrap();
        let state = AppState {
            elements,
            preview: Some(crate::preview::RenderedPreview::from_rendered(
                &ctx, rendered, resolution,
            )),
            needs_rerender: false,
            ..Default::default()
        };
        (ctx, state)
    }

    #[test]
    fn preview_click_selects_blank_padding_and_drag_reorders_only_on_release() {
        let (ctx, mut state) = fixture();
        frame(&ctx, &mut state, vec![]);
        let geometry = frame(&ctx, &mut state, vec![]);
        let source = geometry
            .element_rect(state.preview.as_ref().unwrap(), 0)
            .unwrap()
            .center();
        frame(
            &ctx,
            &mut state,
            vec![
                egui::Event::PointerMoved(source),
                button(source, true, PointerButton::Primary),
            ],
        );
        frame(
            &ctx,
            &mut state,
            vec![button(source, false, PointerButton::Primary)],
        );
        assert_eq!(state.selected_element, Some(0));
        let target = pos2(geometry.content.right() - 2.0, geometry.tape.center().y);
        frame(
            &ctx,
            &mut state,
            vec![button(source, true, PointerButton::Primary)],
        );
        frame(&ctx, &mut state, vec![egui::Event::PointerMoved(target)]);
        assert!(matches!(
            state.elements[0],
            LabelElement::Padding { pixels: 90 }
        ));
        assert!(state.element_drag(&ctx).is_some());
        frame(
            &ctx,
            &mut state,
            vec![button(target, false, PointerButton::Primary)],
        );
        assert!(matches!(state.elements[0], LabelElement::CutMark));
        assert!(matches!(
            state.elements[2],
            LabelElement::Padding { pixels: 90 }
        ));
        assert_eq!(state.selected_element, Some(2));
        assert!(state.needs_rerender);
    }

    #[test]
    fn mirrored_preview_drops_follow_visual_insertion_positions() {
        let (ctx, mut state) = fixture();
        state.overall_flip_h = true;
        let preview = state.preview.as_mut().unwrap();
        preview.bitmap = preview.bitmap.mirrored(true, false);
        let width = preview.bitmap.width();
        for span in preview.element_spans.iter_mut().flatten() {
            *span = width - span.end..width - span.start;
        }
        frame(&ctx, &mut state, vec![]);
        let geometry = frame(&ctx, &mut state, vec![]);
        let source = geometry
            .element_rect(state.preview.as_ref().unwrap(), 0)
            .unwrap()
            .center();
        let target = pos2(geometry.content.left() + 1.0, source.y);
        frame(
            &ctx,
            &mut state,
            vec![
                egui::Event::PointerMoved(source),
                button(source, true, PointerButton::Primary),
            ],
        );
        frame(&ctx, &mut state, vec![egui::Event::PointerMoved(target)]);
        frame(
            &ctx,
            &mut state,
            vec![button(target, false, PointerButton::Primary)],
        );
        assert!(matches!(
            state.elements[2],
            LabelElement::Padding { pixels: 90 }
        ));
        assert_eq!(state.selected_element, Some(2));
    }

    #[test]
    fn space_drag_over_elements_pans_without_starting_reordering() {
        let (ctx, mut state) = fixture();
        state.view.zoom = ZoomMode::Custom(10.0);
        frame(&ctx, &mut state, vec![]);
        let source = frame(&ctx, &mut state, vec![]).content.center();
        frame(
            &ctx,
            &mut state,
            vec![
                egui::Event::PointerMoved(source),
                egui::Event::Key {
                    key: egui::Key::Space,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
                button(source, true, PointerButton::Primary),
            ],
        );
        let target = source + vec2(30.0, 20.0);
        frame(&ctx, &mut state, vec![egui::Event::PointerMoved(target)]);
        frame(
            &ctx,
            &mut state,
            vec![button(target, false, PointerButton::Primary)],
        );
        assert!(state.view.pan.x > 0.0 && state.view.pan.y > 0.0);
        assert!(state.element_drag(&ctx).is_none());
        assert_eq!(state.document_revision, 0);
        assert_eq!(state.selected_element, None);
    }

    #[test]
    fn dropping_outside_the_preview_cancels_the_move() {
        let (ctx, mut state) = fixture();
        frame(&ctx, &mut state, vec![]);
        let geometry = frame(&ctx, &mut state, vec![]);
        let source = geometry
            .element_rect(state.preview.as_ref().unwrap(), 0)
            .unwrap()
            .center();
        frame(
            &ctx,
            &mut state,
            vec![
                egui::Event::PointerMoved(source),
                button(source, true, PointerButton::Primary),
            ],
        );
        let outside = pos2(-10.0, -10.0);
        frame(&ctx, &mut state, vec![egui::Event::PointerMoved(outside)]);
        frame(
            &ctx,
            &mut state,
            vec![button(outside, false, PointerButton::Primary)],
        );
        assert_eq!(state.document_revision, 0);
        assert!(state.element_drag(&ctx).is_none());
        assert!(matches!(
            state.elements[0],
            LabelElement::Padding { pixels: 90 }
        ));
    }

    #[test]
    fn escape_cancels_reordering_and_middle_drag_pans_without_editing() {
        let (ctx, mut state) = fixture();
        state.view.zoom = ZoomMode::Custom(10.0);
        frame(&ctx, &mut state, vec![]);
        let geometry = frame(&ctx, &mut state, vec![]);
        let source = geometry.content.center();
        frame(
            &ctx,
            &mut state,
            vec![
                egui::Event::PointerMoved(source),
                button(source, true, PointerButton::Primary),
            ],
        );
        let target = source + vec2(50.0, 0.0);
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
            vec![button(target, false, PointerButton::Primary)],
        );
        assert_eq!(state.document_revision, 0);
        assert!(state.element_drag(&ctx).is_none());
        frame(
            &ctx,
            &mut state,
            vec![button(source, true, PointerButton::Middle)],
        );
        frame(
            &ctx,
            &mut state,
            vec![egui::Event::PointerMoved(source + vec2(30.0, 20.0))],
        );
        frame(
            &ctx,
            &mut state,
            vec![button(
                source + vec2(30.0, 20.0),
                false,
                PointerButton::Middle,
            )],
        );
        assert!(state.view.pan.x > 0.0 && state.view.pan.y > 0.0);
        assert_eq!(state.document_revision, 0);
        assert!(state.element_drag(&ctx).is_none());
    }
}
