// SPDX-License-Identifier: MIT
//! Job overrides and measured corrections stored with portable printers.

use crate::state::{AppState, CalibrationMode};
use ptouch_profiles::{CalibrationRule, Connection, PrinterProfile, Quality};
use ptouch_render::calibration::PrintCalibration;

pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    ui.collapsing("Position calibration", |ui| {
        ui.add_enabled_ui(!state.is_printer_busy(), |ui| {
            egui::ComboBox::from_id_salt("calibration_mode")
                .selected_text(match state.calibration_mode {
                    CalibrationMode::Profile => "Use printer profile",
                    CalibrationMode::Disabled => "No correction",
                    CalibrationMode::Custom => "Custom correction",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut state.calibration_mode,
                        CalibrationMode::Profile,
                        "Use printer profile",
                    );
                    ui.selectable_value(
                        &mut state.calibration_mode,
                        CalibrationMode::Disabled,
                        "No correction",
                    );
                    ui.selectable_value(
                        &mut state.calibration_mode,
                        CalibrationMode::Custom,
                        "Custom correction",
                    );
                });
            if state.calibration_mode == CalibrationMode::Custom {
                offset_input(ui, &mut state.calibration.offset_mm);
            }
            match state.calibration_offset() {
                Some(offset) => {
                    ui.label(format!("Active correction: {offset:+.3} mm"));
                }
                None => {
                    ui.label("No correction applies to these settings.");
                }
            }
            ui.small("Positive adds leading space. Negative removes blank space only.");
            if let Some(profile) = state.printer_target.profile()
                && ui.button("Manage saved corrections…").clicked()
            {
                state.profiles.edit_profile(&profile.alias.clone());
            }
            ui.horizontal_wrapped(|ui| {
                if ui.button("Load correction file").clicked() {
                    match load_correction() {
                        Ok(Some(correction)) => {
                            state.calibration = correction;
                            state.calibration_mode = CalibrationMode::Custom;
                        }
                        Ok(None) => (),
                        Err(error) => state.status_message = error,
                    }
                }
                if ui
                    .add_enabled(
                        state.calibration_offset().is_some(),
                        egui::Button::new("Export correction file"),
                    )
                    .clicked()
                {
                    let correction = PrintCalibration {
                        name: state.printer_target.label(),
                        offset_mm: state.calibration_offset().unwrap(),
                    };
                    if let Err(error) = export_correction(&correction) {
                        state.status_message = error;
                    }
                }
            });
        });
    });
}

fn offset_input(ui: &mut egui::Ui, offset: &mut f64) {
    ui.horizontal(|ui| {
        ui.label("Offset");
        ui.add(
            crate::widgets::decimal_drag(offset)
                .range(-100.0..=100.0)
                .min_decimals(2)
                .max_decimals(3)
                .speed(0.1)
                .suffix(" mm"),
        );
    });
}

pub fn profile_rules(ui: &mut egui::Ui, profile: &mut PrinterProfile, error: &mut Option<String>) {
    ui.collapsing("Position corrections", |ui| {
        ui.small("Applied automatically for matching quality and cut commands. No match uses no correction.");
        let model = ptouch_core::printer_model::PrinterModel::find(&profile.model);
        let mut remove = None;
        for (index, rule) in profile.calibrations.iter_mut().enumerate() {
            ui.push_id(index, |ui| {
                ui.horizontal_wrapped(|ui| {
                    egui::ComboBox::from_id_salt("quality")
                        .selected_text(format!("{:?}", rule.quality))
                        .show_ui(ui, |ui| {
                            for (quality, core) in [
                                (Quality::Standard, ptouch_core::PrintQuality::Standard),
                                (Quality::High, ptouch_core::PrintQuality::HighRes),
                                (Quality::Draft, ptouch_core::PrintQuality::Draft),
                            ] {
                                if model.is_some_and(|model| model.quality.mode(core).is_ok()) {
                                    ui.selectable_value(&mut rule.quality, quality, format!("{quality:?}"));
                                }
                            }
                        });
                    ui.checkbox(&mut rule.precut, "Leading cut");
                    ui.checkbox(&mut rule.final_cut, "Final cut");
                    if ui.button("Remove").clicked() { remove = Some(index); }
                });
                offset_input(ui, &mut rule.offset_mm);
                ui.horizontal(|ui| {
                    if ui.button("Import correction").clicked() {
                        match load_correction() {
                            Ok(Some(correction)) => rule.offset_mm = correction.offset_mm,
                            Ok(None) => (),
                            Err(e) => *error = Some(e),
                        }
                    }
                    if ui.button("Export correction").clicked() {
                        let correction = PrintCalibration { name: profile.alias.clone(), offset_mm: rule.offset_mm };
                        if let Err(e) = export_correction(&correction) { *error = Some(e); }
                    }
                });
                ui.separator();
            });
        }
        if let Some(index) = remove { profile.calibrations.remove(index); }
        if ui.button("Add correction").clicked() {
            profile.calibrations.push(CalibrationRule {
                quality: profile.quality,
                precut: matches!(profile.connection, Connection::Network { .. }),
                final_cut: profile.final_cut,
                offset_mm: 0.0,
            });
        }
        if let Err(e) = profile.validate() {
            ui.colored_label(egui::Color32::RED, e.to_string());
        }
    });
}

fn load_correction() -> Result<Option<PrintCalibration>, String> {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("Calibration", &["toml"])
        .pick_file()
    else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    PrintCalibration::from_toml(&text)
        .map(Some)
        .map_err(|e| e.to_string())
}

fn export_correction(correction: &PrintCalibration) -> Result<(), String> {
    let text = correction.to_toml().map_err(|e| e.to_string())?;
    if let Some(path) = rfd::FileDialog::new()
        .add_filter("Calibration", &["toml"])
        .set_file_name("printer-calibration.toml")
        .save_file()
    {
        std::fs::write(path, text).map_err(|e| e.to_string())?;
    }
    Ok(())
}
