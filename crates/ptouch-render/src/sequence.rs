// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Sequence membership is independent of composition order. Legacy documents
//! keep their adjacent-relative semantics until an explicit structural edit.

use std::collections::BTreeMap;

use crate::{
    RenderError, Result,
    document::{LabelElement, PositionMode, TextBox},
    object_layout::{Placement, WidthSizing},
};

#[derive(Default)]
pub struct LayoutCursor {
    pub right_mm: f64,
    sequences: BTreeMap<u64, f64>,
}

impl LayoutCursor {
    pub fn place(&mut self, frame: &TextBox, sequence: Option<u64>) -> Result<f64> {
        let x = match frame.position {
            PositionMode::Absolute => frame.x_mm,
            PositionMode::Relative => {
                let previous = match sequence {
                    Some(id) => *self.sequences.get(&id).ok_or_else(|| {
                        RenderError::Layout("sequence must start at an absolute anchor".into())
                    })?,
                    None => self.right_mm,
                };
                previous + frame.gap_mm
            }
        };
        self.right_mm = x + frame.width_mm;
        if let Some(id) = sequence {
            self.sequences.insert(id, self.right_mm);
        }
        Ok(x)
    }
}

pub fn validate(elements: &[LabelElement], version: u32) -> Result<()> {
    let mut seen = BTreeMap::new();
    for object in elements.iter().filter_map(LabelElement::as_object) {
        let Some(id) = object.sequence else { continue };
        if version < 11 || id == 0 || id > i64::MAX as u64 {
            return Err(RenderError::Layout(
                "sequence identifiers require version 11 and must be in 1..=i64::MAX".into(),
            ));
        }
        if object
            .layout
            .as_ref()
            .is_some_and(|p| p.placement == Placement::Free)
        {
            return Err(RenderError::Layout(
                "free objects cannot belong to a sequence".into(),
            ));
        }
        let expected = if seen.insert(id, ()).is_some() {
            PositionMode::Relative
        } else {
            PositionMode::Absolute
        };
        if object.frame.position != expected {
            return Err(RenderError::Layout(
                "a sequence requires one absolute anchor followed by relative members".into(),
            ));
        }
    }
    Ok(())
}

/// Explicit members can have independent objects between them in the list.
pub fn members(elements: &[LabelElement], index: usize) -> Option<Vec<usize>> {
    let object = elements.get(index)?.as_object()?;
    if let Some(id) = object.sequence {
        return Some(
            elements
                .iter()
                .enumerate()
                .filter_map(|(i, e)| (e.as_object()?.sequence == Some(id)).then_some(i))
                .collect(),
        );
    }
    let mut start = index;
    while start > 0
        && elements[start].as_object()?.frame.position == PositionMode::Relative
        && elements[start - 1]
            .as_object()
            .is_some_and(|o| o.sequence.is_none())
    {
        start -= 1;
    }
    let mut end = start;
    while elements
        .get(end + 1)
        .and_then(LabelElement::as_object)
        .is_some_and(|o| o.sequence.is_none() && o.frame.position == PositionMode::Relative)
    {
        end += 1;
    }
    let first = elements[start].as_object()?;
    (end > start
        || first.frame.position == PositionMode::Relative
        || first
            .layout
            .as_ref()
            .is_some_and(|p| p.placement == Placement::Flow))
    .then(|| (start..=end).collect())
}

