// SPDX-License-Identifier: MIT
//! Shared content actions, history, menus, and keyboard routing.

mod history;

use crate::state::{AppState, LabelElement};
use egui::{Id, Key, Modifiers, Response};
pub(crate) use history::EditGroup;
use history::{ContentHistory, ContentSelection, ContentSnapshot};
use std::collections::HashSet;

pub enum EditorAction {
    SelectCanvas(crate::state::CanvasTarget),
    Add(LabelElement),
    Duplicate(usize),
    Delete(usize),
    Move { from: usize, before: usize },
    Undo,
    Redo,
}

#[derive(Default)]
pub struct Editor {
    pub history: ContentHistory,
    surfaces: HashSet<Id>,
    content_inputs: HashSet<Id>,
    input_buffers: HashSet<Id>,
    preferred_focus: Option<Id>,
    focus_after_action: bool,
    text_cursor: Option<(Id, egui::text::CCursorRange)>,
    pub changed_group: Option<EditGroup>,
}

impl Editor {
    pub fn clear_text_cursor(&mut self) {
        self.text_cursor = None;
    }

    pub fn surface(&mut self, response: &Response) {
        self.surfaces.insert(response.id);
        self.preferred_focus = Some(response.id);
    }

    fn input(&mut self, response: &Response) {
        self.content_inputs.insert(response.id);
        self.input_buffers.insert(response.id);
        if response.has_focus()
            && let Some(range) = egui::TextEdit::load_state(&response.ctx, response.id)
                .and_then(|state| state.cursor.char_range())
        {
            self.text_cursor = Some((response.id, range));
        }
        if response.changed() {
            self.changed_group = if response.dragged() {
                Some(EditGroup::Drag(response.id))
            } else if response.has_focus() {
                Some(EditGroup::Typing(response.id))
            } else {
                None
            };
        }
    }
}

pub fn content_input(
    ui: &mut egui::Ui,
    editor: &mut Editor,
    widget: impl egui::Widget,
) -> Response {
    let response = ui.add(widget);
    editor.input(&response);
    response
}

impl AppState {
    pub(super) fn content_snapshot(&mut self) -> ContentSnapshot {
        self.editor.history.capture(
            &self.elements,
            &self.font_name,
            self.font_margin,
            (self.overall_flip_h, self.overall_flip_v),
            ContentSelection {
                element: self.selected_element,
                cursor: self.editor.text_cursor,
                target: self.canvas_edit_target,
            },
            (self.document_version, self.design_dpi),
        )
    }

    pub(super) fn record_content(
        &mut self,
        before: ContentSnapshot,
        name: &'static str,
        group: Option<EditGroup>,
        time: f64,
    ) {
        let after = self.content_snapshot();
        if self.editor.history.record(before, after, name, group, time) {
            self.mark_dirty();
        }
    }

