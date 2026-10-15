// SPDX-License-Identifier: MIT
//! Self-contained symbol identity carried inside ordinary SVG image bytes.

/// Versioned namespace for metadata that opts an SVG into symbol rendering:
/// black artwork on white, threshold conversion, and no automatic inversion.
pub const NAMESPACE: &str = "urn:ptouch-rs:symbol:1";

/// Identity and a readable name, independent of any installed catalogue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolMetadata {
    pub library: String,
    pub id: String,
    pub name: String,
    pub version: String,
}

/// Read a symbol marker from the root SVG's metadata. Ordinary images, malformed
/// markers, and unrelated XML keep their existing image behavior.
pub fn metadata(bytes: &[u8]) -> Option<SymbolMetadata> {
    let text = std::str::from_utf8(bytes).ok()?;
    if !text.contains(NAMESPACE) {
        return None;
    }
    let document = roxmltree::Document::parse(text).ok()?;
    let root = document.root_element();
    if !root.has_tag_name(("http://www.w3.org/2000/svg", "svg")) {
        return None;
    }
    let node = root
        .children()
        .filter(|node| node.has_tag_name(("http://www.w3.org/2000/svg", "metadata")))
        .flat_map(|node| node.children())
        .find(|node| node.has_tag_name((NAMESPACE, "symbol")))?;
    let field = |name| {
        node.attribute(name)
            .filter(|value| !value.is_empty() && value.len() <= 256)
            .map(str::to_owned)
    };
    Some(SymbolMetadata {
        library: field("library")?,
        id: field("id")?,
        name: field("name")?,
        version: field("version")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const FILLED_SYMBOL: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><metadata><symbol xmlns="urn:ptouch-rs:symbol:1" library="test" id="filled" name="Filled &amp; square" version="1"/></metadata><path d="M1 1H23V23H1Z"/></svg>"#;

    #[test]
    fn identity_is_read_from_embedded_svg_without_a_catalogue() {
        assert_eq!(
            metadata(FILLED_SYMBOL),
            Some(SymbolMetadata {
                library: "test".into(),
                id: "filled".into(),
                name: "Filled & square".into(),
                version: "1".into(),
            })
        );
    }

    #[test]
    fn unrelated_or_invalid_markers_do_not_change_image_behavior() {
        for bytes in [
            b"not an image".as_slice(),
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"><!-- urn:ptouch-rs:symbol:1 --></svg>",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"><metadata><symbol xmlns=\"urn:ptouch-rs:symbol:1\" library=\"mdi\"/></metadata></svg>",
            b"<html xmlns=\"http://www.w3.org/2000/svg\"><metadata><symbol xmlns=\"urn:ptouch-rs:symbol:1\" library=\"mdi\" id=\"x\" name=\"X\" version=\"1\"/></metadata></html>",
        ] {
            assert_eq!(metadata(bytes), None);
        }
    }

    #[test]
    fn filled_symbols_preserve_black_artwork_and_ordinary_images_keep_auto_invert() {
        use crate::image_loader::{BinarizeMode, ImageLoadOptions, load_image_from_reader};
        for height in [24, 76, 150] {
            let options = ImageLoadOptions {
                // Symbol policy must also win when a caller requested dithering.
                binarize: BinarizeMode::Dither,
                target_height: Some(height),
                auto_invert: true,
            };
            let bitmap = load_image_from_reader(FILLED_SYMBOL, &options).unwrap();
            assert!(bitmap.get_pixel(height / 2, height / 2));
            assert!(!bitmap.get_pixel(0, 0));
        }
        let ordinary = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M1 1H23V23H1Z"/></svg>"#;
        let bitmap =
            load_image_from_reader(ordinary.as_slice(), &ImageLoadOptions::default()).unwrap();
        assert!(!bitmap.get_pixel(12, 12));
        assert!(bitmap.get_pixel(0, 0));
    }

    #[test]
    fn saved_symbols_render_without_a_catalogue_at_each_print_resolution() {
        use crate::document::{
            LabelDocument, LabelElement, LabelObject, TextBox, render_document_at_resolution,
        };
        use crate::text::TextRenderer;
        let mut renderer = TextRenderer::new();
        for legacy in [true, false] {
            for resolution in [
                ptouch_core::PrintResolution::square(180),
                ptouch_core::PrintResolution {
                    head_dpi: 180,
                    feed_dpi: 360,
                },
                ptouch_core::PrintResolution::square(360),
                ptouch_core::PrintResolution {
                    head_dpi: 360,
                    feed_dpi: 180,
                },
            ] {
                let pixels = if resolution.head_dpi == 180 { 76 } else { 150 };
                let height = f64::from(pixels) * 25.4 / f64::from(resolution.head_dpi);
                let element = if legacy {
                    LabelElement::image_from_bytes(None, FILLED_SYMBOL.to_vec())
                } else {
                    LabelElement::Object(LabelObject::image_from_bytes(
                        None,
                        FILLED_SYMBOL.to_vec(),
                        TextBox {
                            width_mm: height,
                            height_mm: height,
                            ..Default::default()
                        },
                    ))
                };
                let doc = LabelDocument {
                    version: if legacy { 1 } else { 3 },
                    tape_width_mm: 12,
                    dpi: 180,
                    font_name: "sans-serif".into(),
                    font_margin: 0,
                    flip_h: false,
                    flip_v: false,
                    elements: vec![element],
                };
                let reopened =
                    LabelDocument::from_toml_str(&doc.to_toml_string().unwrap()).unwrap();
                assert_eq!(
                    reopened.elements[0].display_name(),
                    "Symbol: Filled & square"
                );
                let bitmap =
                    render_document_at_resolution(&reopened, pixels, resolution, &mut renderer)
                        .unwrap()
                        .bitmap
                        .unwrap();
                assert_eq!(bitmap.height(), pixels);
                let expected_width = (f64::from(pixels) * f64::from(resolution.feed_dpi)
                    / f64::from(resolution.head_dpi))
                .round() as u32;
                assert_eq!(bitmap.width(), expected_width);
                assert!(bitmap.get_pixel(bitmap.width() / 2, pixels / 2));
                assert!(!bitmap.get_pixel(0, 0));
            }
        }
    }

    #[test]
    fn legacy_symbols_rasterize_vector_detail_at_the_requested_print_size() {
        use crate::document::{LabelElement, render_elements};
        use crate::image_loader::{ImageLoadOptions, load_image_from_reader};
        let svg = std::str::from_utf8(FILLED_SYMBOL)
            .unwrap()
            .replace("M1 1H23V23H1Z", "M1 1L23 20L22 21L1 2Z");
        let element = LabelElement::image_from_bytes(None, svg.as_bytes().to_vec());
        let LabelElement::Image {
            bitmap: Some(cache),
            ..
        } = &element
        else {
            panic!("missing eager cache")
        };
        assert_eq!(cache.height(), 24);
        let mut renderer = crate::text::TextRenderer::new();
        for height in [76, 150] {
            let expected = load_image_from_reader(
                svg.as_bytes(),
                &ImageLoadOptions {
                    target_height: Some(height),
                    ..Default::default()
                },
            )
            .unwrap();
            let rendered =
                render_elements(std::slice::from_ref(&element), height, "", 0, &mut renderer)
                    .unwrap()
                    .unwrap();
            let enlarged_cache = cache.scale_to_height(height);
            assert!((0..height).any(|x| {
                (0..height).any(|y| expected.get_pixel(x, y) != enlarged_cache.get_pixel(x, y))
            }));
            for y in 0..height {
                for x in 0..height {
                    assert_eq!(
                        rendered.get_pixel(x, y),
                        expected.get_pixel(x, y),
                        "({x}, {y}) at {height} dots"
                    );
                }
            }
        }
    }
}