/// Preserve exact physical origins while recording previously implicit links.
pub fn materialize(elements: &mut [LabelElement]) -> Result<()> {
    if elements.iter().any(|e| e.as_object().is_none()) {
        return Err(RenderError::Layout(
            "convert legacy elements before editing sequence membership".into(),
        ));
    }
    let mut cursor = LayoutCursor::default();
    let positions: Vec<_> = elements
        .iter()
        .map(|e| {
            let o = e.as_object().unwrap();
            cursor.place(&o.frame, o.sequence)
        })
        .collect::<Result<_>>()?;
    let mut next = elements
        .iter()
        .filter_map(|e| e.as_object()?.sequence)
        .max()
        .unwrap_or(0);
    let mut index = 0;
    while index < elements.len() {
        if elements[index].as_object().unwrap().sequence.is_some() {
            index += 1;
            continue;
        }
        if let Some(chain) = members(elements, index) {
            next = next
                .checked_add(1)
                .ok_or_else(|| RenderError::Layout("sequence identifier limit reached".into()))?;
            for &i in &chain {
                let object = elements[i].as_object_mut().unwrap();
                object.sequence = Some(next);
                if let Some(policy) = &mut object.layout
                    && policy.placement == Placement::Free
                {
                    policy.placement = Placement::Flow;
                    policy.width = WidthSizing::Fixed;
                }
            }
            let start = chain[0];
            let frame = &mut elements[start].as_object_mut().unwrap().frame;
            frame.position = PositionMode::Absolute;
            frame.x_mm = positions[start];
            index = *chain.last().unwrap() + 1;
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn anchors(elements: &[LabelElement]) -> BTreeMap<u64, (TextBox, f64)> {
    let mut anchors = BTreeMap::new();
    for (i, e) in elements.iter().enumerate() {
        if let Some(o) = e.as_object()
            && let Some(id) = o.sequence
        {
            anchors.entry(id).or_insert_with(|| {
                let gap = elements[i + 1..]
                    .iter()
                    .filter_map(LabelElement::as_object)
                    .find(|o| o.sequence == Some(id))
                    .map_or(3.0, |o| o.frame.gap_mm);
                (o.frame.clone(), gap)
            });
        }
    }
    anchors
}

fn normalize(elements: &mut [LabelElement], anchors: &BTreeMap<u64, (TextBox, f64)>) {
    let mut seen = BTreeMap::new();
    for o in elements.iter_mut().filter_map(LabelElement::as_object_mut) {
        let Some(id) = o.sequence else { continue };
        let (anchor, gap) = &anchors[&id];
        if seen.insert(id, ()).is_none() {
            o.frame.position = PositionMode::Absolute;
            o.frame.x_mm = anchor.x_mm;
            o.frame.gap_mm = anchor.gap_mm;
        } else {
            if o.frame.position == PositionMode::Absolute {
                o.frame.gap_mm = *gap;
            }
            o.frame.position = PositionMode::Relative;
            o.frame.x_mm = 0.0;
        }
    }
}

/// Move a member between sequences, or change a free object's list order.
/// All mutations happen on a candidate document so errors leave no partial edit.
pub fn reorder(elements: &mut Vec<LabelElement>, from: usize, to: usize) -> Result<()> {
    if from >= elements.len() || to >= elements.len() {
        return Err(RenderError::Layout("reorder index is out of range".into()));
    }
    if from == to {
        return Ok(());
    }
    validate(elements, 11)?;
    let mut candidate = elements.clone();
    materialize(&mut candidate)?;
    let source = candidate[from].as_object().unwrap().sequence;
    let target = candidate[to].as_object().unwrap().sequence;
    let anchors = anchors(&candidate);
    let mut item = candidate.remove(from);
    if let (Some(source), Some(target)) = (source, target)
        && source != target
    {
        let object = item.as_object_mut().unwrap();
        object.sequence = Some(target);
        object.frame.position = PositionMode::Relative;
        object.frame.gap_mm = anchors[&target].1;
    }
    candidate.insert(to, item);
    normalize(&mut candidate, &anchors);
    validate(&candidate, 11)?;
    *elements = candidate;
    Ok(())
}

/// Unlink a member without moving any other object's physical allocation.
pub fn detach(elements: &mut [LabelElement], index: usize) -> Result<()> {
    if index >= elements.len() {
        return Err(RenderError::Layout("detach index is out of range".into()));
    }
    validate(elements, 11)?;
    materialize(elements)?;
    let Some(chain) = members(elements, index) else {
        return Ok(());
    };
    let mut cursor = LayoutCursor::default();
    let positions: Vec<_> = elements
        .iter()
        .map(|e| {
            let o = e.as_object().unwrap();
            cursor.place(&o.frame, o.sequence)
        })
        .collect::<Result<_>>()?;
    let member = chain.iter().position(|i| *i == index).unwrap();
    if let Some(&next) = chain.get(member + 1) {
        let previous = member.checked_sub(1).map(|p| chain[p]);
        let right =
            previous.map(|p| positions[p] + elements[p].as_object().unwrap().frame.width_mm);
        let frame = &mut elements[next].as_object_mut().unwrap().frame;
        if let Some(right) = right {
            frame.gap_mm = positions[next] - right;
        } else {
            frame.position = PositionMode::Absolute;
            frame.x_mm = positions[next];
        }
    }
    let object = elements[index].as_object_mut().unwrap();
    object.sequence = None;
    object.frame.position = PositionMode::Absolute;
    object.frame.x_mm = positions[index];
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        document::{LabelDocument, LabelObject},
        layout::render_document,
        text::TextRenderer,
    };

    fn fixture() -> Vec<LabelElement> {
        (0..7)
            .map(|i| {
                LabelElement::Object(LabelObject::text(
                    format!("P{i}"),
                    TextBox {
                        width_mm: 10.0,
                        height_mm: 8.0,
                        position: if [0, 3, 6].contains(&i) {
                            PositionMode::Absolute
                        } else {
                            PositionMode::Relative
                        },
                        x_mm: match i {
                            3 => 60.0,
                            6 => 120.0,
                            _ => 0.0,
                        },
                        gap_mm: if i < 3 { 3.0 } else { 2.8 },
                        ..TextBox::default()
                    },
                ))
            })
            .collect()
    }

    fn positions(elements: &[LabelElement]) -> Vec<f64> {
        let mut cursor = LayoutCursor::default();
        elements
            .iter()
            .map(|e| {
                let o = e.as_object().unwrap();
                cursor.place(&o.frame, o.sequence).unwrap()
            })
            .collect()
    }

    fn document(elements: Vec<LabelElement>) -> LabelDocument {
        LabelDocument {
            version: 11,
            tape_width_mm: 12,
            dpi: 180,
            font_name: "Test".into(),
            font_margin: 0,
            flip_h: false,
            flip_v: false,
            elements,
        }
    }

    #[test]
    fn independent_rows_can_cross_sequences_without_rebinding_or_reflow() {
        let mut elements = fixture();
        let before = positions(&elements);
        reorder(&mut elements, 6, 1).unwrap();
        assert_eq!(
            positions(&elements),
            vec![
                before[0], before[6], before[1], before[2], before[3], before[4], before[5]
            ]
        );
        assert_eq!(members(&elements, 0), Some(vec![0, 2, 3]));
        assert!(elements[1].as_object().unwrap().sequence.is_none());
        // Reorder after a free item has been interleaved in this chain.
        reorder(&mut elements, 3, 0).unwrap();
        assert_eq!(members(&elements, 0), Some(vec![0, 1, 3]));
        assert_eq!(positions(&elements)[2], 120.0);
    }

    #[test]
    fn cross_sequence_moves_preserve_both_origins_and_use_destination_gap() {
        let mut elements = fixture();
        reorder(&mut elements, 2, 3).unwrap();
        assert_eq!(members(&elements, 0), Some(vec![0, 1]));
        assert_eq!(members(&elements, 2), Some(vec![2, 3, 4, 5]));
        let p = positions(&elements);
        assert_eq!(&p[..3], &[0.0, 13.0, 60.0]);
        assert!((p[3] - 72.8).abs() < 1e-9);
        assert!((p[4] - 85.6).abs() < 1e-9);
        assert_eq!(p[6], 120.0);
        let mut elements = fixture();
        reorder(&mut elements, 3, 2).unwrap();
        let p = positions(&elements);
        assert_eq!(&p[..4], &[0.0, 13.0, 26.0, 39.0]);
        assert_eq!(p[4], 60.0);
        assert!((p[5] - 72.8).abs() < 1e-9);
    }

    #[test]
    fn detaching_head_middle_or_tail_keeps_every_other_allocation_in_place() {
        for index in 0..7 {
            let mut elements = fixture();
            let before = positions(&elements);
            detach(&mut elements, index).unwrap();
            assert_eq!(positions(&elements), before, "detach {index}");
            assert!(elements[index].as_object().unwrap().sequence.is_none());
            validate(&elements, 11).unwrap();
            elements[index].as_object_mut().unwrap().frame.x_mm += 4.0;
            let after = positions(&elements);
            for i in 0..7 {
                assert!((after[i] - before[i] - if i == index { 4.0 } else { 0.0 }).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn repeated_moves_round_trip_and_keep_complete_object_payloads() {
        for from in 0..7 {
            for to in 0..7 {
                let mut elements = fixture();
                let payloads: Vec<_> = elements
                    .iter()
                    .map(|e| e.as_object().unwrap().content.clone())
                    .collect();
                reorder(&mut elements, from, to).unwrap();
                if from != to {
                    validate(&elements, 11).unwrap();
                }
                let moved = elements[to].as_object().unwrap();
                assert_eq!(moved.content, payloads[from]);
                assert_eq!(moved.frame.width_mm, 10.0);
                let doc = document(elements);
                assert_eq!(
                    LabelDocument::from_toml_str(&doc.to_toml_string().unwrap()).unwrap(),
                    doc
                );
            }
        }
        let mut elements = fixture();
        for i in 0..100 {
            reorder(&mut elements, i % 7, (i * 3 + 1) % 7).unwrap();
        }
        validate(&elements, 11).unwrap();
    }

    #[test]
    fn explicit_membership_preserves_legacy_pixels_and_uses_the_same_dpi_geometry() {
        let original = document(fixture());
        let mut linked = original.clone();
        materialize(&mut linked.elements).unwrap();
        for dpi in [180, 360] {
            let mut renderer = TextRenderer::with_test_font();
            let a =
                render_document(&original, u32::from(dpi) * 76 / 180, dpi, &mut renderer).unwrap();
            let b =
                render_document(&linked, u32::from(dpi) * 76 / 180, dpi, &mut renderer).unwrap();
            assert_eq!(a.bitmap, b.bitmap);
            reorder(&mut linked.elements, 6, 1).unwrap();
            let c =
                render_document(&linked, u32::from(dpi) * 76 / 180, dpi, &mut renderer).unwrap();
            let converted = crate::layout::convert_to_objects(
                &linked,
                u32::from(dpi) * 76 / 180,
                dpi,
                &mut renderer,
            )
            .unwrap();
            let flattened =
                render_document(&converted, u32::from(dpi) * 76 / 180, dpi, &mut renderer).unwrap();
            assert_eq!(c.bitmap, flattened.bitmap);
            let x = positions(&linked.elements);
            for bound in c.bounds {
                assert_eq!(
                    bound.x,
                    (x[bound.element_index] * f64::from(dpi) / 25.4).round() as u32
                );
            }
            linked = original.clone();
            materialize(&mut linked.elements).unwrap();
        }
    }

    #[test]
    fn malformed_links_and_old_reader_versions_are_rejected() {
        let mut elements = fixture();
        materialize(&mut elements).unwrap();
        assert!(validate(&elements, 10).is_err());
        let text = document(elements.clone()).to_toml_string().unwrap();
        assert!(
            LabelDocument::from_toml_str(&text.replacen("version = 11", "version = 10", 1))
                .is_err()
        );
        for id in [0, 99] {
            let mut bad = elements.clone();
            bad[1].as_object_mut().unwrap().sequence = Some(id);
            assert!(validate(&bad, 11).is_err());
        }
        let mut bad = elements.clone();
        bad[1].as_object_mut().unwrap().frame.position = PositionMode::Absolute;
        assert!(validate(&bad, 11).is_err());
        let before = elements.clone();
        assert!(reorder(&mut elements, 9, 1).is_err());
        assert_eq!(elements, before);
    }
}
