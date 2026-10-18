// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Explicit content alignment in the allocation's unrotated tape axes.

use ptouch_render::{
    document::{ContentBox, LabelObject},
    object_layout::{self, Placement},
};

#[derive(Clone, Copy)]
pub(crate) struct ContentAlignment {
    pub horizontal: Option<f64>,
    pub vertical: Option<f64>,
}

impl crate::state::AppState {
    /// Apply the command after numeric transforms and automatic text sizing.
    /// The candidate remains local until the complete edit passes validation.
    pub(crate) fn align_candidate(
        &mut self,
        index: usize,
        candidate: &mut LabelObject,
        alignment: ContentAlignment,
    ) -> Result<(), String> {
        let resolved = self
            .resolved_content_box(index)
            .ok_or("Object geometry is unavailable")?;
        candidate.content_box.get_or_insert(resolved);
        // Measurement must precede alignment. An old offset may lie outside the
        // resized allocation; check containment only after placing the content.
        let constrain = candidate.layout.as_ref().map(|p| p.constrain);
        if let Some(policy) = &mut candidate.layout {
            policy.constrain = false;
        }
        let result = object_layout::resolve_object(
            candidate,
            self.document_version.max(4),
            self.design_dpi,
            &self.font_name,
            self.scene_measurement_renderer
                .get_or_insert_with(ptouch_render::text::TextRenderer::new),
        );
        if let Some(policy) = &mut candidate.layout {
            policy.constrain = constrain.unwrap();
        }
        result.map_err(|error| error.to_string())?;
        let resolved = candidate.content_box.as_ref().unwrap().clone();
        align_content(
            candidate,
            &resolved,
            alignment.horizontal,
            alignment.vertical,
        );
        object_layout::check_allocation(candidate).map_err(|error| error.to_string())
    }
}

