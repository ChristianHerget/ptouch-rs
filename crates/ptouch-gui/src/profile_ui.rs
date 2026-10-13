// SPDX-License-Identifier: MIT
//! Shared portable profiles, with edits saved back to their source file.
use crate::state::{AppState, PrinterCommand, PrinterTarget};
use ptouch_profiles::{Connection, PrinterProfile, ProfileStore, Quality};
use std::path::PathBuf;

pub struct ProfilesUi {
    pub open: bool,
    pub store: Option<ProfileStore>,
    pub error: Option<String>,
    directory: PathBuf,
    draft: PrinterProfile,
    editing: Option<String>,
    filename: String,
}

impl Default for ProfilesUi {
    fn default() -> Self {
        Self {
            open: false,
            store: None,
            error: None,
            directory: ptouch_profiles::default_directory(),
            draft: new_profile(),
            editing: None,
            filename: String::new(),
        }
    }
}

fn new_profile() -> PrinterProfile {
    PrinterProfile {
        alias: "printer".into(),
        model: "PT-P750W".into(),
        connection: Connection::Network {
            host: String::new(),
        },
        quality: Quality::Standard,
        final_cut: true,
        allow_experimental: false,
    }
}

impl ProfilesUi {
    pub fn load(directory: PathBuf) -> Self {
        let mut ui = Self {
            directory,
            ..Self::default()
        };
        ui.reload();
        ui
    }
    fn reload(&mut self) {
        match ProfileStore::load(&self.directory) {
            Ok(store) => {
                self.store = Some(store);
                self.error = None;
            }
            Err(e) => {
                self.store = None;
                self.error = Some(e.to_string());
            }
        }
    }
    pub fn default_profile(&self) -> Option<PrinterProfile> {
        self.store.as_ref()?.selected(None).ok().flatten().cloned()
    }
}