    pub fn apply_editor_action(&mut self, action: EditorAction) -> bool {
        match action {
            EditorAction::SelectCanvas(target) => {
                self.select_canvas_target(Some(target));
                return true;
            }
            EditorAction::Undo | EditorAction::Redo => {
                let snapshot = if matches!(action, EditorAction::Undo) {
                    self.editor.history.undo()
                } else {
                    self.editor.history.redo()
                };
                let Some(snapshot) = snapshot else {
                    return false;
                };
                snapshot.restore(self);
                return true;
            }
            _ => {}
        }
        // Validate before capturing assets or ending an open typing group.
        match &action {
            EditorAction::Duplicate(index) | EditorAction::Delete(index)
                if *index >= self.elements.len() =>
            {
                return false;
            }
            EditorAction::Move { from, before }
                if *from >= self.elements.len()
                    || *before > self.elements.len()
                    || *before == *from
                    || *before == *from + 1 =>
            {
                return false;
            }
            _ => {}
        }
        let before = self.content_snapshot();
        let name = match action {
            EditorAction::Add(element) => {
                self.elements.push(element);
                self.select_element(Some(self.elements.len() - 1));
                "Add object"
            }
            EditorAction::Duplicate(index) => {
                let mut copy = self.elements[index].clone();
                let frame = match &mut copy {
                    LabelElement::Object(object) => Some(&mut object.frame),
                    LabelElement::Text { text_box, .. } => text_box.as_mut(),
                    _ => None,
                };
                if let Some(frame) = frame
                    && frame.position == ptouch_render::document::PositionMode::Absolute
                {
                    let Some((x, _, width, _)) = self.element_rect_mm(index) else {
                        return false;
                    };
                    frame.x_mm = x + width + 3.0;
                }
                let edit = if self.selected_element == Some(index) {
                    self.canvas_edit_target
                } else {
                    crate::state::CanvasEditTarget::Content
                };
                self.elements.insert(index + 1, copy);
                self.select_canvas_target(Some(crate::state::CanvasTarget {
                    element: index + 1,
                    edit,
                }));
                "Duplicate object"
            }
            EditorAction::Delete(index) => {
                self.elements.remove(index);
                let next = if self.elements.is_empty() {
                    None
                } else {
                    Some(index.min(self.elements.len() - 1))
                };
                self.select_element(next);
                "Delete object"
            }
            EditorAction::Move { from, before } => {
                let destination = if before > from { before - 1 } else { before };
                let element = self.elements.remove(from);
                self.elements.insert(destination, element);
                self.select_element(Some(destination));
                "Reorder object"
            }
            EditorAction::Undo | EditorAction::Redo | EditorAction::SelectCanvas(_) => {
                unreachable!()
            }
        };
        self.selection_revision = self.selection_revision.wrapping_add(1);
        self.canvas_hover_target = None;
        self.canvas_drag_target = None;
        let after = self.content_snapshot();
        self.editor.history.record_action(before, after, name);
        self.mark_dirty();
        true
    }
}

/// UI callers also discard stale edit buffers, drags, and open menus.
pub fn apply_action(ctx: &egui::Context, state: &mut AppState, action: EditorAction) {
    let history_action = matches!(action, EditorAction::Undo | EditorAction::Redo);
    let editing_content = ctx
        .memory(|memory| memory.focused())
        .is_some_and(|id| state.editor.content_inputs.contains(&id));
    if state.apply_editor_action(action) {
        if history_action {
            for &id in &state.editor.input_buffers {
                ctx.data_mut(|data| data.remove::<String>(id));
                egui::text_edit::TextEditState::default().store(ctx, id);
            }
            if let Some((id, range)) = state.editor.text_cursor {
                let mut input = egui::text_edit::TextEditState::default();
                input.cursor.set_char_range(Some(range));
                input.store(ctx, id);
            }
        }
        state.editor.focus_after_action = !history_action || !editing_content;
        egui::DragAndDrop::clear_payload(ctx);
        egui::Popup::close_all(ctx);
        ctx.request_repaint();
    }
}

fn consume(ctx: &egui::Context, modifiers: Modifiers, key: Key) -> bool {
    ctx.input_mut(|input| input.consume_key(modifiers, key))
}

fn shortcut(ctx: &egui::Context, modifiers: Modifiers, key: Key) -> String {
    ctx.format_shortcut(&egui::KeyboardShortcut::new(modifiers, key))
}

/// Intercept document undo before egui text inputs can consume it locally.
/// Auxiliary fields (font search, angle drafts, profiles) are not registered.
pub fn begin_frame(ctx: &egui::Context, state: &mut AppState) {
    let focus = ctx.memory(|memory| memory.focused());
    let auxiliary_input = focus.is_some_and(|id| {
        egui::TextEdit::load_state(ctx, id).is_some()
            && !state.editor.content_inputs.contains(&id)
            && !state.editor.surfaces.contains(&id)
    });
    let owns_focus = !auxiliary_input;
    if owns_focus
        && !state.profiles.open
        && !state.symbol_picker.is_open()
        && !egui::DragAndDrop::has_any_payload(ctx)
        && ctx.dragged_id().is_none()
        && !ctx.input(|input| input.pointer.any_pressed())
    {
        let action = if consume(ctx, Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::Z)
            || (!cfg!(target_os = "macos") && consume(ctx, Modifiers::CTRL, Key::Y))
        {
            Some(EditorAction::Redo)
        } else if consume(ctx, Modifiers::COMMAND, Key::Z) {
            Some(EditorAction::Undo)
        } else {
            None
        };
        if let Some(action) = action {
            apply_action(ctx, state, action);
        }
    }
    if !ctx.input(|input| input.pointer.any_pressed()) {
        structure_shortcuts(ctx, state);
    }
    state.editor.history.finish_inactive_group(
        focus,
        ctx.input(|input| input.pointer.primary_down()),
        ctx.input(|input| input.time),
    );
    state.editor.surfaces.clear();
    state.editor.content_inputs.clear();
    state.editor.changed_group = None;
}

