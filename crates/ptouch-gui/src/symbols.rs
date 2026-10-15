// SPDX-License-Identifier: MIT
//! Offline catalogue, symbol insertion/replacement, and the modal browser.

use std::collections::{HashMap, VecDeque};
use std::sync::OnceLock;

use egui::{Color32, Id, Rect, Sense, TextureHandle, TextureOptions, Vec2};
use ptouch_render::bitmap::LabelBitmap;
use ptouch_render::document::{LabelElement, LabelObject, ObjectContent, PositionMode, TextBox};
use ptouch_render::image_loader::{ImageLoadOptions, load_image_from_reader};
use ptouch_render::symbol;
use ptouch_render::text::TextRenderer;
use serde::Deserialize;

use crate::state::AppState;

#[derive(Deserialize)]
pub struct Catalogue {
    pub version: String,
    pub categories: Vec<String>,
    pub icons: Vec<Symbol>,
}

#[derive(Deserialize)]
pub struct Symbol {
    pub id: String,
    pub name: String,
    aliases: Vec<String>,
    categories: Vec<String>,
    author: String,
    path: String,
    #[serde(skip)]
    search: String,
}

pub fn catalogue() -> &'static Catalogue {
    static CATALOGUE: OnceLock<Catalogue> = OnceLock::new();
    CATALOGUE.get_or_init(|| {
        let mut catalogue: Catalogue =
            serde_json::from_str(include_str!("../assets/symbols/mdi.json"))
                .expect("validated bundled symbol catalogue");
        for icon in &mut catalogue.icons {
            icon.search = normalize(
                &std::iter::once(icon.id.as_str())
                    .chain(std::iter::once(icon.name.as_str()))
                    .chain(icon.aliases.iter().map(String::as_str))
                    .chain(icon.categories.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        catalogue
    })
}

fn normalize(value: &str) -> String {
    value.to_lowercase().replace('-', " ")
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

impl Symbol {
    /// Keep both identity and attribution in the image, not a local file path.
    pub fn svg(&self) -> Vec<u8> {
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\"><title>{name}</title><metadata><symbol xmlns=\"{namespace}\" library=\"mdi\" id=\"{id}\" name=\"{name}\" version=\"{version}\" author=\"{author}\" license=\"Apache-2.0\" source=\"https://pictogrammers.com/library/mdi/\"/></metadata><path fill=\"#000000\" d=\"{path}\"/></svg>",
            name = escape(&self.name),
            namespace = symbol::NAMESPACE,
            id = escape(&self.id),
            version = escape(&catalogue().version),
            author = escape(&self.author),
            path = escape(&self.path),
        )
        .into_bytes()
    }
}

fn find(id: &str) -> Option<usize> {
    catalogue()
        .icons
        .binary_search_by(|icon| icon.id.as_str().cmp(id))
        .ok()
}

fn image_bytes(element: &LabelElement) -> Option<&[u8]> {
    match element {
        LabelElement::Image { image_data, .. }
        | LabelElement::Object(LabelObject {
            content: ObjectContent::Image { image_data, .. },
            ..
        }) => Some(image_data),
        _ => None,
    }
}

fn replace_image(element: &mut LabelElement, bytes: Vec<u8>) -> Result<(), String> {
    let (path, image_data, bitmap) = match element {
        LabelElement::Image {
            path,
            image_data,
            bitmap,
            ..
        }
        | LabelElement::Object(LabelObject {
            content:
                ObjectContent::Image {
                    path,
                    image_data,
                    bitmap,
                },
            ..
        }) => (path, image_data, bitmap),
        _ => return Err("Select an image or symbol to replace.".into()),
    };
    let decoded = load_image_from_reader(bytes.as_slice(), &ImageLoadOptions::default())
        .map_err(|error| error.to_string())?;
    *path = None;
    *image_data = bytes;
    *bitmap = Some(decoded);
    Ok(())
}

#[derive(Clone, Copy)]
enum Target {
    Add,
    Replace { element: usize, revision: u64 },
}

#[derive(Default)]
pub struct SymbolPicker {
    target: Option<Target>,
    query: String,
    category: Option<usize>,
    selected: Option<usize>,
    focus_search: bool,
    filter: Option<(String, Option<usize>)>,
    matches: Vec<usize>,
    reset_scroll: bool,
    thumbnails: HashMap<usize, Result<TextureHandle, String>>,
    thumbnail_order: VecDeque<usize>,
    preview: Option<PrintPreview>,
    renderer: Option<TextRenderer>,
    error: Option<String>,
}

struct PrintPreview {
    key: (usize, u64, u32, ptouch_core::PrintResolution),
    texture: Result<TextureHandle, String>,
    size: Vec2,
}

impl SymbolPicker {
    pub fn is_open(&self) -> bool {
        self.target.is_some()
    }

    fn update_filter(&mut self) {
        let filter = (normalize(&self.query), self.category);
        if self.filter.as_ref() == Some(&filter) {
            return;
        }
        let tokens: Vec<_> = filter.0.split_whitespace().collect();
        self.matches = catalogue()
            .icons
            .iter()
            .enumerate()
            .filter(|(_, icon)| {
                self.category
                    .is_none_or(|index| icon.categories.contains(&catalogue().categories[index]))
                    && tokens.iter().all(|token| icon.search.contains(token))
            })
            .map(|(index, _)| index)
            .collect();
        if self
            .selected
            .is_some_and(|index| !self.matches.contains(&index))
        {
            self.selected = None;
        }
        self.filter = Some(filter);
        self.reset_scroll = true;
    }

    fn thumbnail(&mut self, ctx: &egui::Context, index: usize) -> Result<TextureHandle, String> {
        if !self.thumbnails.contains_key(&index) {
            // Bound GPU memory when browsing thousands of symbols. The visible
            // grid remains cached; revisit older entries by rasterizing again.
            if self.thumbnail_order.len() >= 256
                && let Some(oldest) = self.thumbnail_order.pop_front()
            {
                self.thumbnails.remove(&oldest);
            }
            let bitmap = load_image_from_reader(
                catalogue().icons[index].svg().as_slice(),
                &ImageLoadOptions {
                    target_height: Some(64),
                    ..Default::default()
                },
            )
            .map_err(|error| error.to_string());
            let texture = bitmap.map(|bitmap| upload(ctx, format!("symbol-{index}"), &bitmap));
            self.thumbnails.insert(index, texture);
            self.thumbnail_order.push_back(index);
        }
        self.thumbnails[&index].clone()
    }

    fn grid(&mut self, ui: &mut egui::Ui, width: f32, height: f32) {
        let columns = ((width + ui.spacing().item_spacing.x) / (76.0 + ui.spacing().item_spacing.x))
            .floor()
            .max(1.0) as usize;
        let row_height = 82.0;
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("symbol-grid")
            .max_height(height)
            .min_scrolled_height(height)
            .auto_shrink([false, false]);
        if self.reset_scroll {
            let row = self
                .selected
                .and_then(|selected| self.matches.iter().position(|index| *index == selected))
                .map_or(0, |position| position / columns);
            scroll = scroll
                .vertical_scroll_offset(row as f32 * (row_height + ui.spacing().item_spacing.y));
            self.reset_scroll = false;
        }
        scroll.show_rows(
            ui,
            row_height,
            self.matches.len().div_ceil(columns),
            |ui, rows| {
                for row in rows {
                    ui.horizontal(|ui| {
                        let start = row * columns;
                        for offset in 0..columns {
                            let Some(&index) = self.matches.get(start + offset) else {
                                break;
                            };
                            let icon = &catalogue().icons[index];
                            let (rect, response) = ui
                                .allocate_exact_size(egui::vec2(76.0, row_height), Sense::click());
                            response.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::WidgetType::Button,
                                    ui.is_enabled(),
                                    self.selected == Some(index),
                                    &icon.name,
                                )
                            });
                            if response.clicked() {
                                self.selected = Some(index);
                                self.error = None;
                            }
                            if ui.is_rect_visible(rect) {
                                let selected = self.selected == Some(index);
                                let visuals = ui.style().interact_selectable(&response, selected);
                                ui.painter().rect(
                                    rect.shrink(1.0),
                                    4.0,
                                    visuals.bg_fill,
                                    visuals.bg_stroke,
                                    egui::StrokeKind::Inside,
                                );
                                let image = Rect::from_center_size(
                                    rect.center_top() + egui::vec2(0.0, 27.0),
                                    Vec2::splat(44.0),
                                );
                                match self.thumbnail(ui.ctx(), index) {
                                    Ok(texture) => {
                                        ui.painter().image(
                                            texture.id(),
                                            image,
                                            Rect::from_min_max(
                                                egui::Pos2::ZERO,
                                                egui::pos2(1.0, 1.0),
                                            ),
                                            Color32::WHITE,
                                        );
                                    }
                                    Err(_) => {
                                        ui.painter().text(
                                            image.center(),
                                            egui::Align2::CENTER_CENTER,
                                            "?",
                                            egui::FontId::proportional(20.0),
                                            visuals.text_color(),
                                        );
                                    }
                                }
                                let label_rect = Rect::from_min_max(
                                    rect.min + egui::vec2(3.0, 52.0),
                                    rect.max - egui::vec2(3.0, 2.0),
                                );
                                let text = ui.painter().layout(
                                    icon.name.clone(),
                                    egui::FontId::proportional(11.0),
                                    visuals.text_color(),
                                    label_rect.width(),
                                );
                                ui.painter()
                                    .with_clip_rect(label_rect.intersect(ui.clip_rect()))
                                    .galley(
                                        egui::pos2(
                                            label_rect.center().x - text.size().x / 2.0,
                                            label_rect.top(),
                                        ),
                                        text,
                                        visuals.text_color(),
                                    );
                            }
                            response.on_hover_text(format!(
                                "{}\n{}",
                                icon.name,
                                icon.categories.join(", ")
                            ));
                        }
                    });
                }
            },
        );
        if self.matches.is_empty() {
            ui.label("No symbols match. Try another search or category.");
        }
    }

    fn selected_preview(&mut self, ui: &mut egui::Ui, state: &AppState, compact: bool) {
        let Some(index) = self.selected else {
            ui.label("Select a symbol to preview it.");
            return;
        };
        let icon = &catalogue().icons[index];
        ui.strong(&icon.name);
        let resolution = match state.preview_resolution() {
            Ok(resolution) => resolution,
            Err(error) => {
                ui.colored_label(Color32::RED, error.to_string());
                return;
            }
        };
        let key = (
            index,
            state.document_revision,
            state.tape_width_px,
            resolution,
        );
        if self
            .preview
            .as_ref()
            .is_none_or(|preview| preview.key != key)
        {
            let result = self.render_preview(state, index, resolution);
            let (texture, size) = match result {
                Ok(bitmap) => {
                    let size = egui::vec2(
                        bitmap.width() as f32 * resolution.head_dpi as f32
                            / resolution.feed_dpi as f32,
                        bitmap.height() as f32,
                    );
                    (
                        Ok(upload(ui.ctx(), "selected-symbol".into(), &bitmap)),
                        size,
                    )
                }
                Err(error) => (Err(error), Vec2::splat(1.0)),
            };
            self.preview = Some(PrintPreview { key, texture, size });
        }
        let preview = self.preview.as_ref().unwrap();
        match &preview.texture {
            Ok(texture) => {
                let image_height = (ui.available_height() - if compact { 36.0 } else { 58.0 })
                    .clamp(24.0, if compact { 80.0 } else { 190.0 });
                let available = egui::vec2(
                    ui.available_width()
                        .min(if compact { 100.0 } else { 200.0 }),
                    image_height,
                );
                let factor = (available.x / preview.size.x).min(available.y / preview.size.y);
                ui.add(egui::Image::new((texture.id(), preview.size * factor)));
            }
            Err(error) => {
                ui.colored_label(Color32::RED, format!("Preview error: {error}"));
            }
        }
        ui.small(format!(
            "{} × {} DPI",
            resolution.head_dpi, resolution.feed_dpi
        ));
        ui.small(format!(
            "Printable height: {:.2} mm",
            state.printable_height_mm()
        ));
        if !compact {
            ui.small(icon.categories.join(", "));
        }
    }

    fn render_preview(
        &mut self,
        state: &AppState,
        index: usize,
        resolution: ptouch_core::PrintResolution,
    ) -> Result<LabelBitmap, String> {
        let bytes = catalogue().icons[index].svg();
        let mut element = match self.target.unwrap() {
            Target::Add if state.is_free_layout() => {
                let height = state.printable_height_mm();
                LabelElement::Object(LabelObject::image_from_bytes(
                    None,
                    bytes,
                    TextBox {
                        width_mm: height,
                        height_mm: height,
                        ..Default::default()
                    },
                ))
            }
            Target::Add => LabelElement::image_from_bytes(None, bytes),
            Target::Replace { element, .. } => {
                let mut element = state
                    .elements
                    .get(element)
                    .ok_or("The selected object is no longer available.")?
                    .clone();
                replace_image(&mut element, bytes)?;
                if let Some(object) = element.as_object_mut() {
                    // Isolate the object while retaining its vertical position,
                    // frame, content offsets, rotation, and size settings.
                    object.frame.position = PositionMode::Absolute;
                    object.frame.x_mm = 0.0;
                }
                element
            }
        };
        // Relative placement has no predecessor in this isolated preview.
        if let Some(object) = element.as_object_mut() {
            object.frame.gap_mm = 0.0;
        }
        let mut doc = state.document();
        doc.elements = vec![element];
        let rendered = ptouch_render::document::render_document_at_resolution(
            &doc,
            state.tape_width_px,
            resolution,
            self.renderer.get_or_insert_with(TextRenderer::new),
        )
        .map_err(|error| error.to_string())?;
        rendered
            .bitmap
            .ok_or_else(|| "Symbol has no printable pixels at these settings.".into())
    }
}

