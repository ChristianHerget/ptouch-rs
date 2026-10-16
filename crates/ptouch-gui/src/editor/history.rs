// SPDX-License-Identifier: MIT
//! Content-only history. Image source bytes are shared; render caches stay out.

use crate::state::{AppState, LabelElement};
use ptouch_render::document::{ContentBox, LabelObject, ObjectContent, TextBox};
use ptouch_render::text::TextAlign;
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Weak},
};

#[derive(Clone, PartialEq)]
enum ElementContent {
    Object {
        frame: TextBox,
        content_box: Option<ContentBox>,
        rotation: f32,
        flip_h: bool,
        flip_v: bool,
        content: Box<ElementContent>,
    },
    Text {
        text_box: Option<TextBox>,
        content: String,
        font_size: Option<f32>,
        align: TextAlign,
        line_spacing: f32,
        styles: Vec<ptouch_render::styled_text::StyleSpan>,
        rotation: f32,
        flip_h: bool,
        flip_v: bool,
    },
    Image {
        path: Option<PathBuf>,
        bytes: Arc<[u8]>,
        rotation: f32,
        target_height: Option<u32>,
        flip_h: bool,
        flip_v: bool,
    },
    CutMark,
    Padding(u32),
}

impl ElementContent {
    fn restore(&self) -> LabelElement {
        match self {
            Self::Object {
                frame,
                content_box,
                rotation,
                flip_h,
                flip_v,
                content,
            } => {
                let content = match content.restore() {
                    LabelElement::Text {
                        content: text,
                        font_size,
                        align,
                        ..
                    } => ObjectContent::Text {
                        content: text,
                        font_size,
                        align,
                        line_spacing: match content.as_ref() {
                            Self::Text { line_spacing, .. } => *line_spacing,
                            _ => unreachable!("restored text has text content"),
                        },
                        styles: match content.as_ref() {
                            Self::Text { styles, .. } => styles.clone(),
                            _ => unreachable!("restored text has text content"),
                        },
                    },
                    LabelElement::Image {
                        path,
                        image_data,
                        bitmap,
                        ..
                    } => ObjectContent::Image {
                        path,
                        image_data,
                        bitmap,
                    },
                    LabelElement::CutMark => ObjectContent::CutMark,
                    LabelElement::Padding { .. } => ObjectContent::Padding,
                    LabelElement::Object(_) => unreachable!("object content is never nested"),
                };
                LabelElement::Object(LabelObject {
                    frame: frame.clone(),
                    content_box: content_box.clone(),
                    rotation: *rotation,
                    flip_h: *flip_h,
                    flip_v: *flip_v,
                    content,
                })
            }
            Self::Text {
                text_box,
                content,
                font_size,
                align,
                rotation,
                flip_h,
                flip_v,
                ..
            } => LabelElement::Text {
                text_box: text_box.clone(),
                content: content.clone(),
                font_size: *font_size,
                align: *align,
                rotation: *rotation,
                flip_h: *flip_h,
                flip_v: *flip_v,
            },
            Self::Image {
                path,
                bytes,
                rotation,
                target_height,
                flip_h,
                flip_v,
            } => {
                let mut element = LabelElement::image_from_bytes(path.clone(), bytes.to_vec());
                if let LabelElement::Image {
                    rotation: angle,
                    target_height: height,
                    flip_h: h,
                    flip_v: v,
                    ..
                } = &mut element
                {
                    *angle = *rotation;
                    *height = *target_height;
                    *h = *flip_h;
                    *v = *flip_v;
                }
                element
            }
            Self::CutMark => LabelElement::CutMark,
            Self::Padding(pixels) => LabelElement::Padding { pixels: *pixels },
        }
    }
}

#[derive(Clone, PartialEq)]
pub(crate) struct ContentSelection {
    pub element: Option<usize>,
    pub cursor: Option<(egui::Id, egui::text::CCursorRange)>,
    pub target: crate::state::CanvasEditTarget,
    pub rich_text: Option<(egui::Id, crate::rich_text_editor::InlineState)>,
    pub rich_generation: u64,
}

#[derive(Clone, PartialEq)]
pub(crate) struct ContentSnapshot {
    document: (u32, u16),
    elements: Vec<ElementContent>,
    font_name: String,
    font_margin: u32,
    flip_h: bool,
    flip_v: bool,
    selection: ContentSelection,
}