fn structure_shortcuts(ctx: &egui::Context, state: &mut AppState) {
    let focus = ctx.memory(|memory| memory.focused());
    let owns_surface = focus.is_some_and(|id| state.editor.surfaces.contains(&id));
    if owns_surface
        && !state.profiles.open
        && !state.symbol_picker.is_open()
        && !egui::DragAndDrop::has_any_payload(ctx)
        && ctx.dragged_id().is_none()
        && !egui::Popup::is_any_open(ctx)
    {
        let selected = state.selected_element;
        let action = if consume(ctx, Modifiers::COMMAND, Key::D) {
            selected.map(EditorAction::Duplicate)
        } else if consume(ctx, Modifiers::NONE, Key::Delete)
            || consume(ctx, Modifiers::NONE, Key::Backspace)
        {
            selected.map(EditorAction::Delete)
        } else if consume(ctx, Modifiers::ALT, Key::ArrowUp) {
            selected
                .filter(|&index| index > 0)
                .map(|from| EditorAction::Move {
                    from,
                    before: from - 1,
                })
        } else if consume(ctx, Modifiers::ALT, Key::ArrowDown) {
            selected
                .filter(|&index| index + 1 < state.elements.len())
                .map(|from| EditorAction::Move {
                    from,
                    before: from + 2,
                })
        } else {
            None
        };
        if let Some(action) = action {
            apply_action(ctx, state, action);
        }
    }
}

pub fn end_frame(ctx: &egui::Context, state: &mut AppState) {
    if state.symbol_picker.is_open() {
        return;
    }
    structure_shortcuts(ctx, state);
    if state.editor.focus_after_action {
        if let Some(id) = state.editor.preferred_focus {
            ctx.memory_mut(|memory| memory.request_focus(id));
        }
        state.editor.focus_after_action = false;
    }
    let focus = ctx.memory(|memory| memory.focused());
    state.editor.history.finish_inactive_group(
        focus,
        ctx.input(|input| input.pointer.primary_down()),
        ctx.input(|input| input.time),
    );
}

/// Return from an auxiliary dialog to a registered editor surface. In
/// particular, do not leave keyboard undo attached to its old search field.
pub fn focus_editor(ctx: &egui::Context, state: &mut AppState) {
    state.editor.focus_after_action = true;
    ctx.request_repaint();
}

pub fn history_buttons(ui: &mut egui::Ui, state: &mut AppState) {
    for (icon, name, shortcut, action) in [
        (
            crate::widgets::Icon::Undo,
            state.editor.history.undo_name(),
            shortcut(ui.ctx(), Modifiers::COMMAND, Key::Z),
            EditorAction::Undo,
        ),
        (
            crate::widgets::Icon::Redo,
            state.editor.history.redo_name(),
            shortcut(ui.ctx(), Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::Z),
            EditorAction::Redo,
        ),
    ] {
        let verb = if matches!(action, EditorAction::Undo) {
            "Undo"
        } else {
            "Redo"
        };
        let help = name.map_or_else(
            || format!("{verb} ({shortcut})"),
            |name| format!("{verb} {} ({shortcut})", name.to_lowercase()),
        );
        let response =
            crate::widgets::icon_button(ui, icon, verb, name.is_some()).on_hover_text(help);
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, name.is_some(), verb)
        });
        state.editor.surface(&response);
        if response.clicked() {
            apply_action(ui.ctx(), state, action);
        }
    }
}

#[derive(Clone, Copy)]
struct MenuTarget {
    index: Option<usize>,
    revision: u64,
}

#[derive(Default)]
pub struct MenuResult {
    pub selection: Option<usize>,
    pub action: Option<EditorAction>,
}

