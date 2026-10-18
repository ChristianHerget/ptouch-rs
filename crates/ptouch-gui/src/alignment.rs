// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Explicit content alignment in the allocation's unrotated tape axes.

use ptouch_render::{
    document::{ContentBox, LabelObject},
    object_layout::Placement,
};

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
