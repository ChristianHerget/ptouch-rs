// SPDX-License-Identifier: MIT
//! Portable configuration, independent of printer drivers and protocol code.

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item, Table, value};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    #[default]
    Standard,
    High,
    Draft,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "connection", rename_all = "lowercase")]
pub enum Connection {
    Network {
        host: String,
    },
    Usb {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        serial: Option<String>,
    },
}

fn enabled() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterProfile {
    pub alias: String,
    pub model: String,
    #[serde(flatten)]
    pub connection: Connection,
    #[serde(default)]
    pub quality: Quality,
    #[serde(default = "enabled")]
    pub final_cut: bool,
    #[serde(default)]
    pub allow_experimental: bool,
}

impl PrinterProfile {
    pub fn validate(&self) -> Result<()> {
        if self.alias.is_empty()
            || !self
                .alias
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
        {
            return Err("Alias must contain only letters, digits, underscores, or hyphens".into());
        }
        if self.model.trim().is_empty() {
            return Err("A printer model is required".into());
        }
        match &self.connection {
            Connection::Network { host }
                if host.trim().is_empty()
                    || host.chars().any(char::is_whitespace)
                    || host.contains('/') =>
            {
                return Err(
                    "Host must be an IP address or hostname, without a URL or whitespace".into(),
                );
            }
            Connection::Usb {
                serial: Some(serial),
            } if serial.trim().is_empty() => return Err("USB serial must not be empty".into()),
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileFile {
    version: u32,
    #[serde(default)]
    default_printer: Option<String>,
    #[serde(default)]
    printers: Vec<PrinterProfile>,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub profile: PrinterProfile,
    pub source: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ProfileStore {
    pub directory: PathBuf,
    pub entries: BTreeMap<String, Entry>,
    pub default_printer: Option<String>,
    snapshots: BTreeMap<PathBuf, String>,
}

pub fn default_directory() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.config_dir().join("ptouch-rs/printers"))
        .unwrap_or_else(|| PathBuf::from(".ptouch-rs/printers"))
}

impl ProfileStore {
    /// A missing directory is an empty collection; merely loading creates nothing.
    pub fn load(directory: impl Into<PathBuf>) -> Result<Self> {
        let directory = directory.into();
        let mut store = Self {
            directory,
            entries: BTreeMap::new(),
            default_printer: None,
            snapshots: BTreeMap::new(),
        };
        if !store.directory.exists() {
            return Ok(store);
        }
        let mut files =
            fs::read_dir(&store.directory)?.collect::<std::result::Result<Vec<_>, _>>()?;
        files.sort_by_key(|f| f.file_name());
        for file in files {
            let path = file.path();
            if path.extension().is_none_or(|x| x != "toml") || !path.is_file() {
                continue;
            }
            let text = fs::read_to_string(&path)?;
            let parsed = parse_file(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            if parsed.version != 1 {
                return Err(format!(
                    "{}: unsupported profile version {}",
                    path.display(),
                    parsed.version
                )
                .into());
            }
            if let Some(default) = parsed.default_printer {
                if store.default_printer.is_some() {
                    return Err(format!("{}: conflicting default printer; declare only one default in the collection", path.display()).into());
                }
                store.default_printer = Some(default);
            }
            for profile in parsed.printers {
                profile
                    .validate()
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                if let Some(old) = store.entries.get(&profile.alias) {
                    return Err(format!(
                        "Duplicate alias '{}' in {} and {}",
                        profile.alias,
                        old.source.display(),
                        path.display()
                    )
                    .into());
                }
                store.entries.insert(
                    profile.alias.clone(),
                    Entry {
                        profile,
                        source: path.clone(),
                    },
                );
            }
            store.snapshots.insert(path, text);
        }
        if let Some(alias) = &store.default_printer
            && !store.entries.contains_key(alias)
        {
            return Err(format!("Default printer '{alias}' does not exist").into());
        }
        Ok(store)
    }

    pub fn selected(&self, alias: Option<&str>) -> Result<Option<&PrinterProfile>> {
        alias
            .or(self.default_printer.as_deref())
            .map(|name| {
                self.entries
                    .get(name)
                    .map(|e| &e.profile)
                    .ok_or_else(|| format!("Unknown printer alias '{name}'").into())
            })
            .transpose()
    }

    /// Add to an existing group file, or default to a file named after the alias.
    pub fn add(&mut self, profile: PrinterProfile, filename: Option<&str>) -> Result<()> {
        profile.validate()?;
        if self.entries.contains_key(&profile.alias) {
            return Err(format!("Printer '{}' already exists", profile.alias).into());
        }
        let filename = filename
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{}.toml", profile.alias));
        if Path::new(&filename).components().count() != 1 || !filename.ends_with(".toml") {
            return Err("Choose a .toml filename inside the profiles directory".into());
        }
        let path = self.directory.join(filename);
        let mut doc = self.document(&path)?;
        if !doc.contains_key("printers") {
            doc["printers"] = Item::ArrayOfTables(Default::default());
        }
        doc["printers"]
            .as_array_of_tables_mut()
            .ok_or("printers must be an array of tables")?
            .push(profile_table(&profile)?);
        self.write(&path, doc)
    }

    /// Update an entry in place, preserving comments and neighbouring printers.
    pub fn update(&mut self, alias: &str, profile: PrinterProfile) -> Result<()> {
        profile.validate()?;
        if profile.alias != alias && self.entries.contains_key(&profile.alias) {
            return Err("Alias already exists".into());
        }
        let path = self
            .entries
            .get(alias)
            .ok_or("Unknown printer")?
            .source
            .clone();
        let mut doc = self.document(&path)?;
        let table = doc["printers"]
            .as_array_of_tables_mut()
            .ok_or("Missing printers")?
            .iter_mut()
            .find(|t| t["alias"].as_str() == Some(alias))
            .ok_or("Missing printer")?;
        let replacement = profile_table(&profile)?;
        for key in [
            "alias",
            "model",
            "connection",
            "host",
            "serial",
            "quality",
            "final_cut",
            "allow_experimental",
        ] {
            if let Some(new) = replacement.get(key) {
                let mut new = new.clone();
                if let (Some(old), Some(v)) =
                    (table.get(key).and_then(Item::as_value), new.as_value_mut())
                {
                    *v.decor_mut() = old.decor().clone();
                }
                table.insert(key, new);
            } else {
                table.remove(key);
            }
        }
        // A default may be declared in a different file. Require a separate
        // explicit change there instead of silently modifying multiple files.
        if self.default_printer.as_deref() == Some(alias) && alias != profile.alias {
            return Err("Clear the default before renaming its printer".into());
        }
        self.write(&path, doc)
    }

    pub fn remove(&mut self, alias: &str) -> Result<()> {
        let path = self
            .entries
            .get(alias)
            .ok_or("Unknown printer")?
            .source
            .clone();
        if self.default_printer.as_deref() == Some(alias) {
            return Err("Clear the default before removing its printer".into());
        }
        let mut doc = self.document(&path)?;
        let tables = doc["printers"]
            .as_array_of_tables_mut()
            .ok_or("Missing printers")?;
        let index = tables
            .iter()
            .position(|t| t["alias"].as_str() == Some(alias))
            .ok_or("Missing printer")?;
        tables.remove(index);
        self.write(&path, doc)
    }

    /// Keep the optional default in its own file, with the same versioned schema.
    pub fn set_default(&mut self, alias: Option<&str>) -> Result<()> {
        if let Some(alias) = alias
            && !self.entries.contains_key(alias)
        {
            return Err("Unknown printer".into());
        }
        // Honour manually authored defaults wherever they were declared.
        let existing: Vec<_> = self
            .snapshots
            .iter()
            .filter_map(|(p, t)| {
                t.parse::<DocumentMut>()
                    .ok()
                    .filter(|d| d.contains_key("default_printer"))
                    .map(|_| p.clone())
            })
            .collect();
        if existing.len() > 1 {
            return Err("Consolidate default_printer into one file before changing it".into());
        }
        let path = existing
            .first()
            .cloned()
            .unwrap_or_else(|| self.directory.join("default.toml"));
        let mut doc = self.document(&path)?;
        if let Some(alias) = alias {
            doc["default_printer"] = value(alias);
        } else {
            doc.remove("default_printer");
        }
        self.write(&path, doc)
    }

    fn document(&self, path: &Path) -> Result<DocumentMut> {
        if let Some(text) = self.snapshots.get(path) {
            Ok(text.parse()?)
        } else {
            Ok("version = 1\n".parse()?)
        }
    }

    fn write(&mut self, path: &Path, doc: DocumentMut) -> Result<()> {
        fs::create_dir_all(&self.directory)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.directory.join(".profiles.lock"))?;
        fs2::FileExt::lock_exclusive(&lock)?;
        let fresh = Self::load(&self.directory)?;
        if fresh.snapshots != self.snapshots {
            return Err("Profile files changed on disk; reload before saving".into());
        }
        // Validate the proposed file before writing anything. Collection-level
        // alias and default checks are performed by add/update/remove.
        let content = doc.to_string();
        let parsed = parse_file(&content)?;
        for profile in &parsed.printers {
            profile.validate()?;
        }
        let mut temp = tempfile::NamedTempFile::new_in(&self.directory)?;
        temp.write_all(content.as_bytes())?;
        temp.as_file().sync_all()?;
        temp.persist(path).map_err(|e| e.error)?;
        *self = Self::load(&self.directory)?;
        Ok(())
    }
}

fn parse_file(text: &str) -> Result<ProfileFile> {
    // Serde flatten cannot combine reliably with deny_unknown_fields. Check
    // printer keys explicitly so typos and connection leftovers are rejected.
    let value: toml::Value = toml::from_str(text)?;
    if let Some(printers) = value.get("printers").and_then(toml::Value::as_array) {
        for printer in printers {
            let table = printer.as_table().ok_or("Each printer must be a table")?;
            let network = table.get("connection").and_then(toml::Value::as_str) == Some("network");
            for key in table.keys() {
                if ![
                    "alias",
                    "model",
                    "connection",
                    "quality",
                    "final_cut",
                    "allow_experimental",
                ]
                .contains(&key.as_str())
                    && key != if network { "host" } else { "serial" }
                {
                    return Err(format!("Unknown printer field '{key}'").into());
                }
            }
        }
    }
    Ok(toml::from_str(text)?)
}

fn profile_table(profile: &PrinterProfile) -> Result<Table> {
    Ok(toml::to_string(profile)?
        .parse::<DocumentMut>()?
        .as_table()
        .clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_typos_versions_and_conflicting_defaults_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ProfileStore::load(dir.path()).unwrap();
        store.add(printer("desk"), None).unwrap();
        let original = fs::read_to_string(dir.path().join("desk.toml")).unwrap();
        for content in [
            original.replace("final_cut", "final_cutt"),
            original.replace("version = 1", "version = 2"),
            format!("{original}\nserial = 'wrong-connection'\n"),
        ] {
            fs::write(dir.path().join("desk.toml"), content).unwrap();
            assert!(ProfileStore::load(dir.path()).is_err());
        }
        fs::write(dir.path().join("desk.toml"), original).unwrap();
        store = ProfileStore::load(dir.path()).unwrap();
        assert!(
            store
                .add(printer("elsewhere"), Some("../outside.toml"))
                .is_err()
        );
        store.set_default(Some("desk")).unwrap();
        fs::write(
            dir.path().join("second.toml"),
            "version = 1\ndefault_printer = 'desk'\n",
        )
        .unwrap();
        assert!(ProfileStore::load(dir.path()).is_err());
    }
    fn printer(alias: &str) -> PrinterProfile {
        PrinterProfile {
            alias: alias.into(),
            model: "PT-P750W".into(),
            connection: Connection::Network {
                host: "printer.local".into(),
            },
            quality: Quality::Standard,
            final_cut: true,
            allow_experimental: false,
        }
    }
    #[test]
    fn mixed_files_comments_and_portability() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ProfileStore::load(dir.path()).unwrap();
        store.add(printer("desk"), Some("office.toml")).unwrap();
        store.add(printer("bench"), Some("office.toml")).unwrap();
        store.add(printer("lab"), None).unwrap();
        let path = dir.path().join("office.toml");
        fs::write(
            &path,
            format!("# My group\n{}", fs::read_to_string(&path).unwrap()),
        )
        .unwrap();
        store = ProfileStore::load(dir.path()).unwrap();
        let mut p = printer("desk");
        p.quality = Quality::High;
        store.update("desk", p).unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("# My group"));
        assert_eq!(store.entries.len(), 3);
        store.remove("bench").unwrap();
        store.set_default(Some("desk")).unwrap();
        let copy = tempfile::tempdir().unwrap();
        for path in store.snapshots.keys() {
            fs::copy(path, copy.path().join(path.file_name().unwrap())).unwrap();
        }
        let moved = ProfileStore::load(copy.path()).unwrap();
        assert_eq!(
            moved.selected(None).unwrap().unwrap().quality,
            Quality::High
        );
        assert_eq!(moved.entries.len(), 2);
    }
    #[test]
    fn conflicts_and_external_edits_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ProfileStore::load(dir.path()).unwrap();
        store.add(printer("desk"), None).unwrap();
        let mut other = store.clone();
        store.add(printer("lab"), None).unwrap();
        assert!(
            other
                .remove("desk")
                .unwrap_err()
                .to_string()
                .contains("changed on disk")
        );
        fs::copy(
            dir.path().join("desk.toml"),
            dir.path().join("duplicate.toml"),
        )
        .unwrap();
        let error = ProfileStore::load(dir.path()).unwrap_err().to_string();
        assert!(error.contains("desk.toml") && error.contains("duplicate.toml"));
    }
}