fn menu_contents(ui: &mut egui::Ui, state: &AppState, target: MenuTarget) -> Option<EditorAction> {
    if target.revision != state.document_revision {
        ui.close();
        return None;
    }
    let action = if let Some(index) = target.index.filter(|&index| index < state.elements.len()) {
        if ui.button("Select object").clicked() {
            Some(EditorAction::SelectCanvas(crate::state::CanvasTarget {
                element: index,
                edit: crate::state::CanvasEditTarget::Content,
            }))
        } else if ui
            .add_enabled(
                crate::state::slot_frame(&state.elements[index]).is_some(),
                egui::Button::new("Select frame"),
            )
            .clicked()
        {
            Some(EditorAction::SelectCanvas(crate::state::CanvasTarget {
                element: index,
                edit: crate::state::CanvasEditTarget::Slot,
            }))
        } else if ui
            .add(egui::Button::new("Duplicate").shortcut_text(shortcut(
                ui.ctx(),
                Modifiers::COMMAND,
                Key::D,
            )))
            .clicked()
        {
            Some(EditorAction::Duplicate(index))
        } else if ui
            .add(egui::Button::new("Delete").shortcut_text(shortcut(
                ui.ctx(),
                Modifiers::NONE,
                if cfg!(target_os = "macos") {
                    Key::Backspace
                } else {
                    Key::Delete
                },
            )))
            .clicked()
        {
            Some(EditorAction::Delete(index))
        } else {
            None
        }
    } else {
        let undo = ui
            .add_enabled(
                state.editor.history.undo_name().is_some(),
                egui::Button::new("Undo").shortcut_text(shortcut(
                    ui.ctx(),
                    Modifiers::COMMAND,
                    Key::Z,
                )),
            )
            .clicked();
        let redo = ui
            .add_enabled(
                state.editor.history.redo_name().is_some(),
                egui::Button::new("Redo").shortcut_text(shortcut(
                    ui.ctx(),
                    Modifiers::COMMAND.plus(Modifiers::SHIFT),
                    Key::Z,
                )),
            )
            .clicked();
        if undo {
            Some(EditorAction::Undo)
        } else if redo {
            Some(EditorAction::Redo)
        } else {
            None
        }
    };
    if action.is_some() {
        ui.close();
    }
    action
}

pub fn row_menu(ui: &mut egui::Ui, state: &AppState, index: usize) -> (Response, MenuResult) {
    let enabled = !egui::DragAndDrop::has_any_payload(ui.ctx()) && ui.ctx().dragged_id().is_none();
    let response =
        crate::widgets::icon_button(ui, crate::widgets::Icon::Menu, "Object actions", enabled)
            .on_hover_text("Object actions");
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, "Object actions")
    });
    let key = response.id.with("target");
    let mut result = MenuResult::default();
    if response.clicked() {
        response.request_focus();
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                key,
                MenuTarget {
                    index: Some(index),
                    revision: state.document_revision,
                },
            )
        });
        result.selection = Some(index);
    }
    egui::Popup::menu(&response).show(|ui| {
        if let Some(target) = ui.ctx().data(|data| data.get_temp::<MenuTarget>(key)) {
            result.action = menu_contents(ui, state, target);
        }
    });
    (response, result)
}

