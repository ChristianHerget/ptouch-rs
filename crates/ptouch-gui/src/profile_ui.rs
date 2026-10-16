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
    usb_devices: Vec<ptouch_core::usb_profiles::UsbCandidate>,
    usb_error: Option<String>,
    usb_loading: bool,
    usb_refresh: bool,
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
            usb_devices: Vec::new(),
            usb_error: None,
            usb_loading: false,
            usb_refresh: true,
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
        calibrations: Vec::new(),
    }
}

impl ProfilesUi {
    pub fn edit_profile(&mut self, alias: &str) {
        self.open = true;
        if let Some(entry) = self
            .store
            .as_ref()
            .and_then(|store| store.entries.get(alias))
        {
            self.draft = entry.profile.clone();
            self.editing = Some(alias.into());
            self.filename = entry
                .source
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into();
        }
    }
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
    pub fn usb_discovered(
        &mut self,
        result: Result<Vec<ptouch_core::usb_profiles::UsbCandidate>, String>,
    ) {
        self.usb_loading = false;
        match result {
            Ok(devices) => {
                self.usb_devices = devices;
                self.usb_error = None;
            }
            Err(error) => self.usb_error = Some(error),
        }
    }

    fn saved_target(&self, alias: &str) -> PrinterTarget {
        let result = (|| {
            let store = self.store.as_ref().ok_or_else(|| {
                self.error
                    .clone()
                    .unwrap_or_else(|| "Profiles unavailable".into())
            })?;
            let entry = store
                .entries
                .get(alias)
                .ok_or_else(|| format!("Saved printer {alias} no longer exists"))?;
            validate(&entry.profile)?;
            Ok::<_, String>(entry.profile.clone())
        })();
        match result {
            Ok(p) => PrinterTarget::Profile(p),
            Err(e) => PrinterTarget::Unavailable(e),
        }
    }

    pub fn directory_target(&self) -> PrinterTarget {
        match self
            .store
            .as_ref()
            .and_then(|s| s.default_printer.as_deref())
        {
            Some(alias) => self.saved_target(alias),
            None => PrinterTarget::Unavailable(self.error.clone().unwrap_or_else(|| {
                "No default printer in this directory; select a printer".into()
            })),
        }
    }

