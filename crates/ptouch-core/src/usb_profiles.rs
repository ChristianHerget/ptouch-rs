// SPDX-License-Identifier: GPL-3.0-or-later
//! USB identities for portable profiles. Discovery never claims an interface.

#[cfg(not(windows))]
use crate::transport::UsbLocation;
use crate::{DeviceFlags, PtouchError, Result, device};
#[cfg(not(windows))]
use rusb::{Context, UsbContext};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Selector {
    #[cfg(not(windows))]
    Libusb(UsbLocation),
    #[cfg(any(windows, test))]
    Windows { instance_id: String, usbprint: bool },
}

/// Detected physical printer. Only `serial` is a portable selection key.
#[derive(Debug, Clone)]
pub struct UsbCandidate {
    pub model: &'static str,
    pub serial: Option<String>,
    /// A failed serial read is distinct from a device with no serial descriptor.
    pub identity_error: Option<String>,
    pub(crate) selector: Selector,
}

impl UsbCandidate {
    pub(crate) fn matches_serial(&self, serial: &str) -> bool {
        self.serial.as_deref().is_some_and(|detected| {
            // Windows device instance IDs are case-insensitive and PnP may
            // normalize the serial's case. Unix USB descriptors preserve it.
            match &self.selector {
                #[cfg(any(windows, test))]
                Selector::Windows { .. } => detected.eq_ignore_ascii_case(serial),
                #[cfg(not(windows))]
                Selector::Libusb(_) => detected == serial,
            }
        })
    }
}

pub(crate) fn select<'a>(
    devices: &'a [UsbCandidate],
    model: &str,
    serial: Option<&str>,
) -> Result<&'a UsbCandidate> {
    let mut matches = Vec::new();
    for device in devices
        .iter()
        .filter(|d| d.model.eq_ignore_ascii_case(model))
    {
        if serial.is_some()
            && let Some(error) = &device.identity_error
        {
            return Err(PtouchError::StatusError(format!(
                "Cannot verify USB serial for {}: {error}",
                device.model
            )));
        }
        if serial.is_none_or(|serial| device.matches_serial(serial)) {
            matches.push(device);
        }
    }
    match matches.as_slice() {
        [] => Err(PtouchError::DeviceNotFound),
        [device] => Ok(device),
        _ => Err(PtouchError::AmbiguousDevice),
    }
}

/// Read supported printers' USB descriptors and serials without claiming their
/// interfaces or issuing printer commands. On Windows, PnP is the authoritative
/// physical inventory, so libusb and USBPRINT cannot duplicate the same printer.
#[cfg(not(windows))]
pub fn discover() -> Result<Vec<UsbCandidate>> {
    let context = Context::new()?;
    let mut candidates = Vec::new();
    for usb in context.devices()?.iter() {
        let Ok(desc) = usb.device_descriptor() else {
            continue;
        };
        let Some(info) = device::find_device(desc.vendor_id(), desc.product_id()) else {
            continue;
        };
        if info
            .flags
            .intersects(DeviceFlags::PLITE | DeviceFlags::UNSUP_RASTER)
        {
            continue;
        }
        let serial = if desc.serial_number_string_index().is_none() {
            Ok(None)
        } else {
            usb.open()
                .and_then(|handle| handle.read_serial_number_string_ascii(&desc))
                .map(|s| (!s.trim().is_empty()).then(|| s.trim().to_owned()))
        };
        let (serial, identity_error) = match serial {
            Ok(serial) => (serial, None),
            Err(error) => (None, Some(error.to_string())),
        };
        candidates.push(UsbCandidate {
            model: info.name,
            serial,
            identity_error,
            selector: Selector::Libusb(UsbLocation::of(&usb)),
        });
    }
    Ok(candidates)
}

#[cfg(windows)]
pub fn discover() -> Result<Vec<UsbCandidate>> {
    from_bindings(&crate::diagnostics::native_bindings()?)
}