pub fn context_menu(response: &Response, state: &AppState, index: Option<usize>) -> MenuResult {
    if egui::DragAndDrop::has_any_payload(&response.ctx) || response.ctx.dragged_id().is_some() {
        return MenuResult::default();
    }
    let key = response.id.with("context_target");
    let mut result = MenuResult::default();
    if response.secondary_clicked() && !egui::DragAndDrop::has_any_payload(&response.ctx) {
        response.request_focus();
        response.ctx.data_mut(|data| {
            data.insert_temp(
                key,
                MenuTarget {
                    index,
                    revision: state.document_revision,
                },
            )
        });
        result.selection = index;
    }
    response.context_menu(|ui| {
        if let Some(target) = ui.ctx().data(|data| data.get_temp::<MenuTarget>(key)) {
            result.action = menu_contents(ui, state, target);
        }
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command() -> Modifiers {
        if cfg!(target_os = "macos") {
            Modifiers::COMMAND.plus(Modifiers::MAC_CMD)
        } else {
            Modifiers::COMMAND.plus(Modifiers::CTRL)
        }
    }

    fn key(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn content(state: &AppState) -> String {
        match &state.elements[0] {
            LabelElement::Text { content, .. } => content.clone(),
            _ => panic!(),
        }
    }

    fn fixture() -> AppState {
        AppState {
            elements: vec![LabelElement::Text {
                text_box: None,
                content: "A".into(),
                font_size: Some(24.0),
                align: ptouch_render::text::TextAlign::Left,
                rotation: 0.0,
                flip_h: false,
                flip_v: false,
            }],
            selected_element: Some(0),
            ..Default::default()
        }
    }

    fn frame(ctx: &egui::Context, state: &mut AppState, time: f64, events: Vec<egui::Event>) -> Id {
        let mut surface = Id::NULL;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 1000.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| {
                begin_frame(ui.ctx(), state);
                egui::CentralPanel::default().show(ui, |ui| {
                    crate::panels::properties::show_properties(ui, state);
                    let response = ui.button("Editor surface");
                    state.editor.surface(&response);
                    surface = response.id;
                });
                end_frame(ui.ctx(), state);
            },
        );
        output.textures_delta.clear();
        surface
    }

    #[test]
    fn content_undo_is_shared_and_auxiliary_fields_keep_native_editing() {
        let ctx = egui::Context::default();
        let mut state = fixture();
        frame(&ctx, &mut state, 0.0, vec![]);
        frame(&ctx, &mut state, 0.1, vec![]);
        let text_id = *state
            .editor
            .content_inputs
            .iter()
            .find(|&&id| {
                ctx.read_response(id)
                    .is_some_and(|response| response.rect.height() > 40.0)
            })
            .unwrap();
        ctx.memory_mut(|memory| memory.request_focus(text_id));
        frame(&ctx, &mut state, 0.2, vec![egui::Event::Text("B".into())]);
        frame(&ctx, &mut state, 0.3, vec![egui::Event::Text("C".into())]);
        let edited = content(&state);
        assert!(edited.contains('B') && edited.contains('C'));
        frame(&ctx, &mut state, 0.4, vec![key(Key::Z, command())]);
        assert_eq!(content(&state), "A");
        frame(
            &ctx,
            &mut state,
            0.5,
            vec![key(Key::Z, command().plus(Modifiers::SHIFT))],
        );
        assert_eq!(content(&state), edited);
        ctx.memory_mut(|memory| memory.request_focus(Id::new("label_font_search")));
        frame(
            &ctx,
            &mut state,
            0.6,
            vec![egui::Event::Paste("filter".into())],
        );
        assert_eq!(state.font_search, "filter");
        frame(&ctx, &mut state, 0.7, vec![key(Key::Z, command())]);
        assert!(state.font_search.is_empty());
        assert_eq!(content(&state), edited);
        frame(
            &ctx,
            &mut state,
            0.8,
            vec![key(Key::D, command()), key(Key::Delete, Modifiers::NONE)],
        );
        assert_eq!(state.elements.len(), 1);
    }

    #[test]
    fn text_undo_redo_restores_the_caret_in_the_middle_of_content() {
        let ctx = egui::Context::default();
        let mut state = fixture();
        if let LabelElement::Text { content, .. } = &mut state.elements[0] {
            *content = "First middle last".into();
        }
        frame(&ctx, &mut state, 0.0, vec![]);
        let text_id = *state
            .editor
            .content_inputs
            .iter()
            .find(|&&id| {
                ctx.read_response(id)
                    .is_some_and(|response| response.rect.height() > 40.0)
            })
            .unwrap();
        let mut input = egui::TextEdit::load_state(&ctx, text_id).unwrap();
        input
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(6),
            )));
        input.store(&ctx, text_id);
        ctx.memory_mut(|memory| memory.request_focus(text_id));
        frame(&ctx, &mut state, 0.1, vec![]);
        frame(&ctx, &mut state, 0.2, vec![egui::Event::Text("B".into())]);
        assert_eq!(content(&state), "First Bmiddle last");
        frame(&ctx, &mut state, 0.3, vec![key(Key::Z, command())]);
        assert_eq!(content(&state), "First middle last");
        assert_eq!(
            egui::TextEdit::load_state(&ctx, text_id)
                .unwrap()
                .cursor
                .char_range()
                .unwrap()
                .primary
                .index,
            egui::text::CCursor::new(6).index
        );
        frame(
            &ctx,
            &mut state,
            0.4,
            vec![key(Key::Z, command().plus(Modifiers::SHIFT))],
        );
        assert_eq!(content(&state), "First Bmiddle last");
        assert_eq!(
            egui::TextEdit::load_state(&ctx, text_id)
                .unwrap()
                .cursor
                .char_range()
                .unwrap()
                .primary
                .index,
            egui::text::CCursor::new(7).index
        );
    }

    #[test]
    fn editor_shortcuts_duplicate_delete_and_restore_the_last_element() {
        let ctx = egui::Context::default();
        let mut state = fixture();
        let surface = frame(&ctx, &mut state, 0.0, vec![]);
        ctx.memory_mut(|memory| memory.request_focus(surface));
        frame(&ctx, &mut state, 0.1, vec![key(Key::D, command())]);
        assert_eq!(state.elements.len(), 2);
        assert_eq!(state.selected_element, Some(1));
        frame(
            &ctx,
            &mut state,
            0.2,
            vec![key(Key::ArrowUp, Modifiers::ALT)],
        );
        assert_eq!(state.selected_element, Some(0));
        frame(&ctx, &mut state, 0.3, vec![key(Key::Z, command())]);
        assert_eq!(state.selected_element, Some(1));
        frame(&ctx, &mut state, 0.4, vec![key(Key::Z, command())]);
        assert_eq!(state.elements.len(), 1);
        frame(
            &ctx,
            &mut state,
            0.5,
            vec![key(Key::Delete, Modifiers::NONE)],
        );
        assert!(state.elements.is_empty());
        frame(&ctx, &mut state, 0.6, vec![key(Key::Z, command())]);
        assert_eq!(state.elements.len(), 1);
        assert_eq!(state.selected_element, Some(0));
    }

    #[test]
    fn history_buttons_discard_numeric_drafts_before_the_field_loses_focus() {
        let ctx = egui::Context::default();
        let mut state = fixture();
        frame(&ctx, &mut state, 0.0, vec![]);
        let numeric = *state
            .editor
            .content_inputs
            .iter()
            .find(|&&id| {
                ctx.read_response(id)
                    .is_some_and(|response| response.rect.height() < 30.0)
            })
            .unwrap();
        let before = state.content_snapshot();
        state.font_margin = 3;
        state.record_content(before, "Margin", None, 0.1);
        ctx.memory_mut(|memory| memory.request_focus(numeric));
        ctx.data_mut(|data| data.insert_temp(numeric, "99".to_string()));
        // Simulate a header button after begin_frame cleared current registrations.
        state.editor.content_inputs.clear();
        apply_action(&ctx, &mut state, EditorAction::Undo);
        assert!(
            ctx.data_mut(|data| data.get_temp::<String>(numeric))
                .is_none()
        );
        frame(&ctx, &mut state, 0.2, vec![]);
        assert_eq!(state.font_margin, 0);
        assert!(matches!(
            state.elements[0],
            LabelElement::Text {
                font_size: Some(24.0),
                ..
            }
        ));
    }
}