fn upload(ctx: &egui::Context, name: String, bitmap: &LabelBitmap) -> TextureHandle {
    let rgba = bitmap.to_rgba_image();
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [bitmap.width() as usize, bitmap.height() as usize],
        rgba.as_raw(),
    );
    ctx.load_texture(name, image, TextureOptions::NEAREST)
}

impl AppState {
    pub fn open_symbol_picker(&mut self, replace: Option<usize>) {
        let selected = replace
            .and_then(|index| self.elements.get(index))
            .and_then(image_bytes)
            .and_then(symbol::metadata)
            .filter(|metadata| metadata.library == "mdi")
            .and_then(|metadata| find(&metadata.id));
        self.editor.history.finish_group();
        let picker = &mut self.symbol_picker;
        picker.target = Some(replace.map_or(Target::Add, |element| Target::Replace {
            element,
            revision: self.document_revision,
        }));
        picker.query.clear();
        picker.category = None;
        picker.selected = selected;
        picker.filter = None;
        picker.focus_search = true;
        picker.preview = None;
        picker.error = None;
    }

    fn apply_symbol(&mut self, target: Target, index: usize) -> Result<(), String> {
        let bytes = catalogue()
            .icons
            .get(index)
            .ok_or("Choose a symbol first.")?
            .svg();
        match target {
            Target::Add => {
                if !self.add_image_bytes(None, bytes) {
                    return Err("Could not decode the selected symbol.".into());
                }
            }
            Target::Replace { element, revision } => {
                if self.document_revision != revision {
                    return Err(
                        "The document changed. Close the picker and select the object again."
                            .into(),
                    );
                }
                let mut replacement = self
                    .elements
                    .get(element)
                    .ok_or("The selected object is no longer available.")?
                    .clone();
                replace_image(&mut replacement, bytes)?;
                let before = self.content_snapshot();
                self.elements[element] = replacement;
                self.record_content(before, "Change symbol", None, 0.0);
            }
        }
        Ok(())
    }
}