/// Align one axis without resizing or moving the allocation. `None` keeps
/// the other axis unchanged; 0, 0.5 and 1 select start, centre and end.
pub fn align_content(
    object: &mut LabelObject,
    resolved: &ContentBox,
    horizontal: Option<f64>,
    vertical: Option<f64>,
) -> bool {
    if object
        .layout
        .as_ref()
        .is_some_and(|p| p.placement == Placement::Free)
    {
        return false;
    }
    let mut content = object.content_box.as_ref().unwrap_or(resolved).clone();
    let (sin, cos) = f64::from(object.rotation).to_radians().sin_cos();
    let width = content.width_mm * cos.abs() + content.height_mm * sin.abs();
    let height = content.width_mm * sin.abs() + content.height_mm * cos.abs();
    let margin = object.layout.as_ref().map_or(0.0, |p| p.padding_mm);
    let frame = &object.frame;
    for (fraction, available, extent, offset, flipped) in [
        (
            horizontal,
            frame.width_mm,
            width,
            &mut content.offset_x_mm,
            object.flip_h,
        ),
        (
            vertical,
            frame.height_mm,
            height,
            &mut content.offset_y_mm,
            object.flip_v,
        ),
    ] {
        if let Some(fraction) = fraction {
            *offset = (available - 2.0 * margin - extent)
                * (fraction - 0.5)
                * if flipped { -1.0 } else { 1.0 };
        }
    }
    if object.content_box.as_ref() == Some(&content) {
        return false;
    }
    object.content_box = Some(content);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptouch_render::{
        document::TextBox,
        object_layout::{LayoutOptions, content_extent},
    };

    #[test]
    fn all_alignment_edges_respect_rotations_reflections_and_inner_margin() {
        for rotation in [0.0, 37.0, 90.0, 180.0, 270.0] {
            for flip_h in [false, true] {
                for flip_v in [false, true] {
                    for horizontal in [0.0, 0.5, 1.0] {
                        for vertical in [0.0, 0.5, 1.0] {
                            let mut object = LabelObject::text(
                                "Port",
                                TextBox {
                                    width_mm: 20.0,
                                    height_mm: 12.0,
                                    ..TextBox::default()
                                },
                            );
                            object.rotation = rotation;
                            object.flip_h = flip_h;
                            object.flip_v = flip_v;
                            object.layout = Some(LayoutOptions {
                                placement: Placement::Flow,
                                padding_mm: 0.5,
                                ..LayoutOptions::default()
                            });
                            let resolved = ContentBox {
                                width_mm: 8.0,
                                height_mm: 0.3,
                                offset_x_mm: 0.17,
                                offset_y_mm: -0.23,
                            };
                            let frame = object.frame.clone();
                            assert!(align_content(
                                &mut object,
                                &resolved,
                                Some(horizontal),
                                Some(vertical)
                            ));
                            let (x, y, w, h) = content_extent(&object).unwrap();
                            assert!((x - (-9.5 + (19.0 - w) * horizontal)).abs() < 1e-9);
                            assert!((y - (-5.5 + (11.0 - h) * vertical)).abs() < 1e-9);
                            assert_eq!(object.frame, frame);
                            assert!(!align_content(
                                &mut object,
                                &resolved,
                                Some(horizontal),
                                Some(vertical)
                            ));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn horizontal_center_keeps_vertical_offset_and_free_objects_unchanged() {
        let resolved = ContentBox {
            width_mm: 8.0,
            height_mm: 0.3,
            offset_x_mm: 1.0,
            offset_y_mm: -0.5,
        };
        let mut object = LabelObject::text("", TextBox::default());
        object.rotation = 90.0;
        assert!(align_content(&mut object, &resolved, Some(0.5), None));
        let content = object.content_box.as_ref().unwrap();
        assert_eq!(content.offset_x_mm, 0.0);
        assert_eq!(content.offset_y_mm, -0.5);
        assert_eq!((content.width_mm, content.height_mm), (8.0, 0.3));
        object.layout = Some(LayoutOptions::default());
        let before = object.clone();
        assert!(!align_content(&mut object, &resolved, Some(1.0), Some(0.0)));
        assert_eq!(object, before);
    }

    #[test]
    fn pending_auto_width_resize_is_resolved_before_all_six_alignment_commands() {
        use ptouch_render::{
            document::LabelElement, object_layout::WidthSizing, scene::SceneOptions,
        };

        for scene in [false, true] {
            let mut state = crate::state::AppState {
                document_version: if scene { 12 } else { 7 },
                ..Default::default()
            };
            for rotation in [0.0, 25.0] {
                for flip_h in [false, true] {
                    for flip_v in [false, true] {
                        let mut before = LabelObject::text(
                            "PORT 01",
                            TextBox {
                                x_mm: 1.0,
                                width_mm: 14.4,
                                height_mm: 10.0,
                                ..TextBox::default()
                            },
                        );
                        before.rotation = rotation;
                        before.flip_h = flip_h;
                        before.flip_v = flip_v;
                        before.content_box = Some(ContentBox {
                            width_mm: 13.8,
                            height_mm: 4.0,
                            offset_x_mm: 0.0,
                            offset_y_mm: 0.0,
                        });
                        before.layout = Some(LayoutOptions {
                            placement: Placement::Flow,
                            width: WidthSizing::Content,
                            constrain: false,
                            padding_mm: 0.0,
                        });
                        before.scene = scene.then(|| {
                            Box::new(SceneOptions {
                                id: 1,
                                layer: 0,
                                anchor: None,
                            })
                        });
                        object_layout::resolve_object(
                            &mut before,
                            state.document_version,
                            state.design_dpi,
                            &state.font_name,
                            state
                                .scene_measurement_renderer
                                .get_or_insert_with(ptouch_render::text::TextRenderer::new),
                        )
                        .unwrap();
                        for horizontal in [false, true] {
                            for fraction in [0.0, 0.5, 1.0] {
                                state.elements = vec![LabelElement::Object(before.clone())];
                                let mut candidate = before.clone();
                                let content = candidate.content_box.as_mut().unwrap();
                                content.height_mm *= 12.0 / content.width_mm;
                                content.width_mm = 12.0;
                                state
                                    .constrain_transform(0, &before, &mut candidate)
                                    .unwrap();
                                assert!(
                                    candidate.content_box.as_ref().unwrap().offset_x_mm.abs() > 1.0
                                );
                                state
                                    .align_candidate(
                                        0,
                                        &mut candidate,
                                        ContentAlignment {
                                            horizontal: horizontal.then_some(fraction),
                                            vertical: (!horizontal).then_some(fraction),
                                        },
                                    )
                                    .unwrap();
                                let aligned = candidate.clone();
                                state.apply_object_edit(0, candidate).unwrap();
                                let result = state.elements[0].as_object().unwrap();
                                // A later scene transaction and render pass must not undo the command.
                                assert_eq!(result.content_box, aligned.content_box);
                                let mut rendered = result.clone();
                                object_layout::resolve_object(
                                    &mut rendered,
                                    state.document_version,
                                    state.design_dpi,
                                    &state.font_name,
                                    state.scene_measurement_renderer.as_mut().unwrap(),
                                )
                                .unwrap();
                                assert_eq!(&rendered, result);
                                let (x, y, w, h) = content_extent(result).unwrap();
                                let (start, extent, available) = if horizontal {
                                    (x, w, result.frame.width_mm)
                                } else {
                                    (y, h, result.frame.height_mm)
                                };
                                assert!(
                                    (start - (-available / 2.0 + (available - extent) * fraction))
                                        .abs()
                                        < 1e-9
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn centered_spacer_lines_render_in_the_allocation_center_at_both_dpis() {
        use ptouch_render::{
            document::{LabelElement, ObjectContent},
            text::TextRenderer,
        };
        let mut renderer = TextRenderer::new();
        for dpi in [180, 360] {
            for flip_h in [false, true] {
                for flip_v in [false, true] {
                    let mut object = LabelObject::text(
                        "",
                        TextBox {
                            x_mm: 14.4,
                            width_mm: 2.8,
                            height_mm: 10.0,
                            ..TextBox::default()
                        },
                    );
                    object.content = ObjectContent::Line {
                        pattern: Default::default(),
                    };
                    object.rotation = 90.0;
                    object.flip_h = flip_h;
                    object.flip_v = flip_v;
                    let resolved = ContentBox {
                        width_mm: 8.0,
                        height_mm: 0.3,
                        offset_x_mm: 0.7,
                        offset_y_mm: -0.4,
                    };
                    assert!(align_content(&mut object, &resolved, Some(0.5), Some(0.5)));
                    let mut doc = crate::state::AppState::default().document();
                    doc.version = 11;
                    doc.elements = vec![LabelElement::Object(object)];
                    let rendered = ptouch_render::document::render_document(
                        &doc,
                        (10.0 * f64::from(dpi) / 25.4_f64).round() as u32,
                        dpi,
                        &mut renderer,
                    )
                    .unwrap();
                    let bitmap = rendered.bitmap.unwrap();
                    let mut xs = Vec::new();
                    for x in 0..bitmap.width() {
                        if (0..bitmap.height()).any(|y| bitmap.get_pixel(x, y)) {
                            xs.push(x);
                        }
                    }
                    assert!(!xs.is_empty());
                    let centre = f64::from(xs[0] + xs[xs.len() - 1] + 1) / 2.0;
                    assert!((centre - 15.8 * f64::from(dpi) / 25.4).abs() <= 1.0);
                }
            }
        }
    }
}