#[cfg(any(windows, test))]
fn from_bindings(bindings: &[crate::diagnostics::DriverBinding]) -> Result<Vec<UsbCandidate>> {
    let mut candidates: Vec<UsbCandidate> = Vec::new();
    for binding in bindings {
        let Some(info) = device::supported_devices().iter().find(|info| {
            !info
                .flags
                .intersects(DeviceFlags::PLITE | DeviceFlags::UNSUP_RASTER)
                && binding.hardware_ids.iter().any(|id| {
                    id.to_ascii_uppercase()
                        .strip_prefix(&format!("USB\\VID_{:04X}&PID_{:04X}", info.vid, info.pid))
                        .is_some_and(|tail| tail.is_empty() || tail.starts_with("&REV_"))
                })
        }) else {
            continue;
        };
        // Exclude composite-interface identities: a profile identifies the physical device.
        let upper = binding.instance_id.to_ascii_uppercase();
        if !upper.starts_with("USB\\") || upper.contains("&MI_") {
            continue;
        }
        if candidates.iter().any(|d| matches!(&d.selector, Selector::Windows { instance_id, .. } if instance_id.eq_ignore_ascii_case(&binding.instance_id))) { continue; }
        let usbprint = info.name == "PT-P710BT"
            && binding
                .service
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("usbprint"));
        let serial = verified_windows_serial(&binding.instance_id, binding.unique_id);
        candidates.push(UsbCandidate {
            model: info.name,
            serial,
            identity_error: binding.identity_error.clone(),
            selector: Selector::Windows {
                instance_id: binding.instance_id.clone(),
                usbprint,
            },
        });
    }
    Ok(candidates)
}

/// A physical USB instance suffix is a serial only when the bus reports UniqueID.
/// Without that capability Windows generates a location-dependent suffix.
#[cfg(any(windows, test))]
fn verified_windows_serial(instance: &str, unique: bool) -> Option<String> {
    if !unique {
        return None;
    }
    let parts: Vec<_> = instance.split('\\').collect();
    if parts.len() != 3
        || !parts[0].eq_ignore_ascii_case("USB")
        || parts[1].to_ascii_uppercase().contains("&MI_")
        || parts[2].is_empty()
    {
        return None;
    }
    Some(parts[2].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_serials_on_distinct_printers_are_ambiguous() {
        let mut first = binding("A", "WinUSB");
        let mut second = binding("B", "WinUSB");
        first.unique_id = false;
        second.unique_id = false;
        let mut devices = from_bindings(&[first, second]).unwrap();
        for device in &mut devices {
            device.serial = Some("same".into());
        }
        assert!(matches!(
            select(&devices, "PT-P710BT", Some("same")),
            Err(PtouchError::AmbiguousDevice)
        ));
        assert!(matches!(
            select(&devices, "PT-P710BT", None),
            Err(PtouchError::AmbiguousDevice)
        ));
    }

    #[test]
    fn windows_serial_comparison_accepts_pnp_case_normalization() {
        let devices = from_bindings(&[binding("ABC123", "usbprint")]).unwrap();
        assert!(select(&devices, "PT-P710BT", Some("abc123")).is_ok());
    }

    fn binding(serial: &str, service: &str) -> crate::diagnostics::DriverBinding {
        crate::diagnostics::DriverBinding {
            instance_id: format!("USB\\VID_04F9&PID_20AF\\{serial}"),
            hardware_ids: vec!["USB\\VID_04F9&PID_20AF".into()],
            service: Some(service.into()),
            description: None,
            unique_id: true,
            identity_error: None,
        }
    }

    #[test]
    fn native_inventory_deduplicates_interfaces_and_selects_usbprint() {
        let bindings = [binding("A", "usbprint"), binding("A", "usbprint")];
        let devices = from_bindings(&bindings).unwrap();
        assert_eq!(devices.len(), 1);
        for serial in [None, Some("A")] {
            assert!(matches!(
                select(&devices, "PT-P710BT", serial).unwrap().selector,
                Selector::Windows { usbprint: true, .. }
            ));
        }
        let devices = from_bindings(&[binding("A", "usbprint"), binding("B", "WinUSB")]).unwrap();
        assert!(matches!(
            select(&devices, "PT-P710BT", None),
            Err(PtouchError::AmbiguousDevice)
        ));
        assert!(matches!(
            select(&devices, "PT-P710BT", Some("B")).unwrap().selector,
            Selector::Windows {
                usbprint: false,
                ..
            }
        ));
    }

    #[test]
    fn absent_and_unreadable_serials_are_not_interchangeable() {
        let mut b = binding("7&123&0&2", "usbprint");
        b.unique_id = false;
        let devices = from_bindings(&[b]).unwrap();
        assert!(devices[0].serial.is_none());
        assert!(select(&devices, "PT-P710BT", None).is_ok());
        assert!(select(&devices, "PT-P710BT", Some("7&123&0&2")).is_err());
        let mut b = binding("B", "usbprint");
        b.identity_error = Some("Access denied".into());
        assert!(matches!(
            select(
                &from_bindings(&[binding("A", "usbprint"), b]).unwrap(),
                "PT-P710BT",
                Some("A")
            ),
            Err(PtouchError::StatusError(_))
        ));
        assert!(
            select(
                &from_bindings(&[binding("A", "usbprint"), binding("A", "WinUSB")]).unwrap(),
                "PT-P700",
                None
            )
            .is_err()
        );
    }
}