pub fn show(ctx: &egui::Context, state: &mut AppState) {
    if !state.profiles.open {
        return;
    }
    if state.is_printer_busy() {
        return;
    }
    let mut open = true;
    let mut selected = None;
    let p = &mut state.profiles;
    egui::Window::new("Printers")
        .open(&mut open)
        .default_width(620.0)
        .show(ctx, |ui| {
            ui.label(format!("Profiles directory: {}", p.directory.display()));
            ui.horizontal(|ui| {
                if ui.button("Choose directory…").clicked()
                    && let Some(dir) = rfd::FileDialog::new()
                        .set_directory(&p.directory)
                        .pick_folder()
                {
                    p.directory = dir;
                    p.reload();
                    p.editing = None;
                }
                if ui.button("Reload files").clicked() {
                    p.reload();
                    p.editing = None;
                }
                if ui.button("New printer").clicked() {
                    p.draft = new_profile();
                    p.editing = None;
                    p.filename.clear();
                }
            });
            if let Some(error) = &p.error {
                ui.colored_label(egui::Color32::RED, error);
            }
            if let Some(store) = &p.store {
                for (alias, entry) in &store.entries {
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(p.editing.as_ref() == Some(alias), alias)
                            .clicked()
                        {
                            p.editing = Some(alias.clone());
                            p.draft = entry.profile.clone();
                            p.filename = entry
                                .source
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned();
                        }
                        ui.label(&entry.profile.model);
                        ui.weak(
                            entry
                                .source
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy(),
                        );
                        if store.default_printer.as_ref() == Some(alias) {
                            ui.label("Default");
                        }
                    });
                }
            }
            ui.separator();
            egui::Grid::new("printer_profile_fields")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Alias");
                    ui.text_edit_singleline(&mut p.draft.alias);
                    ui.end_row();
                    ui.label("Model");
                    egui::ComboBox::from_id_salt("profile_model")
                        .selected_text(&p.draft.model)
                        .show_ui(ui, |ui| {
                            for model in ptouch_core::printer_model::PrinterModel::all() {
                                ui.selectable_value(
                                    &mut p.draft.model,
                                    model.name.to_owned(),
                                    format!(
                                        "{}{}",
                                        model.name,
                                        if model.experimental {
                                            " (experimental network)"
                                        } else {
                                            ""
                                        }
                                    ),
                                );
                            }
                        });
                    ui.end_row();
                    ui.label("Connection");
                    let was_network = matches!(p.draft.connection, Connection::Network { .. });
                    let mut network = was_network;
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut network, true, "Network");
                        ui.radio_value(&mut network, false, "USB");
                    });
                    if network != was_network {
                        p.draft.connection = if network {
                            Connection::Network {
                                host: String::new(),
                            }
                        } else {
                            Connection::Usb { serial: None }
                        };
                    }
                    ui.end_row();
                    match &mut p.draft.connection {
                        Connection::Network { host } => {
                            ui.label("Hostname / IP");
                            ui.text_edit_singleline(host);
                        }
                        Connection::Usb { serial } => {
                            ui.label("USB serial (optional)");
                            let mut value = serial.clone().unwrap_or_default();
                            if ui.text_edit_singleline(&mut value).changed() {
                                *serial = if value.trim().is_empty() {
                                    None
                                } else {
                                    Some(value)
                                };
                            }
                        }
                    };
                    ui.end_row();
                    ui.label("Preferred quality");
                    let model = ptouch_core::printer_model::PrinterModel::find(&p.draft.model);
                    egui::ComboBox::from_id_salt("profile_quality")
                        .selected_text(format!("{:?}", p.draft.quality))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut p.draft.quality,
                                Quality::Standard,
                                "Standard",
                            );
                            if model.is_some_and(|m| m.high_resolution) {
                                ui.selectable_value(
                                    &mut p.draft.quality,
                                    Quality::High,
                                    "High resolution",
                                );
                            }
                            if model.is_some_and(|m| m.draft) {
                                ui.selectable_value(&mut p.draft.quality, Quality::Draft, "Draft");
                            }
                        });
                    ui.end_row();
                    ui.label("Final cut");
                    ui.checkbox(&mut p.draft.final_cut, "Feed and cut after printing");
                    ui.end_row();
                    ui.label("Experimental model");
                    ui.checkbox(
                        &mut p.draft.allow_experimental,
                        "Allow experimental network printing",
                    );
                    ui.end_row();
                    ui.label("File");
                    ui.add_enabled(
                        p.editing.is_none(),
                        egui::TextEdit::singleline(&mut p.filename)
                            .hint_text("Leave blank for <alias>.toml"),
                    );
                    ui.end_row();
                });
            ui.horizontal(|ui| {
                if ui.button("Connect without saving").clicked() {
                    match validate(&p.draft) {
                        Ok(()) => selected = Some(p.draft.clone()),
                        Err(e) => p.error = Some(e),
                    }
                }
                if ui
                    .add_enabled(p.store.is_some(), egui::Button::new("Save"))
                    .clicked()
                {
                    let result = validate(&p.draft).and_then(|()| {
                        let store = p.store.as_mut().ok_or("Reload profiles first")?;
                        if let Some(alias) = &p.editing {
                            store.update(alias, p.draft.clone())
                        } else {
                            store.add(
                                p.draft.clone(),
                                (!p.filename.is_empty()).then_some(p.filename.as_str()),
                            )
                        }
                        .map_err(|e| e.to_string())
                    });
                    match result {
                        Ok(()) => {
                            p.editing = Some(p.draft.alias.clone());
                            p.error = None;
                            selected = Some(p.draft.clone());
                        }
                        Err(e) => p.error = Some(e),
                    }
                }
            });
            if let Some(alias) = p.editing.clone() {
                ui.horizontal(|ui| {
                    if ui.button("Make default").clicked()
                        && let Some(store) = &mut p.store
                    {
                        p.error = store.set_default(Some(&alias)).err().map(|e| e.to_string());
                    }
                    if ui.button("Clear default").clicked()
                        && let Some(store) = &mut p.store
                    {
                        p.error = store.set_default(None).err().map(|e| e.to_string());
                    }
                    if ui.button("Remove").clicked()
                        && let Some(store) = &mut p.store
                    {
                        p.error = store.remove(&alias).err().map(|e| e.to_string());
                        if p.error.is_none() {
                            p.editing = None;
                        }
                    }
                });
            }
        });
    p.open = open;
    if let Some(profile) = selected {
        state.printer_target = PrinterTarget::Profile(profile);
        state.apply_target_preferences();
        state.printer_connected = false;
        state.connecting = true;
        state.printer_status = Some("Connecting…".into());
        if let Some(tx) = &state.printer_cmd_tx {
            let _ = tx.send(PrinterCommand::Poll(state.printer_target.clone()));
        }
    }
}

fn validate(p: &PrinterProfile) -> Result<(), String> {
    p.validate().map_err(|e| e.to_string())?;
    let m = ptouch_core::printer_model::PrinterModel::find(&p.model).ok_or("Unknown model")?;
    if matches!(p.connection, Connection::Usb { .. }) && !m.usb {
        return Err("USB is not implemented for this model".into());
    }
    if matches!(p.connection, Connection::Network { .. }) && m.network.is_none() {
        return Err("This model has no network backend".into());
    }
    if (p.quality == Quality::High && !m.high_resolution)
        || (p.quality == Quality::Draft && !m.draft)
    {
        return Err("Quality unsupported by this model".into());
    }
    Ok(())
}