#[cfg(test)]
mod menu_tests {
    use super::*;

    fn frame(
        ctx: &egui::Context,
        state: &mut AppState,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(320.0, 950.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                begin_frame(ui.ctx(), state);
                egui::CentralPanel::default().show(ui, |ui| {
                    crate::panels::toolbar::show_toolbar(ui, state);
                    crate::panels::sidebar::show_sidebar(ui, state);
                });
                end_frame(ui.ctx(), state);
            },
        )
    }

    fn texts(output: &mut egui::FullOutput, label: &str) -> Vec<egui::Pos2> {
        let positions = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .collect();
        output.textures_delta.clear();
        positions
    }

    fn click(ctx: &egui::Context, state: &mut AppState, point: egui::Pos2) -> egui::FullOutput {
        let event = |pressed| egui::Event::PointerButton {
            pos: point,
            pressed,
            button: egui::PointerButton::Primary,
            modifiers: Modifiers::NONE,
        };
        let mut output = frame(
            ctx,
            state,
            vec![egui::Event::PointerMoved(point), event(true)],
        );
        output.textures_delta.clear();
        frame(ctx, state, vec![event(false)]).textures_delta.clear();
        // A newly opened egui popup first performs an invisible sizing pass.
        frame(ctx, state, vec![])
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
    fn visible_row_menus_duplicate_delete_and_undo_without_old_button_bar() {
        let ctx = egui::Context::default();
        let mut state = fixture();
        frame(&ctx, &mut state, vec![]).textures_delta.clear();
        let mut output = frame(&ctx, &mut state, vec![]);
        assert!(texts(&mut output, "Up").is_empty());
        assert!(texts(&mut output, "Down").is_empty());
        assert!(texts(&mut output, "Delete").is_empty());
        let menus = crate::widgets::icon_positions(&ctx, crate::widgets::Icon::Menu);
        assert_eq!(menus.len(), 3);
        let mut output = click(&ctx, &mut state, menus[1]);
        assert_eq!(state.selected_element, Some(1));
        let duplicate = texts(&mut output, "Duplicate")[0];
        click(&ctx, &mut state, duplicate).textures_delta.clear();
        assert_eq!(state.elements.len(), 4);
        assert_eq!(state.selected_element, Some(2));
        assert!(matches!(
            state.elements[2],
            LabelElement::Padding { pixels: 20 }
        ));
        frame(&ctx, &mut state, vec![]).textures_delta.clear();
        let menu = crate::widgets::icon_positions(&ctx, crate::widgets::Icon::Menu)[2];
        let mut output = click(&ctx, &mut state, menu);
        let delete = texts(&mut output, "Delete")[0];
        click(&ctx, &mut state, delete).textures_delta.clear();
        assert_eq!(state.elements.len(), 3);
        frame(&ctx, &mut state, vec![]).textures_delta.clear();
        let undo = crate::widgets::icon_positions(&ctx, crate::widgets::Icon::Undo)[0];
        click(&ctx, &mut state, undo).textures_delta.clear();
        assert_eq!(state.elements.len(), 4);
        assert_eq!(state.selected_element, Some(2));
    }

    #[test]
    fn stale_open_menu_cannot_act_on_a_changed_document() {
        let ctx = egui::Context::default();
        let mut state = fixture();
        frame(&ctx, &mut state, vec![]).textures_delta.clear();
        frame(&ctx, &mut state, vec![]).textures_delta.clear();
        let menu = crate::widgets::icon_positions(&ctx, crate::widgets::Icon::Menu)[0];
        let mut output = click(&ctx, &mut state, menu);
        let old_duplicate = texts(&mut output, "Duplicate")[0];
        state.apply_editor_action(EditorAction::Duplicate(1));
        frame(&ctx, &mut state, vec![]).textures_delta.clear();
        assert!(!egui::Popup::is_any_open(&ctx));
        click(&ctx, &mut state, old_duplicate)
            .textures_delta
            .clear();
        assert_eq!(state.elements.len(), 4);
    }
}

