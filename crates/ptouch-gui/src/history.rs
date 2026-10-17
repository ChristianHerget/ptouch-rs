// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Document history: a pointer gesture is a transaction, including reflow.

use ptouch_render::document::LabelDocument;

#[derive(Clone)]
pub struct Snapshot {
    pub document: LabelDocument,
    pub selection: Option<usize>,
    pub time: f64,
}

#[derive(Default)]
pub struct DocumentHistory {
    current: Option<Snapshot>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    redo_backup: Option<Vec<Snapshot>>,
    gesture: bool,
    typing: Option<(f64, Option<usize>)>,
}

impl DocumentHistory {
    pub fn initialized(&self) -> bool {
        self.current.is_some()
    }
    pub fn last_edit_at(&self) -> f64 {
        self.current.as_ref().map_or(0.0, |s| s.time)
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn active_gesture(&self) -> bool {
        self.gesture
    }

    pub fn observe(
        &mut self,
        document: LabelDocument,
        selection: Option<usize>,
        down: bool,
        typing: bool,
        time: f64,
    ) {
        let was_gesture = self.gesture;
        let next = Snapshot {
            document,
            selection,
            time,
        };
        let mut next = next;
        if let Some(current) = &self.current {
            if current.document != next.document {
                let grouped = self.gesture
                    || typing
                        && self.typing.is_some_and(|(last, selected)| {
                            time - last < 1.0 && selected == selection
                        });
                if !grouped {
                    self.undo.push(current.clone());
                }
                if down && !self.gesture {
                    self.redo_backup = Some(std::mem::take(&mut self.redo));
                }
                self.redo.clear();
                self.typing = typing.then_some((time, selection));
                self.gesture = down;
            } else {
                next.time = current.time;
            }
        }
        self.current = Some(next);
        if !down {
            // Esc or a drag back to its exact origin leaves no empty undo step.
            if was_gesture
                && self.undo.last().is_some_and(|prior| {
                    Some(&prior.document) == self.current.as_ref().map(|s| &s.document)
                })
            {
                if let Some(prior) = self.undo.pop()
                    && let Some(current) = &mut self.current
                {
                    current.time = prior.time;
                }
                if let Some(redo) = self.redo_backup.take() {
                    self.redo = redo;
                }
            }
            self.redo_backup = None;
            self.gesture = false;
        }
        while self.undo.len() > 100
            || self.undo.len() > 1
                && self.undo.iter().map(snapshot_bytes).sum::<usize>() > 64 * 1024 * 1024
        {
            self.undo.remove(0);
        }
    }

    pub fn step(&mut self, redo: bool) -> Option<Snapshot> {
        let (source, destination) = if redo {
            (&mut self.redo, &mut self.undo)
        } else {
            (&mut self.undo, &mut self.redo)
        };
        let previous = source.pop()?;
        if let Some(current) = self.current.take() {
            destination.push(current);
        }
        self.current = Some(previous.clone());
        self.gesture = false;
        self.redo_backup = None;
        self.typing = None;
        Some(previous)
    }
}

fn snapshot_bytes(snapshot: &Snapshot) -> usize {
    // Cache and embedded image bytes dominate. TOML/base64 encoding every
    // pointer frame would be substantially more expensive than this bound.
    snapshot
        .document
        .elements
        .iter()
        .map(|element| {
            if let Some(o) = element.as_object() {
                match &o.content {
                    ptouch_render::document::ObjectContent::Text {
                        content, styles, ..
                    } => content.len() + styles.len() * 256 + 512,
                    ptouch_render::document::ObjectContent::Image {
                        image_data, bitmap, ..
                    } => image_data.len() + bitmap.as_ref().map_or(0, |b| b.data().len()) + 512,
                    _ => 512,
                }
            } else if let ptouch_render::document::LabelElement::Image {
                image_data, bitmap, ..
            } = element
            {
                image_data.len() + bitmap.as_ref().map_or(0, |b| b.data().len()) + 512
            } else if let ptouch_render::document::LabelElement::Text { content, .. } = element {
                content.len() + 512
            } else {
                512
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptouch_render::document::LabelElement;
    fn doc(pixels: u32) -> LabelDocument {
        LabelDocument {
            version: 1,
            tape_width_mm: 12,
            dpi: 180,
            font_name: String::new(),
            font_margin: 0,
            flip_h: false,
            flip_v: false,
            elements: vec![LabelElement::Padding { pixels }],
        }
    }
    #[test]
    fn pointer_transaction_undo_redo_and_cancellation() {
        let mut h = DocumentHistory::default();
        h.observe(doc(1), Some(0), false, false, 0.0);
        for n in 2..20 {
            h.observe(doc(n), Some(0), true, false, n as f64);
        }
        h.observe(doc(20), Some(0), false, false, 20.0);
        assert_eq!(h.undo.len(), 1);
        assert_eq!(h.step(false).unwrap().document, doc(1));
        assert_eq!(h.step(true).unwrap().document, doc(20));
        h.observe(doc(30), Some(0), true, false, 21.0);
        h.observe(doc(20), Some(0), false, false, 22.0);
        assert_eq!(h.undo.len(), 1);
    }

    #[test]
    fn cancelling_a_drag_preserves_an_existing_redo_branch() {
        let mut h = DocumentHistory::default();
        h.observe(doc(1), None, false, false, 0.0);
        h.observe(doc(2), None, false, false, 1.0);
        h.step(false).unwrap();
        h.observe(doc(3), None, true, false, 2.0);
        h.observe(doc(1), None, false, false, 3.0);
        assert!(h.can_redo());
        assert_eq!(h.step(true).unwrap().document, doc(2));
    }

    #[test]
    fn legacy_embedded_images_also_limit_history_memory() {
        let mut h = DocumentHistory::default();
        let mut document = doc(0);
        document.elements.push(LabelElement::Image {
            path: None,
            image_data: vec![0; 2 * 1024 * 1024],
            bitmap: None,
            target_height: None,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
        });
        for n in 0..40 {
            document.elements[0] = LabelElement::Padding { pixels: n };
            h.observe(document.clone(), None, false, false, f64::from(n));
        }
        assert!(h.undo.len() < 32);
        assert!(h.can_undo());
        assert!(
            h.step(false).unwrap().document.elements[0] == LabelElement::Padding { pixels: 38 }
        );
    }
}
