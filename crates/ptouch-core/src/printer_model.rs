// SPDX-License-Identifier: MIT
//! Built-in capabilities. User profiles select these; they cannot redefine commands.
use crate::{
    PtouchError, Result,
    device::{self, DeviceFlags},
    protocol::PrintQuality,
    tape::{self, HeadBand},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkDialect {
    P750,
    P900,
    E850,
}

#[derive(Debug, Clone, Copy)]
pub struct PrinterModel {
    pub name: &'static str,
    pub head_dpi: u16,
    pub raster_pins: u16,
    pub network: Option<NetworkDialect>,
    pub usb: bool,
    pub experimental: bool,
    pub high_resolution: bool,
    pub draft: bool,
}

impl PrinterModel {
    pub fn find(name: &str) -> Option<Self> {
        let name = name.to_ascii_uppercase();
        if let Some(info) = device::supported_devices()
            .iter()
            .find(|d| d.name == name && !d.flags.contains(DeviceFlags::PLITE))
        {
            return Some(Self {
                name: info.name,
                head_dpi: info.dpi,
                raster_pins: info.max_px,
                usb: !info
                    .flags
                    .intersects(DeviceFlags::PLITE | DeviceFlags::UNSUP_RASTER),
                network: match info.name {
                    "PT-P750W" | "PT-E550W" => Some(NetworkDialect::P750),
                    _ => None,
                },
                experimental: info.name == "PT-E550W",
                high_resolution: info.name == "PT-P750W"
                    || info.flags.contains(DeviceFlags::LEGACY_HIRES),
                draft: info.flags.contains(DeviceFlags::LEGACY_HIRES),
            });
        }
        let (name, network) = match name.as_str() {
            "PT-P900W" => ("PT-P900W", NetworkDialect::P900),
            "PT-P950NW" => ("PT-P950NW", NetworkDialect::P900),
            "PT-E850TKW" => ("PT-E850TKW", NetworkDialect::E850),
            _ => return None,
        };
        Some(Self {
            name,
            head_dpi: 360,
            raster_pins: 560,
            network: Some(network),
            usb: false,
            experimental: true,
            high_resolution: false,
            draft: false,
        })
    }

    pub fn all() -> Vec<Self> {
        let mut names: Vec<_> = device::supported_devices()
            .iter()
            .filter(|d| !d.flags.contains(DeviceFlags::PLITE))
            .map(|d| d.name)
            .collect();
        names.extend(["PT-P900W", "PT-P950NW", "PT-E850TKW"]);
        names.sort_unstable();
        names.dedup();
        names.into_iter().filter_map(Self::find).collect()
    }

    pub fn validate_quality(self, quality: PrintQuality, media_type: u8) -> Result<()> {
        if (quality == PrintQuality::HighRes && !self.high_resolution)
            || (quality == PrintQuality::Draft && !self.draft)
        {
            return Err(PtouchError::UnsupportedQuality(self.name.into()));
        }
        // Enable new P750 high resolution only on the documented laminated medium.
        if self.name == "PT-P750W" && quality == PrintQuality::HighRes && media_type != 0x01 {
            return Err(PtouchError::UnsupportedOperation(
                "PT-P750W high resolution requires laminated tape",
            ));
        }
        Ok(())
    }

    /// Feed resolution; cross-tape head resolution never changes.
    pub fn feed_dpi(self, quality: PrintQuality) -> u16 {
        match quality {
            PrintQuality::Standard => self.head_dpi,
            PrintQuality::HighRes => self.head_dpi * 2,
            PrintQuality::Draft => self.head_dpi / 2,
        }
    }

    pub fn network_band(self, media_type: u8, width: u8) -> Option<HeadBand> {
        if self.raster_pins == 560 {
            return tape::head_band_560(media_type, width);
        }
        if !matches!(media_type, 0x01 | 0x03) {
            return None;
        }
        // PT-E550W/PT-P750W raster reference v1.02, printable area table.
        let pins = match width {
            4 => 24,
            6 => 32,
            9 => 50,
            12 => 70,
            18 => 112,
            24 => 128,
            _ => return None,
        };
        Some(HeadBand {
            left_pins: (128 - pins) / 2,
            print_pins: pins,
        })
    }
}
