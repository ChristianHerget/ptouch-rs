# Rust raster API migration

`print_raster` now requires the actual input raster resolution. This is a
breaking Rust API change. CLI quality flags retain their existing meaning. Printer profiles with calibration
rules use format v2; profiles without them retain v1. New GUI
objects use upstream format v7; unchanged legacy documents keep their version.

Use a model's shared quality metadata to select the mode and render its input:

```rust,ignore
use ptouch_core::{PrintQuality, printer_model::PrinterModel};
use ptouch_render::document::render_elements_at_resolution;

let quality = PrintQuality::HighRes;
let model = PrinterModel::find("PT-P750W").unwrap();
let resolution = model.quality.mode(quality)?.resolution;
let bitmap = render_elements_at_resolution(
    &elements,
    printable_height,
    &font_name,
    font_margin,
    &mut renderer,
    resolution,
)?;
// Pack the bitmap using the connected printer's raster width and media band.
printer.print_raster(&lines, resolution, chain_print, precut, quality)?;
```

Document coordinates remain square head-DPI pixels. The renderer changes the
feed-axis density while retaining head height and physical length. Apply whole
label mirroring once after rendering, then pack the resulting bitmap. The GUI
uses the selected print resolution for previews, printing, and image exports.

For interactive previews, `render_elements_with_layout_at_resolution` accepts
the same arguments and returns `Option<RenderedElements>`. This contains the print
`bitmap` and `element_spans`: optional feed-column ranges indexed by source
element. Skipped elements have no range; a draft element can have an empty
range when none of its columns is sampled. Ranges are collected in the same
rendering pass and follow the final resolution, including global draft sampling.
Use `RenderedElements::mirrored(horizontal, vertical)` to mirror the pixels and
ranges together. The bitmap-only rendering functions remain available.

For complete saved layouts, including v2 text boxes and v3–v7 objects, use
`render_document_at_resolution(&document, printable_height, resolution, &mut renderer)`.
The square-DPI `render_document` entrypoint remains available for offline rendering.
Physical X coordinates use feed DPI; Y coordinates use head DPI. Rotations are
rendered in square source pixels before sampling into printer dots. Legacy
unboxed pixel dimensions retain their previous rendering behavior.

Document rendering returns `RenderedLabel` with an optional bitmap, slot `bounds`,
oriented `content_bounds`, and clipping `diagnostics`. It applies the document's
whole-label mirrors to both pixels and geometry. Callers must not mirror again.
The former legacy bitmap/span result is now named `RenderedElements` to keep
these two interfaces distinct.

Convert a span's column count with `resolution.length_mm` to measure its
contribution to preview length, including whitespace. Preview dimensions omit
printer-added feed and cut margins. Full tape margins, dimension lines, and
selection guides are GUI overlays and do not change the print bitmap or exports.

| Model | Standard | High resolution | Draft |
| --- | --- | --- | --- |
| PT-P750W | 180×180 | 180×360, laminated tape | Unsupported |
| PT-9500PC, PT-9700PC | 360×360 | 360×720, HG tape | 360×180, HG tape |
| PT-P900W, PT-P950NW, PT-E850TKW | 360×360 | Unsupported | Unsupported |
| Other implemented models | Model's square head DPI | Unsupported | Unsupported |

`PrintResolution { head_dpi, feed_dpi }` accompanies every raster. USB and
network `print_raster` accept it immediately after `lines`; Bluetooth accepts
`print_raster(lines, resolution)` with 180×180 only. The selected mode rejects
mismatched metadata before sending job data. Callers must supply truthful
metadata: the driver cannot infer physical DPI from raw bytes.

`protocol::build_print_job` also takes `resolution` and a `QualityProfile` and
returns `Result`. It sends every supplied column once. Legacy high resolution
no longer duplicates standard-resolution columns, and draft no longer drops
columns in the protocol. Render or resample before calling the driver. Use
`resolution.length_mm(column_count)` to calculate physical feed length.

`DeviceFlags::LEGACY_HIRES` and `DeviceFlags::P750_HIRES` are removed, as are
`PrinterModel`'s quality booleans and `feed_dpi` helper. Query
`model.quality.modes` or `model.quality.mode(quality)` instead. Each `QualityMode`
defines resolution and media requirements. `QualityCommands` separately
selects the USB command sequence; frontends do not select commands by model
name. Standard-mode command streams retain their existing behavior.


Inline text styles use the optional `ObjectContent::Text.styles` vector of
`styled_text::StyleSpan`. Ranges count Unicode scalar values and use an exclusive
end. Unstyled constructors should supply `styles: Vec::new()`. Styles require
layout v6; custom line spacing requires v5. Serialization raises the layout
version only when needed, and unstyled text retains its previous rendering path.

`LabelObject` now includes optional `layout: Option<object_layout::LayoutOptions>`.
Supply `layout: None` in legacy constructors. `LayoutOptions` selects free or
flow placement, fixed or content-sized width, containment and allocation padding.
These policies require layout v7. Both document render entrypoints resolve
automatic widths at the saved design DPI before sampling at the requested
head/feed resolution. Fixed allocations retain their template positions;
content-sized flow allocations reflow after placeholder substitution.

Apply `calibration::apply_calibration(&bitmap, resolution.feed_dpi, offset_mm)`
after document rendering/mirroring and before raster encoding. It returns a new
bitmap without changing layout bounds. Negative offsets can remove only blank
leading columns. Select any saved correction using the actual quality and cut
commands for that label; chained copies may need different rules. Convert the
corrected raster to square pixels only afterward when exporting a GUI image.
