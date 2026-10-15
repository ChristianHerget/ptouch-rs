// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Custom egui widgets and shared helpers.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Undo,
    Redo,
    Grab,
    Menu,
}

/// Draw controls without relying on the selected font's symbol coverage.
pub fn icon_button(ui: &mut egui::Ui, icon: Icon, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, |ui: &mut egui::Ui| {
        let size = egui::Vec2::splat(ui.spacing().interact_size.y);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
        });
        if ui.is_rect_visible(rect) {
            let visuals = ui.style().interact(&response);
            ui.painter().rect(
                rect.expand(visuals.expansion),
                visuals.corner_radius,
                visuals.weak_bg_fill,
                visuals.bg_stroke,
                egui::StrokeKind::Inside,
            );
            paint_icon(ui.painter(), rect, icon, visuals.fg_stroke);
        }
        #[cfg(test)]
        remember_icon(ui.ctx(), icon, rect);
        response
    })
}

pub fn drag_handle(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(16.0, ui.spacing().interact_size.y),
        egui::Sense::drag(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Other, ui.is_enabled(), "Reorder object")
    });
    paint_icon(
        ui.painter(),
        rect,
        Icon::Grab,
        ui.style().interact(&response).fg_stroke,
    );
    #[cfg(test)]
    remember_icon(ui.ctx(), Icon::Grab, rect);
    response
}

fn paint_icon(painter: &egui::Painter, rect: egui::Rect, icon: Icon, stroke: egui::Stroke) {
    let center = rect.center();
    match icon {
        Icon::Grab | Icon::Menu => {
            let columns: &[f32] = if icon == Icon::Grab {
                &[-2.5, 2.5]
            } else {
                &[0.0]
            };
            for x in columns {
                for y in [-5.0, 0.0, 5.0] {
                    painter.circle_filled(center + egui::vec2(*x, y), 1.3, stroke.color);
                }
            }
        }
        Icon::Undo | Icon::Redo => {
            let direction = if icon == Icon::Undo { 1.0 } else { -1.0 };
            let point = |x: f32, y: f32| center + egui::vec2(x * direction, y);
            let points = [
                point(-5.0, -3.0),
                point(1.0, -3.0),
                point(5.0, -1.0),
                point(5.0, 3.0),
                point(2.0, 5.0),
            ];
            painter.add(egui::Shape::line(points.to_vec(), stroke));
            painter.add(egui::Shape::line(
                vec![point(-1.0, -7.0), point(-5.0, -3.0), point(-1.0, 1.0)],
                stroke,
            ));
        }
    }
}

#[cfg(test)]
fn remember_icon(ctx: &egui::Context, icon: Icon, rect: egui::Rect) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|data| {
        let entry = data
            .get_temp_mut_or_default::<(u64, Vec<(Icon, egui::Rect)>)>(egui::Id::new("test_icons"));
        if entry.0 != pass {
            *entry = (pass, Vec::new());
        }
        entry.1.push((icon, rect));
    });
}

#[cfg(test)]
pub fn icon_positions(ctx: &egui::Context, icon: Icon) -> Vec<egui::Pos2> {
    ctx.data(|data| data.get_temp::<(u64, Vec<(Icon, egui::Rect)>)>(egui::Id::new("test_icons")))
        .unwrap_or_default()
        .1
        .into_iter()
        .filter(|(kind, _)| *kind == icon)
        .map(|(_, rect)| rect.center())
        .collect()
}

/// Build an image file dialog with per-format filters.
///
/// The first filter is "All Images" (default), followed by individual
/// format groups so the user can narrow down if needed.
pub fn image_file_dialog() -> rfd::FileDialog {
    rfd::FileDialog::new()
        .add_filter(
            "All Images",
            &[
                "png", "jpg", "jpeg", "gif", "bmp", "tiff", "tif", "webp", "ico", "pnm", "tga",
                "qoi", "svg", "svgz",
            ],
        )
        .add_filter("PNG", &["png"])
        .add_filter("JPEG", &["jpg", "jpeg"])
        .add_filter("SVG", &["svg", "svgz"])
        .add_filter("GIF", &["gif"])
        .add_filter("BMP", &["bmp"])
        .add_filter("TIFF", &["tiff", "tif"])
        .add_filter("WebP", &["webp"])
        .add_filter("ICO", &["ico"])
        .add_filter("QOI", &["qoi"])
        .add_filter("PNM", &["pnm"])
        .add_filter("TGA", &["tga"])
}

/// Build a file dialog filtered to layout files (`.ptl`).
pub fn layout_file_dialog() -> rfd::FileDialog {
    rfd::FileDialog::new().add_filter("Layout", &["ptl"])
}

/// iOS-style toggle switch widget.
///
/// Based on the egui demo toggle_switch example.
pub fn toggle(on: &mut bool) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| toggle_ui(ui, on)
}

fn toggle_ui(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let desired_size = ui.spacing().interact_size.y * egui::vec2(2.0, 1.0);
    let (rect, mut response) = ui.allocate_exact_size(desired_size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, "")
    });

    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
        let visuals = ui.style().interact_selectable(&response, *on);
        let rect = rect.expand(visuals.expansion);
        let radius = 0.5 * rect.height();
        ui.painter().rect(
            rect,
            radius,
            visuals.bg_fill,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let circle_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        let center = egui::pos2(circle_x, rect.center().y);
        ui.painter()
            .circle(center, 0.75 * radius, visuals.bg_fill, visuals.fg_stroke);
    }

    response
}