#[cfg(test)]
mod preview_menu_tests {
    use super::*;
    use crate::preview::{PreviewGeometry, RenderedPreview, ZoomMode};

    fn frame(
        ctx: &egui::Context,
        state: &mut AppState,
        events: Vec<egui::Event>,
    ) -> (Option<PreviewGeometry>, egui::FullOutput) {
        let mut geometry = None;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 500.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                begin_frame(ui.ctx(), state);
                if state.needs_rerender {
                    let resolution = state.preview_resolution().unwrap();
                    let rendered =
                        ptouch_render::document::render_elements_with_layout_at_resolution(
                            &state.elements,
                            state.tape_width_px,
                            &state.font_name,
                            state.font_margin,
                            &mut ptouch_render::text::TextRenderer::new(),
                            resolution,
                        )
                        .unwrap();
                    state.preview = rendered.map(|rendered| {
                        RenderedPreview::from_rendered(
                            ui.ctx(),
                            rendered.mirrored(state.overall_flip_h, state.overall_flip_v),
                            resolution,
                        )
                    });
                    state.needs_rerender = false;
                }
                geometry = egui::CentralPanel::default()
                    .show(ui, |ui| crate::panels::canvas::show_canvas(ui, state))
                    .inner;
                end_frame(ui.ctx(), state);
            },
        );
        (geometry, output)
    }

    fn label(output: &mut egui::FullOutput, expected: &str) -> Option<egui::Pos2> {
        let position = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == expected => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        });
        output.textures_delta.clear();
        position
    }

    fn click(
        ctx: &egui::Context,
        state: &mut AppState,
        point: egui::Pos2,
        button: egui::PointerButton,
    ) -> egui::FullOutput {
        let event = |pressed| egui::Event::PointerButton {
            pos: point,
            pressed,
            button,
            modifiers: Modifiers::NONE,
        };
        frame(
            ctx,
            state,
            vec![egui::Event::PointerMoved(point), event(true)],
        )
        .1
        .textures_delta
        .clear();
        frame(ctx, state, vec![event(false)])
            .1
            .textures_delta
            .clear();
        frame(ctx, state, vec![]).1
    }

    #[test]
    fn right_click_targets_the_hovered_element_at_each_scale_and_when_mirrored() {
        for zoom in [ZoomMode::Fit, ZoomMode::ActualSize, ZoomMode::Pixels] {
            for mirrored in [false, true] {
                let ctx = egui::Context::default();
                let mut state = AppState {
                    elements: [10, 20, 30]
                        .into_iter()
                        .map(|pixels| LabelElement::Padding { pixels })
                        .collect(),
                    selected_element: Some(0),
                    overall_flip_h: mirrored,
                    printer_quality: Some(ptouch_core::quality::QualityProfile::for_model(
                        "PT-P750W", 180,
                    )),
                    print_quality: ptouch_core::PrintQuality::HighRes,
                    ..Default::default()
                };
                state.view.zoom = zoom;
                frame(&ctx, &mut state, vec![]).1.textures_delta.clear();
                let (geometry, mut output) = frame(&ctx, &mut state, vec![]);
                output.textures_delta.clear();
                let target = geometry
                    .unwrap()
                    .element_rect(state.preview.as_ref().unwrap(), 2)
                    .unwrap()
                    .center();
                let mut output = click(&ctx, &mut state, target, egui::PointerButton::Secondary);
                assert_eq!(state.selected_element, Some(2));
                let duplicate = label(&mut output, "Duplicate").unwrap();
                click(&ctx, &mut state, duplicate, egui::PointerButton::Primary)
                    .textures_delta
                    .clear();
                assert_eq!(state.elements.len(), 4);
                assert_eq!(state.selected_element, Some(3));
                assert!(matches!(
                    state.elements[3],
                    LabelElement::Padding { pixels: 30 }
                ));
                assert_eq!(state.preview.as_ref().unwrap().element_spans.len(), 4);
            }
        }
    }

    #[test]
    fn empty_preview_menu_undo_restores_the_last_deleted_element() {
        let ctx = egui::Context::default();
        let mut state = AppState::default();
        state.apply_editor_action(EditorAction::Add(LabelElement::CutMark));
        state.apply_editor_action(EditorAction::Delete(0));
        frame(&ctx, &mut state, vec![]).1.textures_delta.clear();
        frame(&ctx, &mut state, vec![]).1.textures_delta.clear();
        let mut output = click(
            &ctx,
            &mut state,
            egui::pos2(400.0, 250.0),
            egui::PointerButton::Secondary,
        );
        assert!(label(&mut output, "Duplicate").is_none());
        assert!(label(&mut output, "Delete").is_none());
        let undo = label(&mut output, "Undo").unwrap();
        click(&ctx, &mut state, undo, egui::PointerButton::Primary)
            .textures_delta
            .clear();
        assert_eq!(state.elements.len(), 1);
        assert_eq!(state.selected_element, Some(0));
        assert!(state.preview.is_some());
    }
}