impl ContentSnapshot {
    fn same_content(&self, other: &Self) -> bool {
        self.document == other.document
            && self.elements == other.elements
            && self.font_name == other.font_name
            && self.font_margin == other.font_margin
            && self.flip_h == other.flip_h
            && self.flip_v == other.flip_v
            && self
                .selection
                .rich_text
                .as_ref()
                .map(|(id, state)| (*id, &state.pending, state.pending_at))
                == other
                    .selection
                    .rich_text
                    .as_ref()
                    .map(|(id, state)| (*id, &state.pending, state.pending_at))
    }

    pub fn restore(&self, state: &mut AppState) {
        let old_dpi = state.design_dpi;
        (state.document_version, state.design_dpi) = self.document;
        if old_dpi != state.design_dpi && !state.printer_connected && !state.preview_uses_printer {
            state.update_tape_pixels();
        }
        state.elements = self.elements.iter().map(ElementContent::restore).collect();
        state.font_name.clone_from(&self.font_name);
        state.font_margin = self.font_margin;
        state.overall_flip_h = self.flip_h;
        state.overall_flip_v = self.flip_v;
        state.select_element(self.selection.element);
        state.canvas_edit_target = self.selection.target;
        state.canvas_drag_target = None;
        state.canvas_hover_target = None;
        state.editor.text_cursor = self.selection.cursor;
        state.editor.rich_text = self.selection.rich_text.clone();
        state.editor_generation = self.selection.rich_generation;
        state.editor.rich_restore = true;
        state.mark_dirty();
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum EditGroup {
    Typing(egui::Id),
    Drag(egui::Id),
}

struct Transaction {
    before: ContentSnapshot,
    after: ContentSnapshot,
    name: &'static str,
}

#[derive(Default)]
pub struct ContentHistory {
    undos: VecDeque<Transaction>,
    redos: Vec<Transaction>,
    group: Option<(EditGroup, f64)>,
    sources: Vec<Weak<[u8]>>,
    // Keep currently used assets alive even before the first history entry.
    current_sources: Vec<Arc<[u8]>>,
}

impl ContentHistory {
    fn intern(&mut self, bytes: &[u8]) -> Arc<[u8]> {
        self.sources.retain(|source| source.strong_count() > 0);
        if let Some(source) = self
            .sources
            .iter()
            .filter_map(Weak::upgrade)
            .find(|source| source.as_ref() == bytes)
        {
            return source;
        }
        let source: Arc<[u8]> = bytes.into();
        self.sources.push(Arc::downgrade(&source));
        source
    }

    fn image_bytes(
        &mut self,
        bytes: &[u8],
        bitmap: Option<&ptouch_render::bitmap::LabelBitmap>,
    ) -> Arc<[u8]> {
        // Preserve bitmap-only callers without retaining decoded caches in history.
        if bytes.is_empty()
            && let Some(bitmap) = bitmap
        {
            let mut png = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(bitmap.to_rgba_image())
                .write_to(&mut png, image::ImageFormat::Png)
                .expect("encoding a bitmap into memory");
            self.intern(png.get_ref())
        } else {
            self.intern(bytes)
        }
    }

    fn capture_element(&mut self, element: &LabelElement) -> ElementContent {
        match element {
            LabelElement::Object(object) => {
                let content = match &object.content {
                    ObjectContent::Text {
                        content,
                        font_size,
                        align,
                        line_spacing,
                        styles,
                    } => ElementContent::Text {
                        content: content.clone(),
                        font_size: *font_size,
                        align: *align,
                        line_spacing: *line_spacing,
                        styles: styles.clone(),
                        text_box: None,
                        rotation: 0.0,
                        flip_h: false,
                        flip_v: false,
                    },
                    ObjectContent::Image {
                        path,
                        image_data,
                        bitmap,
                    } => ElementContent::Image {
                        path: path.clone(),
                        bytes: self.image_bytes(image_data, bitmap.as_ref()),
                        rotation: 0.0,
                        target_height: None,
                        flip_h: false,
                        flip_v: false,
                    },
                    ObjectContent::CutMark => ElementContent::CutMark,
                    ObjectContent::Padding => ElementContent::Padding(0),
                };
                ElementContent::Object {
                    frame: object.frame.clone(),
                    content_box: object.content_box.clone(),
                    rotation: object.rotation,
                    flip_h: object.flip_h,
                    flip_v: object.flip_v,
                    content: Box::new(content),
                }
            }
            LabelElement::Text {
                text_box,
                content,
                font_size,
                align,
                rotation,
                flip_h,
                flip_v,
            } => ElementContent::Text {
                text_box: text_box.clone(),
                content: content.clone(),
                font_size: *font_size,
                align: *align,
                line_spacing: ptouch_render::text::DEFAULT_LINE_SPACING,
                styles: Vec::new(),
                rotation: *rotation,
                flip_h: *flip_h,
                flip_v: *flip_v,
            },
            LabelElement::Image {
                path,
                image_data,
                bitmap,
                rotation,
                target_height,
                flip_h,
                flip_v,
            } => ElementContent::Image {
                path: path.clone(),
                bytes: self.image_bytes(image_data, bitmap.as_ref()),
                rotation: *rotation,
                target_height: *target_height,
                flip_h: *flip_h,
                flip_v: *flip_v,
            },
            LabelElement::CutMark => ElementContent::CutMark,
            LabelElement::Padding { pixels } => ElementContent::Padding(*pixels),
        }
    }

    pub fn capture(
        &mut self,
        elements: &[LabelElement],
        font_name: &str,
        font_margin: u32,
        mirrors: (bool, bool),
        selection: ContentSelection,
        document: (u32, u16),
    ) -> ContentSnapshot {
        let elements: Vec<_> = elements
            .iter()
            .map(|element| self.capture_element(element))
            .collect();
        self.current_sources = elements
            .iter()
            .filter_map(|element| match element {
                ElementContent::Image { bytes, .. } => Some(bytes.clone()),
                ElementContent::Object { content, .. } => match content.as_ref() {
                    ElementContent::Image { bytes, .. } => Some(bytes.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        ContentSnapshot {
            document,
            elements,
            font_name: font_name.into(),
            font_margin,
            flip_h: mirrors.0,
            flip_v: mirrors.1,
            selection,
        }
    }

    pub fn record(
        &mut self,
        before: ContentSnapshot,
        after: ContentSnapshot,
        name: &'static str,
        group: Option<EditGroup>,
        time: f64,
    ) -> bool {
        if before.same_content(&after) {
            return false;
        }
        let merge = group.is_some()
            && self.group.is_some_and(|(previous, changed)| {
                Some(previous) == group
                    && (matches!(previous, EditGroup::Drag(_)) || time - changed < 1.0)
            })
            && self
                .undos
                .back()
                .is_some_and(|edit| edit.after.same_content(&before));
        if merge {
            self.undos.back_mut().unwrap().after = after;
        } else {
            self.undos.push_back(Transaction {
                before,
                after,
                name,
            });
            if self.undos.len() > 100 {
                self.undos.pop_front();
            }
        }
        self.redos.clear();
        self.group = group.map(|key| (key, time));
        if self
            .undos
            .back()
            .is_some_and(|edit| edit.before.same_content(&edit.after))
        {
            self.undos.pop_back();
            self.group = None;
        }
        true
    }

    pub fn finish_group(&mut self) {
        self.group = None;
    }

    /// A validated structural move still matters when two elements have equal
    /// content: their selection/order must be undoable and targets invalidated.
    pub fn record_action(
        &mut self,
        before: ContentSnapshot,
        after: ContentSnapshot,
        name: &'static str,
    ) {
        self.finish_group();
        self.undos.push_back(Transaction {
            before,
            after,
            name,
        });
        if self.undos.len() > 100 {
            self.undos.pop_front();
        }
        self.redos.clear();
    }

    pub fn finish_inactive_group(
        &mut self,
        focus: Option<egui::Id>,
        pointer_down: bool,
        time: f64,
    ) {
        if self.group.is_some_and(|(group, changed)| match group {
            EditGroup::Typing(id) => focus != Some(id) || time - changed >= 1.0,
            EditGroup::Drag(_) => !pointer_down,
        }) {
            self.finish_group();
        }
    }

    pub fn undo_name(&self) -> Option<&'static str> {
        self.undos.back().map(|edit| edit.name)
    }
    pub fn redo_name(&self) -> Option<&'static str> {
        self.redos.last().map(|edit| edit.name)
    }

    pub fn undo(&mut self) -> Option<ContentSnapshot> {
        self.finish_group();
        let edit = self.undos.pop_back()?;
        let snapshot = edit.before.clone();
        self.redos.push(edit);
        Some(snapshot)
    }

    pub fn redo(&mut self) -> Option<ContentSnapshot> {
        self.finish_group();
        let edit = self.redos.pop()?;
        let snapshot = edit.after.clone();
        self.undos.push_back(edit);
        Some(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::EditorAction;

    fn text(content: &str) -> LabelElement {
        LabelElement::Text {
            text_box: None,
            content: content.into(),
            font_size: Some(24.0),
            align: TextAlign::Left,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
        }
    }

    #[test]
    fn mixed_actions_restore_content_and_selection_without_restoring_printer_state() {
        let mut state = AppState::default();
        state.apply_editor_action(EditorAction::Add(text("Original")));
        state.apply_editor_action(EditorAction::Duplicate(0));
        let before = state.content_snapshot();
        if let LabelElement::Text {
            content, rotation, ..
        } = &mut state.elements[1]
        {
            *content = "Edited copy".into();
            *rotation = 37.0;
        }
        state.font_name = "Different font".into();
        state.font_margin = 3;
        state.overall_flip_h = true;
        state.record_content(before, "Edit element", None, 1.0);
        state.apply_editor_action(EditorAction::Move { from: 1, before: 0 });
        state.apply_editor_action(EditorAction::Delete(1));
        state.tape_width_mm = 24;
        state.printer_dpi = 360;
        state.printer_connected = true;
        state.operation_in_progress = true;
        state.printer_status = Some("Printing".into());
        state.view.zoom = crate::preview::ZoomMode::Pixels;
        assert!(state.apply_editor_action(EditorAction::Undo));
        assert_eq!(state.elements.len(), 2);
        assert_eq!(state.selected_element, Some(0));
        assert!(state.apply_editor_action(EditorAction::Undo));
        assert_eq!(state.selected_element, Some(1));
        assert!(
            matches!(&state.elements[1], LabelElement::Text { content, .. } if content == "Edited copy")
        );
        assert!(state.apply_editor_action(EditorAction::Undo));
        assert!(
            matches!(&state.elements[1], LabelElement::Text { content, rotation, .. } if content == "Original" && *rotation == 0.0)
        );
        assert_eq!(state.font_name, "DejaVuSans");
        assert_eq!(state.font_margin, 0);
        assert!(!state.overall_flip_h);
        assert!(state.apply_editor_action(EditorAction::Redo));
        assert_eq!(state.font_name, "Different font");
        assert_eq!(state.rotation_input, "37");
        assert_eq!(state.tape_width_mm, 24);
        assert_eq!(state.printer_dpi, 360);
        assert!(state.printer_connected && state.operation_in_progress);
        assert_eq!(state.printer_status.as_deref(), Some("Printing"));
        assert_eq!(state.view.zoom, crate::preview::ZoomMode::Pixels);
        assert!(state.needs_rerender);
    }

    #[test]
    fn duplicate_preserves_all_element_types_and_image_sources_without_file_access() {
        let mut bitmap = ptouch_render::bitmap::LabelBitmap::new(7, 5);
        bitmap.set_pixel(1, 2, true);
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(bitmap.to_rgba_image())
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let mut image = LabelElement::image_from_bytes(
            Some(PathBuf::from("/missing/source.png")),
            png.into_inner(),
        );
        if let LabelElement::Image {
            rotation,
            target_height,
            flip_h,
            flip_v,
            ..
        } = &mut image
        {
            *rotation = 90.0;
            *target_height = Some(32);
            *flip_h = true;
            *flip_v = true;
        }
        for source in [
            text("Text"),
            image,
            LabelElement::Padding { pixels: 17 },
            LabelElement::CutMark,
        ] {
            let mut state = AppState {
                elements: vec![source],
                selected_element: Some(0),
                ..Default::default()
            };
            let original = state.content_snapshot();
            state.apply_editor_action(EditorAction::Duplicate(0));
            let duplicated = state.content_snapshot();
            assert_eq!(duplicated.elements.len(), 2);
            assert!(duplicated.elements[0] == duplicated.elements[1]);
            assert_eq!(state.selected_element, Some(1));
            state.apply_editor_action(EditorAction::Delete(1));
            state.apply_editor_action(EditorAction::Undo);
            assert!(state.content_snapshot().elements == duplicated.elements);
            if let LabelElement::Image {
                bitmap, image_data, ..
            } = &mut state.elements[1]
            {
                assert!(bitmap.as_ref().unwrap().get_pixel(1, 2));
                image_data[0] = 0;
                assert!(state.content_snapshot().elements[0] == original.elements[0]);
            }
        }
    }

    #[test]
    fn spacing_and_duplicate_position_survive_history_without_upgrading_defaults() {
        let mut state = AppState::default();
        state.add_text();
        assert_eq!(state.document().version, 4);
        let before = state.content_snapshot();
        if let ObjectContent::Text { line_spacing, .. } =
            &mut state.elements[0].as_object_mut().unwrap().content
        {
            *line_spacing = 0.8;
        }
        state.record_content(before, "Line spacing", None, 1.0);
        let spaced = state.document().to_toml_string().unwrap();
        assert!(spaced.contains("version = 5"));

        state.canvas_edit_target = crate::state::CanvasEditTarget::Slot;
        assert!(state.apply_editor_action(EditorAction::Duplicate(0)));
        assert_eq!(state.elements[1].as_object().unwrap().frame.x_mm, 13.0);
        assert_eq!(
            state.canvas_edit_target,
            crate::state::CanvasEditTarget::Slot
        );
        let duplicated = state.document().to_toml_string().unwrap();
        assert!(state.apply_editor_action(EditorAction::Undo));
        assert_eq!(state.document().to_toml_string().unwrap(), spaced);
        assert!(state.apply_editor_action(EditorAction::Undo));
        assert_eq!(state.document().version, 4);
        assert!(
            !state
                .document()
                .to_toml_string()
                .unwrap()
                .contains("line_spacing")
        );
        assert!(state.apply_editor_action(EditorAction::Redo));
        assert_eq!(state.document().to_toml_string().unwrap(), spaced);
        assert!(state.apply_editor_action(EditorAction::Redo));
        assert_eq!(state.document().to_toml_string().unwrap(), duplicated);

        // An unrelated edit also restores text through the history snapshot.
        assert!(state.apply_editor_action(EditorAction::Delete(0)));
        assert!(state.apply_editor_action(EditorAction::Undo));
        assert_eq!(state.document().to_toml_string().unwrap(), duplicated);
    }

    #[test]
    fn typing_groups_split_on_idle_or_focus_and_drags_are_single_transactions() {
        let mut state = AppState {
            elements: vec![text("A")],
            ..Default::default()
        };
        let field = egui::Id::new("text");
        for (time, value) in [(0.0, "AB"), (0.5, "ABC"), (2.0, "ABCD")] {
            let before = state.content_snapshot();
            if let LabelElement::Text { content, .. } = &mut state.elements[0] {
                *content = value.into();
            }
            state.record_content(before, "Edit text", Some(EditGroup::Typing(field)), time);
        }
        assert_eq!(state.editor.history.undos.len(), 2);
        state.apply_editor_action(EditorAction::Undo);
        assert!(
            matches!(&state.elements[0], LabelElement::Text { content, .. } if content == "ABC")
        );
        state.apply_editor_action(EditorAction::Undo);
        assert!(matches!(&state.elements[0], LabelElement::Text { content, .. } if content == "A"));
        for time in [3.0, 8.0] {
            let before = state.content_snapshot();
            state.font_margin += 1;
            state.record_content(before, "Margin", Some(EditGroup::Drag(field)), time);
        }
        assert_eq!(state.editor.history.undos.len(), 1);
        state.editor.history.finish_inactive_group(None, false, 9.0);
        let before = state.content_snapshot();
        state.font_margin += 1;
        state.record_content(before, "Margin", Some(EditGroup::Drag(field)), 9.0);
        assert_eq!(state.editor.history.undos.len(), 2);
        state.editor.history.finish_inactive_group(None, false, 9.0);
        assert!(state.editor.history.group.is_none());
    }

    #[test]
    fn history_limits_noops_and_redo_invalidation_follow_content_changes_only() {
        let mut state = AppState::default();
        for _ in 0..105 {
            state.apply_editor_action(EditorAction::Add(LabelElement::CutMark));
        }
        assert_eq!(state.editor.history.undos.len(), 100);
        state.apply_editor_action(EditorAction::Undo);
        state.mark_dirty(); // status/quality/tape render invalidation is not an edit.
        state.select_element(Some(0));
        assert!(state.editor.history.redo_name().is_some());
        assert!(!state.apply_editor_action(EditorAction::Move { from: 0, before: 1 }));
        assert!(!state.apply_editor_action(EditorAction::Delete(999)));
        assert!(state.editor.history.redo_name().is_some());
        state.apply_editor_action(EditorAction::Duplicate(0));
        assert!(state.editor.history.redo_name().is_none());
        for _ in 0..100 {
            assert!(state.apply_editor_action(EditorAction::Undo));
        }
        assert!(!state.apply_editor_action(EditorAction::Undo));
        assert_eq!(state.elements.len(), 5);
    }

    #[test]
    fn image_history_shares_source_bytes_and_opening_a_layout_resets_history() {
        let mut state = AppState {
            elements: vec![LabelElement::image_from_bytes(None, vec![1, 2, 3])],
            ..Default::default()
        };
        let before = state.content_snapshot();
        state.apply_editor_action(EditorAction::Duplicate(0));
        let after = state.content_snapshot();
        let ElementContent::Image {
            bytes: original, ..
        } = &before.elements[0]
        else {
            panic!()
        };
        for element in &after.elements {
            let ElementContent::Image { bytes, .. } = element else {
                panic!()
            };
            assert!(Arc::ptr_eq(original, bytes));
        }
        let document = ptouch_render::document::LabelDocument {
            version: 1,
            tape_width_mm: 12,
            dpi: 180,
            font_name: "sans-serif".into(),
            font_margin: 0,
            flip_h: false,
            flip_v: false,
            elements: vec![text("New")],
        };
        crate::panels::toolbar::apply_layout(&mut state, document).unwrap();
        assert!(state.editor.history.undo_name().is_none());
        assert!(state.editor.history.redo_name().is_none());
    }

    #[test]
    fn returning_to_the_original_value_does_not_leave_a_noop_in_history() {
        let mut state = AppState::default();
        let field = egui::Id::new("margin");
        for (time, margin) in [(0.0, 1), (0.1, 0)] {
            let before = state.content_snapshot();
            state.font_margin = margin;
            state.record_content(before, "Margin", Some(EditGroup::Typing(field)), time);
        }
        assert!(state.editor.history.undo_name().is_none());
    }
    #[test]
    fn conversion_geometry_design_dpi_and_target_are_restored_together() {
        use crate::state::{CanvasEditTarget, CanvasTarget};
        let mut state = AppState {
            document_version: 1,
            elements: vec![text("Old")],
            selected_element: Some(0),
            ..Default::default()
        };
        let original = state.document().to_toml_string().unwrap();
        state.convert_to_free_layout().unwrap();
        let converted = state.document().to_toml_string().unwrap();
        state.select_canvas_target(Some(CanvasTarget {
            element: 0,
            edit: CanvasEditTarget::Slot,
        }));
        let before = state.content_snapshot();
        state.pin_content_box(0);
        let object = state.elements[0].as_object_mut().unwrap();
        object.rotation = 37.0;
        object.frame.x_mm = 5.75;
        object.content_box.as_mut().unwrap().offset_x_mm = -2.25;
        state.design_dpi = 360;
        state.record_content(before, "Edit object", None, 1.0);
        let edited = state.document().to_toml_string().unwrap();
        state.apply_editor_action(EditorAction::Undo);
        assert_eq!(state.document().to_toml_string().unwrap(), converted);
        assert_eq!(state.canvas_edit_target, CanvasEditTarget::Slot);
        assert_eq!(state.design_dpi, 180);
        assert_eq!(state.tape_width_px, 76);
        state.apply_editor_action(EditorAction::Undo);
        assert_eq!(state.document().to_toml_string().unwrap(), original);
        state.apply_editor_action(EditorAction::Redo);
        state.apply_editor_action(EditorAction::Redo);
        assert_eq!(state.document().to_toml_string().unwrap(), edited);
        assert_eq!(state.canvas_edit_target, CanvasEditTarget::Slot);
        assert_eq!(state.design_dpi, 360);
        assert_eq!(state.tape_width_px, 150);
    }
}