    fn reload_target(&self, current: &PrinterTarget) -> Option<PrinterTarget> {
        match current {
            PrinterTarget::Profile(p) => Some(self.saved_target(&p.alias)),
            _ => None,
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
    let active_target = state.printer_target.clone();
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
                    selected = Some(p.directory_target());
                }
                if ui.button("Reload files").clicked() {
                    p.reload();
                    p.editing = None;
                    selected = p.reload_target(&active_target);
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
            let old_identity = (p.draft.model.clone(), p.draft.connection.clone());
            egui::Grid::new("printer_profile_fields")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Alias");
                    ui.text_edit_singleline(&mut p.draft.alias);
                    ui.end_row();
                    ui.label("Model");
                    let previous_model = p.draft.model.clone();
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
                    reconcile_model(&mut p.draft, &previous_model);
                    ui.end_row();
                    ui.label("Connection");
                    let was_network = matches!(p.draft.connection, Connection::Network { .. });
                    let mut network = was_network;
                    ui.horizontal(|ui| {
                        let model = ptouch_core::printer_model::PrinterModel::find(&p.draft.model);
                        ui.add_enabled_ui(model.is_some_and(|m| m.network.is_some()), |ui| {
                            ui.radio_value(&mut network, true, "Network");
                        });
                        ui.add_enabled_ui(model.is_some_and(|m| m.usb), |ui| {
                            ui.radio_value(&mut network, false, "USB");
                        });
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
                            ui.label("Hostname / IP address");
                            ui.text_edit_singleline(host);
                        }
                        Connection::Usb { serial } => {
                            ui.label("USB serial");
                            p.usb_refresh |= usb_serial_selector(
                                ui, serial, &p.draft.model, &p.usb_devices, p.usb_loading,
                            );
                        }
                    };
                    ui.end_row();
                    ui.label("Preferred quality");
                    let model = ptouch_core::printer_model::PrinterModel::find(&p.draft.model);
                    let selected_mode = model
                        .and_then(|m| m.quality.mode(to_core_quality(p.draft.quality)).ok());
                    egui::ComboBox::from_id_salt("profile_quality")
                        .selected_text(selected_mode.map(|m| m.label())
                            .unwrap_or_else(|| "Unsupported quality".into()))
                        .show_ui(ui, |ui| {
                            if let Some(model) = model {
                                for mode in model.quality.modes {
                                    ui.selectable_value(
                                        &mut p.draft.quality, from_core_quality(mode.quality), mode.label(),
                                    ).on_hover_text(format!(
                                        "DPI: across the head × along the tape. {}", mode.requirement(),
                                    ));
                                }
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
            if clear_changed_calibrations(&mut p.draft, &old_identity) {
                ui.label("Printer changed; measured corrections cleared from this draft.");
            }
            crate::calibration_ui::profile_rules(ui, &mut p.draft, &mut p.error);
            if p.usb_loading {
                ui.label("Reading USB serials…");
            }
            if let Some(error) = &p.usb_error {
                ui.colored_label(egui::Color32::RED, error);
            }
            ui.horizontal(|ui| {
                if ui.button("Connect without saving").clicked() {
                    match validate(&p.draft) {
                        Ok(()) => selected = Some(PrinterTarget::TemporaryProfile(p.draft.clone())),
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
                            selected = Some(PrinterTarget::Profile(p.draft.clone()));
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
                            if matches!(&active_target, PrinterTarget::Profile(profile) if profile.alias == alias) {
                                selected = Some(PrinterTarget::Unavailable(format!("Saved printer {alias} was removed")));
                            }
                        }
                    }
                });
            }
        });
    p.open = open;
    if p.usb_refresh && !p.usb_loading {
        p.usb_refresh = false;
        if let Some(tx) = &state.printer_cmd_tx {
            p.usb_loading = tx.send(PrinterCommand::DiscoverUsbProfiles).is_ok();
        }
    }
    if let Some(target) = selected {
        state.select_target(target);
    }
}

fn clear_changed_calibrations(profile: &mut PrinterProfile, old: &(String, Connection)) -> bool {
    if (profile.model != old.0 || profile.connection != old.1) && !profile.calibrations.is_empty() {
        profile.calibrations.clear();
        true
    } else {
        false
    }
}

fn validate(p: &PrinterProfile) -> Result<(), String> {
    p.validate().map_err(|e| e.to_string())?;
    if let Connection::Network { host } = &p.connection {
        ptouch_core::network_host::normalize(host).map_err(|e| e.to_string())?;
    }
    let m = ptouch_core::printer_model::PrinterModel::find(&p.model).ok_or("Unknown model")?;
    if matches!(p.connection, Connection::Usb { .. }) && !m.usb {
        return Err("USB is not implemented for this model".into());
    }
    if matches!(p.connection, Connection::Network { .. }) && m.network.is_none() {
        return Err("This model has no network backend".into());
    }
    for rule in &p.calibrations {
        m.quality
            .mode(to_core_quality(rule.quality))
            .map_err(|e| e.to_string())?;
    }
    if m.quality.mode(to_core_quality(p.quality)).is_err() {
        return Err("Quality unsupported by this model".into());
    }
    Ok(())
}

/// A single editable dropdown; returns whether discovery should be refreshed.
fn usb_serial_selector(
    ui: &mut egui::Ui,
    serial: &mut Option<String>,
    model: &str,
    devices: &[ptouch_core::usb_profiles::UsbCandidate],
    loading: bool,
) -> bool {
    let mut refresh = false;
    let mut value = serial.clone().unwrap_or_default();
    egui::Frame::group(ui.style())
        .inner_margin(2.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut value)
                            .frame(egui::Frame::NONE)
                            .desired_width(240.0)
                            .hint_text("Automatic — one matching printer"),
                    )
                    .changed()
                {
                    *serial = serial_value(&value);
                }
                ui.menu_button("▾", |ui| {
                    if ui
                        .selectable_label(serial.is_none(), "Automatic — one matching printer")
                        .clicked()
                    {
                        *serial = None;
                        ui.close();
                    }
                    for device in devices.iter().filter(|d| d.model == model) {
                        if let Some(error) = &device.identity_error {
                            ui.add_enabled(false, egui::Button::new(error));
                        } else if let Some(candidate) = &device.serial {
                            if ui
                                .selectable_label(serial.as_ref() == Some(candidate), candidate)
                                .clicked()
                            {
                                *serial = Some(candidate.clone());
                                ui.close();
                            }
                        } else {
                            ui.add_enabled(
                                false,
                                egui::Button::new("Detected printer has no serial"),
                            );
                        }
                    }
                    ui.separator();
                    if ui
                        .add_enabled(!loading, egui::Button::new("Refresh devices"))
                        .clicked()
                    {
                        refresh = true;
                        ui.close();
                    }
                });
            });
        });
    refresh
}

