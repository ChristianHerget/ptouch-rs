# Rust raster API migration

`print_raster` now requires the actual input raster resolution. This is a
breaking Rust API change. CLI flags and saved profile/layout formats are
unchanged.

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
the same arguments and returns `Option<RenderedLabel>`. This contains the print
`bitmap` and `element_spans`: optional feed-column ranges indexed by source
element. Skipped elements have no range; a draft element can have an empty
range when none of its columns is sampled. Ranges are collected in the same
rendering pass and follow the final resolution, including global draft sampling.
Use `RenderedLabel::mirrored(horizontal, vertical)` to mirror the pixels and
ranges together. The bitmap-only rendering functions remain available.

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