/// Render after the canvas so the modal blocks all other interactions.
pub fn show(ctx: &egui::Context, state: &mut AppState) {
    if !state.symbol_picker.is_open() {
        return;
    }
    let mut picker = std::mem::take(&mut state.symbol_picker);
    let replacing = matches!(picker.target, Some(Target::Replace { .. }));
    let width = (ctx.content_rect().width() - 48.0).clamp(260.0, 760.0);
    let height = (ctx.content_rect().height() - 235.0).clamp(100.0, 360.0);
    let dialog_height = (height + 180.0).min((ctx.content_rect().height() - 48.0).max(120.0));
    let mut confirm = false;
    let mut cancel = false;
    let response = egui::Modal::new(Id::new("symbol-picker")).show(ctx, |ui| {
        ui.set_width(width);
        // A scroll area otherwise keeps the Area's small initial height and
        // hides the footer even when the viewport has ample space.
        ui.set_height(dialog_height);
        egui::ScrollArea::vertical().id_salt("symbol-dialog")
            .max_height((dialog_height - 50.0).max(70.0))
            .show(ui, |ui| {
                ui.heading(if replacing { "Change Symbol" } else { "Add Symbol" });
                let search = ui.add(egui::TextEdit::singleline(&mut picker.query)
                    .hint_text("Search names, aliases, or categories…").desired_width(f32::INFINITY));
                if picker.focus_search { search.request_focus(); picker.focus_search = false; }
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("symbol-category")
                        .width(260.0_f32.min(width - 100.0))
                        .selected_text(picker.category.map_or("All categories", |index| &catalogue().categories[index]))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut picker.category, None, "All categories");
                            for (index, category) in catalogue().categories.iter().enumerate() {
                                ui.selectable_value(&mut picker.category, Some(index), category);
                            }
                        });
                    picker.update_filter();
                    ui.small(format!("{} symbols", picker.matches.len()));
                });
                ui.separator();
                let preview_width = if width >= 560.0 { 208.0 } else { 132.0 };
                ui.allocate_ui_with_layout(egui::vec2(width, height), egui::Layout::left_to_right(egui::Align::Min), |ui| {
                        let grid_width = width - preview_width - 24.0;
                        ui.allocate_ui_with_layout(egui::vec2(grid_width, height), egui::Layout::top_down(egui::Align::Min), |ui| picker.grid(ui, grid_width, height));
                        ui.allocate_ui_with_layout(egui::vec2(preview_width, height), egui::Layout::top_down(egui::Align::Min), |ui| picker.selected_preview(ui, state, width < 560.0 || height < 180.0));
                });
                ui.collapsing(format!("Material Design Icons {} · Apache 2.0", catalogue().version), |ui| {
                    ui.hyperlink_to("Pictogrammers · Library and contributors", "https://pictogrammers.com/library/mdi/");
                    ui.label("Artwork is embedded in each label with its name, author, source, and license.");
                    egui::ScrollArea::vertical().id_salt("symbol-license").max_height(120.0).show(ui, |ui| {
                        ui.small(include_str!("../assets/symbols/LICENSE-MDI.txt"));
                        ui.small(include_str!("../assets/symbols/LICENSE-APACHE-2.0.txt"));
                    });
                });
            });
        ui.separator();
        if let Some(error) = &picker.error { ui.colored_label(Color32::RED, error); }
        ui.horizontal(|ui| {
            let valid_preview = picker.preview.as_ref().is_some_and(|preview| preview.texture.is_ok());
            confirm = ui.add_enabled(picker.selected.is_some() && valid_preview,
                egui::Button::new(if replacing { "Replace" } else { "Add" })).clicked();
            cancel = ui.button("Cancel").clicked();
        });
    });
    let close = cancel || response.should_close();
    state.symbol_picker = picker;
    if confirm {
        let target = state.symbol_picker.target.unwrap();
        let index = state.symbol_picker.selected.unwrap();
        match state.apply_symbol(target, index) {
            Ok(()) => {
                state.symbol_picker.target = None;
                crate::editor::focus_editor(ctx, state);
                state.status_message = format!(
                    "{} symbol: {}",
                    if replacing { "Changed" } else { "Added" },
                    catalogue().icons[index].name
                );
            }
            Err(error) => state.symbol_picker.error = Some(error),
        }
    } else if close {
        state.symbol_picker.target = None;
        crate::editor::focus_editor(ctx, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{self, EditorAction};
    use ptouch_render::document::ContentBox;

    #[test]
    fn bundled_catalogue_is_complete_categorized_and_renderable() {
        let catalogue = catalogue();
        assert_eq!(catalogue.version, "7.4.47");
        assert_eq!(catalogue.icons.len(), 7188);
        assert_eq!(catalogue.categories.len(), 15);
        let mut previous = "";
        for icon in &catalogue.icons {
            assert!(
                icon.id.as_str() > previous,
                "duplicate or unsorted ID: {}",
                icon.id
            );
            previous = &icon.id;
            assert!(!icon.categories.is_empty());
            assert!(
                icon.categories
                    .iter()
                    .all(|group| catalogue.categories.contains(group))
            );
            let bytes = icon.svg();
            let metadata = symbol::metadata(&bytes).unwrap();
            assert_eq!(metadata.id, icon.id);
            assert_eq!(metadata.name, icon.name);
            let bitmap = load_image_from_reader(
                bytes.as_slice(),
                &ImageLoadOptions {
                    target_height: Some(64),
                    ..Default::default()
                },
            )
            .unwrap_or_else(|error| panic!("{}: {error}", icon.id));
            assert_eq!((bitmap.width(), bitmap.height()), (64, 64));
            assert!(
                (0..64).any(|x| (0..64).any(|y| bitmap.get_pixel(x, y))),
                "blank icon: {}",
                icon.id
            );
        }
        for (id, group) in [
            ("ethernet", "Networking & Computers"),
            ("wifi", "Networking & Computers"),
            ("usb", "Networking & Computers"),
            ("power-plug", "Electrical & Power"),
            ("screwdriver", "Tools & Workshop"),
            ("washing-machine", "Clothing & Laundry"),
            ("silverware", "Food & Drink"),
            ("cat", "Animals & Nature"),
        ] {
            assert!(
                catalogue.icons[find(id).unwrap()]
                    .categories
                    .iter()
                    .any(|category| category == group)
            );
        }
    }

    #[test]
    fn filtering_matches_aliases_categories_case_and_hyphens() {
        let mut picker = SymbolPicker::default();
        for (query, expected) in [
            ("ETHERNET", "ethernet"),
            ("wi-fi", "wifi"),
            ("laundry room", "washing-machine"),
            ("cutlery", "silverware"),
            ("networking", "usb"),
        ] {
            picker.query = query.into();
            picker.update_filter();
            assert!(picker.matches.contains(&find(expected).unwrap()), "{query}");
        }
        picker.category = catalogue()
            .categories
            .iter()
            .position(|category| category == "Food & Drink");
        picker.query = "cutlery".into();
        picker.update_filter();
        assert!(picker.matches.contains(&find("silverware").unwrap()));
        picker.selected = Some(find("silverware").unwrap());
        picker.query = "no-such-symbol-xyz".into();
        picker.update_filter();
        assert!(picker.matches.is_empty());
        assert!(picker.selected.is_none());
    }

    #[test]
    fn insertion_reuses_image_placement_and_is_one_undoable_action() {
        for version in [1, 4] {
            let mut state = AppState {
                document_version: version,
                ..Default::default()
            };
            state.add_text();
            let before = state.document().to_toml_string().unwrap();
            state.open_symbol_picker(None);
            assert_eq!(state.document().to_toml_string().unwrap(), before);
            state
                .apply_symbol(Target::Add, find("ethernet").unwrap())
                .unwrap();
            assert_eq!(state.elements.len(), 2);
            assert_eq!(state.selected_element, Some(1));
            assert_eq!(state.elements[1].display_name(), "Symbol: Ethernet");
            if version == 4 {
                let object = state.elements[1].as_object().unwrap();
                assert_eq!(object.frame.height_mm, state.printable_height_mm());
                assert_eq!(object.frame.width_mm, state.printable_height_mm());
                assert_eq!(object.frame.x_mm, 13.0);
            } else {
                assert!(matches!(
                    state.elements[1],
                    LabelElement::Image { path: None, .. }
                ));
                assert_eq!(state.document_version, 1);
            }
            let after = state.document().to_toml_string().unwrap();
            assert!(state.apply_editor_action(EditorAction::Undo));
            assert_eq!(state.document().to_toml_string().unwrap(), before);
            assert!(state.apply_editor_action(EditorAction::Redo));
            assert_eq!(state.document().to_toml_string().unwrap(), after);
        }
    }

    #[test]
    fn replacing_free_and_legacy_images_preserves_every_geometry_setting_and_history() {
        let bytes = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 16"><path d="M1 1H12V12H1Z"/></svg>"#.to_vec();
        for version in [1, 3, 4] {
            let original = if version == 1 {
                let mut element =
                    LabelElement::image_from_bytes(Some("missing.svg".into()), bytes.clone());
                if let LabelElement::Image {
                    rotation,
                    target_height,
                    flip_h,
                    flip_v,
                    ..
                } = &mut element
                {
                    *rotation = 17.0;
                    *target_height = Some(31);
                    *flip_h = true;
                    *flip_v = true;
                }
                element
            } else {
                let mut object = LabelObject::image_from_bytes(
                    Some("missing.svg".into()),
                    bytes.clone(),
                    TextBox {
                        x_mm: 18.0,
                        y_mm: 1.0,
                        width_mm: 7.0,
                        height_mm: 5.0,
                        position: PositionMode::Relative,
                        gap_mm: 2.0,
                        ..Default::default()
                    },
                );
                if version == 4 {
                    object.content_box = Some(ContentBox {
                        width_mm: 9.0,
                        height_mm: 3.0,
                        offset_x_mm: 1.0,
                        offset_y_mm: -0.5,
                    });
                }
                object.rotation = 17.0;
                object.flip_h = true;
                object.flip_v = true;
                LabelElement::Object(object)
            };
            let mut state = AppState {
                document_version: version,
                elements: vec![original.clone()],
                selected_element: Some(0),
                ..Default::default()
            };
            let before = state.document().to_toml_string().unwrap();
            state.open_symbol_picker(Some(0));
            assert!(state.symbol_picker.selected.is_none());
            state
                .apply_symbol(
                    state.symbol_picker.target.unwrap(),
                    find("washing-machine").unwrap(),
                )
                .unwrap();
            assert_eq!(state.elements[0].display_name(), "Symbol: Washing Machine");
            match (&original, &state.elements[0]) {
                (LabelElement::Object(before), LabelElement::Object(after)) => {
                    assert_eq!(before.frame, after.frame);
                    assert_eq!(before.content_box, after.content_box);
                    assert_eq!(
                        (before.rotation, before.flip_h, before.flip_v),
                        (after.rotation, after.flip_h, after.flip_v)
                    );
                    assert!(matches!(
                        after.content,
                        ObjectContent::Image { path: None, .. }
                    ));
                }
                (
                    LabelElement::Image {
                        rotation,
                        target_height,
                        flip_h,
                        flip_v,
                        ..
                    },
                    LabelElement::Image {
                        rotation: after_rotation,
                        target_height: after_height,
                        flip_h: after_h,
                        flip_v: after_v,
                        path,
                        ..
                    },
                ) => {
                    assert_eq!(
                        (rotation, target_height, flip_h, flip_v),
                        (after_rotation, after_height, after_h, after_v)
                    );
                    assert!(path.is_none());
                }
                _ => panic!("replacement changed the element type"),
            }
            let after = state.document().to_toml_string().unwrap();
            assert_eq!(state.editor.history.undo_name(), Some("Change symbol"));
            assert!(state.apply_editor_action(EditorAction::Undo));
            assert_eq!(state.document().to_toml_string().unwrap(), before);
            assert!(state.apply_editor_action(EditorAction::Redo));
            assert_eq!(state.document().to_toml_string().unwrap(), after);
            state.open_symbol_picker(Some(0));
            assert_eq!(state.symbol_picker.selected, find("washing-machine"));
        }
    }

    #[test]
    fn stale_or_invalid_replacement_leaves_document_and_history_unchanged() {
        let mut state = AppState::default();
        state.add_text();
        state.open_symbol_picker(Some(0));
        let before = state.document().to_toml_string().unwrap();
        let history = state.editor.history.undo_name();
        let index = find("wifi").unwrap();
        assert!(
            state
                .apply_symbol(state.symbol_picker.target.unwrap(), index)
                .is_err()
        );
        assert!(
            state
                .apply_symbol(
                    Target::Replace {
                        element: 10,
                        revision: state.document_revision
                    },
                    index
                )
                .is_err()
        );
        assert!(
            state
                .apply_symbol(
                    Target::Replace {
                        element: 0,
                        revision: state.document_revision + 1
                    },
                    index
                )
                .is_err()
        );
        assert_eq!(state.document().to_toml_string().unwrap(), before);
        assert_eq!(state.editor.history.undo_name(), history);
    }

    fn key(key: egui::Key, command: bool, shift: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                command,
                ctrl: command && !cfg!(target_os = "macos"),
                mac_cmd: command && cfg!(target_os = "macos"),
                shift,
                ..Default::default()
            },
        }
    }

    fn frame(
        ctx: &egui::Context,
        state: &mut AppState,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                editor::begin_frame(ctx, state);
                ui.add_enabled_ui(!state.symbol_picker.is_open(), |ui| {
                    crate::panels::toolbar::show_add_controls(ui, state);
                    let (_, response) =
                        ui.allocate_exact_size(egui::vec2(600.0, 100.0), Sense::click());
                    state.editor.surface(&response);
                });
                show(ctx, state);
                editor::end_frame(ctx, state);
            },
        )
    }

    fn click_label(ctx: &egui::Context, state: &mut AppState, label: &str) {
        let mut output = frame(ctx, state, vec![]);
        let position = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() / 2.0)
                }
                _ => None,
            })
            .unwrap_or_else(|| {
                let labels: Vec<_> = output
                    .shapes
                    .iter()
                    .filter_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) => Some((&text.galley.job.text, text.pos)),
                        _ => None,
                    })
                    .collect();
                panic!(
                    "missing label {label}; picker open={}; labels={labels:?}",
                    state.symbol_picker.is_open()
                )
            });
        output.textures_delta.clear();
        for pressed in [true, false] {
            frame(
                ctx,
                state,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            )
            .textures_delta
            .clear();
        }
    }

    #[test]
    fn picker_select_then_add_returns_focus_for_keyboard_undo_and_redo() {
        let ctx = egui::Context::default();
        let mut state = AppState::default();
        state.add_text();
        let before = state.document().to_toml_string().unwrap();
        click_label(&ctx, &mut state, "Add Symbol");
        assert!(state.symbol_picker.is_open());
        assert!(state.symbol_picker.selected.is_none());
        assert_eq!(state.document().to_toml_string().unwrap(), before);
        state.symbol_picker.query = "ethernet".into();
        frame(&ctx, &mut state, vec![]).textures_delta.clear();
        click_label(&ctx, &mut state, "Ethernet");
        assert_eq!(state.symbol_picker.selected, find("ethernet"));
        assert_eq!(state.document().to_toml_string().unwrap(), before);
        click_label(&ctx, &mut state, "Add");
        assert!(!state.symbol_picker.is_open());
        assert_eq!(state.elements.len(), 2);
        let after = state.document().to_toml_string().unwrap();
        frame(&ctx, &mut state, vec![key(egui::Key::Z, true, false)])
            .textures_delta
            .clear();
        assert_eq!(state.document().to_toml_string().unwrap(), before);
        frame(&ctx, &mut state, vec![key(egui::Key::Z, true, true)])
            .textures_delta
            .clear();
        assert_eq!(state.document().to_toml_string().unwrap(), after);
    }

    #[test]
    fn picker_blocks_document_shortcuts_and_escape_cancels_without_an_edit() {
        let ctx = egui::Context::default();
        let mut state = AppState::default();
        state.add_image_bytes(None, catalogue().icons[find("wifi").unwrap()].svg());
        let before = state.document().to_toml_string().unwrap();
        let history = state.editor.history.undo_name();
        state.open_symbol_picker(Some(0));
        for event in [
            key(egui::Key::Z, true, false),
            key(egui::Key::D, true, false),
            key(egui::Key::Delete, false, false),
        ] {
            frame(&ctx, &mut state, vec![event]).textures_delta.clear();
            assert_eq!(state.document().to_toml_string().unwrap(), before);
        }
        frame(&ctx, &mut state, vec![key(egui::Key::Escape, false, false)])
            .textures_delta
            .clear();
        assert!(!state.symbol_picker.is_open());
        assert_eq!(state.document().to_toml_string().unwrap(), before);
        assert_eq!(state.editor.history.undo_name(), history);
    }

    #[test]
    fn confirmation_controls_stay_visible_in_short_and_narrow_viewports() {
        for size in [
            egui::vec2(1200.0, 800.0),
            egui::vec2(860.0, 338.0),
            egui::vec2(520.0, 338.0),
            egui::vec2(400.0, 250.0),
        ] {
            let ctx = egui::Context::default();
            let mut state = AppState::default();
            state.add_image_bytes(None, catalogue().icons[find("ethernet").unwrap()].svg());
            state.open_symbol_picker(Some(0));
            for pass in 0..4 {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
                        ..Default::default()
                    },
                    |_| show(&ctx, &mut state),
                );
                output.textures_delta.clear();
                // Floating areas use an initial sizing pass before painting.
                if pass < 2 {
                    continue;
                }
                for label in ["Replace", "Cancel"] {
                    let (clip, text) = output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) if text.galley.job.text == label => {
                                Some((shape.clip_rect, text))
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| panic!("{label} is hidden at {size}"));
                    let center = text.pos + text.galley.size() / 2.0;
                    assert!(
                        clip.contains(center)
                            && Rect::from_min_size(egui::Pos2::ZERO, size).contains(center),
                        "{label} is clipped at {size}"
                    );
                }
            }
        }
    }
}