fn serial_value(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.trim().to_owned())
}

fn to_core_quality(quality: Quality) -> ptouch_core::PrintQuality {
    match quality {
        Quality::Standard => ptouch_core::PrintQuality::Standard,
        Quality::High => ptouch_core::PrintQuality::HighRes,
        Quality::Draft => ptouch_core::PrintQuality::Draft,
    }
}
fn from_core_quality(quality: ptouch_core::PrintQuality) -> Quality {
    match quality {
        ptouch_core::PrintQuality::Standard => Quality::Standard,
        ptouch_core::PrintQuality::HighRes => Quality::High,
        ptouch_core::PrintQuality::Draft => Quality::Draft,
    }
}

fn reconcile_model(profile: &mut PrinterProfile, previous: &str) {
    if profile.model == previous {
        return;
    }
    let Some(model) = ptouch_core::printer_model::PrinterModel::find(&profile.model) else {
        return;
    };
    match &mut profile.connection {
        Connection::Network { .. } if model.network.is_none() && model.usb => {
            profile.connection = Connection::Usb { serial: None }
        }
        Connection::Usb { .. } if !model.usb && model.network.is_some() => {
            profile.connection = Connection::Network {
                host: String::new(),
            }
        }
        Connection::Usb { serial } => *serial = None,
        _ => {}
    }
    if model
        .quality
        .mode(to_core_quality(profile.quality))
        .is_err()
    {
        profile.quality = Quality::Standard;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "ptouch-profile-ui-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn profile() -> PrinterProfile {
        PrinterProfile {
            connection: Connection::Network {
                host: "old-printer".into(),
            },
            ..new_profile()
        }
    }
    fn saved(directory: &TestDirectory) -> ProfilesUi {
        let mut ui = ProfilesUi::load(directory.0.clone());
        ui.store.as_mut().unwrap().add(profile(), None).unwrap();
        ui
    }

    #[test]
    fn corrections_follow_profiles_and_overrides_reset_on_context_changes() {
        use crate::state::CalibrationMode;
        let mut profile = profile();
        profile.calibrations = vec![ptouch_profiles::CalibrationRule {
            quality: Quality::Standard,
            precut: true,
            final_cut: true,
            offset_mm: 0.75,
        }];
        let mut state = AppState::default();
        state.select_target(PrinterTarget::Profile(profile.clone()));
        assert_eq!(state.calibration_offset(), Some(0.75));
        state.calibration_mode = CalibrationMode::Disabled;
        assert_eq!(state.calibration_offset(), None);
        state.calibration_mode = CalibrationMode::Custom;
        state.calibration.offset_mm = 1.25;
        assert_eq!(state.calibration_offset(), Some(1.25));
        state.set_print_quality(ptouch_core::PrintQuality::HighRes);
        assert_eq!(state.calibration_mode, CalibrationMode::Profile);
        assert_eq!(state.calibration_offset(), None);
        state.set_print_quality(ptouch_core::PrintQuality::Standard);
        assert_eq!(state.calibration_offset(), Some(0.75));
        state.auto_cut = false;
        assert_eq!(state.calibration_offset(), None);
        state.calibration_mode = CalibrationMode::Custom;
        state.select_target(PrinterTarget::Usb);
        assert_eq!(state.calibration_mode, CalibrationMode::Profile);
        assert_eq!(state.calibration_offset(), None);
        let identity = (profile.model.clone(), profile.connection.clone());
        profile.alias = "renamed".into();
        assert!(!clear_changed_calibrations(&mut profile, &identity));
        profile.connection = Connection::Network {
            host: "new-printer".into(),
        };
        assert!(clear_changed_calibrations(&mut profile, &identity));
        assert!(profile.calibrations.is_empty());
    }

    #[test]
    fn reload_reconnects_saved_alias_with_current_connection_and_preferences() {
        let dir = TestDirectory::new();
        let mut ui = saved(&dir);
        let mut state = AppState::default();
        state.select_target(PrinterTarget::Profile(profile()));
        state.printer_connected = true;
        let old_selection = state.selection();
        let mut updated = profile();
        updated.connection = Connection::Network {
            host: "new-printer".into(),
        };
        updated.quality = Quality::High;
        updated.final_cut = false;
        ProfileStore::load(&dir.0)
            .unwrap()
            .update("printer", updated.clone())
            .unwrap();
        ui.reload();
        state.select_target(ui.reload_target(&state.printer_target).unwrap());
        assert_eq!(state.printer_target, PrinterTarget::Profile(updated));
        assert!(!state.printer_connected);
        assert!(state.connecting);
        assert!(!state.auto_cut);
        assert_eq!(state.print_quality, ptouch_core::PrintQuality::HighRes);
        assert_ne!(old_selection, state.selection());
    }

    #[test]
    fn removed_or_invalid_saved_profiles_disconnect_but_temporary_profiles_survive_reload() {
        let dir = TestDirectory::new();
        let mut ui = saved(&dir);
        let active = PrinterTarget::Profile(profile());
        let temporary = PrinterTarget::TemporaryProfile(profile());
        ProfileStore::load(&dir.0)
            .unwrap()
            .remove("printer")
            .unwrap();
        ui.reload();
        assert!(matches!(
            ui.reload_target(&active),
            Some(PrinterTarget::Unavailable(_))
        ));
        assert!(ui.reload_target(&temporary).is_none());
        std::fs::write(dir.0.join("broken.toml"), "[not valid").unwrap();
        ui.reload();
        assert!(ui.error.is_some());
        assert!(matches!(
            ui.reload_target(&active),
            Some(PrinterTarget::Unavailable(_))
        ));
        assert!(ui.reload_target(&temporary).is_none());
    }

    #[test]
    fn new_directory_uses_its_default_and_rejects_invalid_defaults() {
        let dir = TestDirectory::new();
        let mut ui = saved(&dir);
        assert!(matches!(
            ui.directory_target(),
            PrinterTarget::Unavailable(_)
        ));
        ui.store
            .as_mut()
            .unwrap()
            .set_default(Some("printer"))
            .unwrap();
        assert_eq!(ui.directory_target(), PrinterTarget::Profile(profile()));
        let mut invalid = profile();
        invalid.model = "PT-P700".into(); // USB only
        ui.store
            .as_mut()
            .unwrap()
            .update("printer", invalid)
            .unwrap();
        assert!(matches!(
            ui.directory_target(),
            PrinterTarget::Unavailable(_)
        ));
        let other = TestDirectory::new();
        let other_ui = saved(&other);
        assert!(matches!(
            other_ui.directory_target(),
            PrinterTarget::Unavailable(_)
        ));
    }

    #[test]
    fn model_changes_preserve_valid_settings_and_reset_incompatible_settings() {
        let mut p = profile();
        p.quality = Quality::High;
        p.model = "PT-P700".into();
        reconcile_model(&mut p, "PT-P750W");
        assert_eq!(p.connection, Connection::Usb { serial: None });
        assert_eq!(p.quality, Quality::Standard);
        p.connection = Connection::Usb {
            serial: Some("old-model-serial".into()),
        };
        p.model = "PT-9700PC".into();
        p.quality = Quality::High;
        reconcile_model(&mut p, "PT-P700");
        assert_eq!(p.connection, Connection::Usb { serial: None });
        assert_eq!(p.quality, Quality::High);
        p.model = "PT-P950NW".into();
        reconcile_model(&mut p, "PT-9700PC");
        assert!(matches!(p.connection, Connection::Network { .. }));
        assert_eq!(p.quality, Quality::Standard);
        p.connection = Connection::Network {
            host: "keep-host".into(),
        };
        p.model = "PT-P750W".into();
        reconcile_model(&mut p, "PT-P950NW");
        assert_eq!(
            p.connection,
            Connection::Network {
                host: "keep-host".into()
            }
        );
    }

    #[test]
    fn serial_entry_survives_absent_devices_and_discovery_failures() {
        let mut ui = ProfilesUi::default();
        ui.draft.connection = Connection::Usb {
            serial: serial_value("disconnected-device"),
        };
        ui.usb_discovered(Ok(Vec::new()));
        ui.usb_discovered(Err("USB unavailable".into()));
        assert_eq!(
            ui.draft.connection,
            Connection::Usb {
                serial: Some("disconnected-device".into())
            }
        );
        assert_eq!(serial_value(" "), None);
        assert_eq!(serial_value(" ABC "), Some("ABC".into()));
    }
}
