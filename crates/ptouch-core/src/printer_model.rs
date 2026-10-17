// SPDX-License-Identifier: MIT
//! Built-in capabilities. User profiles select these; they cannot redefine commands.
use crate::{
    Result,
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
    pub quality: crate::quality::QualityProfile,
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
                quality: crate::quality::QualityProfile::for_model(info.name, info.dpi),
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
            quality: crate::quality::QualityProfile::for_model(name, 360),
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
        self.quality.mode(quality)?.validate_media(media_type)
    }

    /// Feed amount when suppressing the initial cut, from the raster references.
    /// E850 follows P900 provisionally; its margin remains experimental.
    pub fn margin_dots(self, quality: PrintQuality, precut: bool) -> Result<u16> {
        let dpi = self.quality.mode(quality)?.resolution.feed_dpi;
        if precut {
            return Ok((f64::from(dpi) * 2.0 / 25.4).round() as u16);
        }
        Ok(match self.network {
            Some(NetworkDialect::P900 | NetworkDialect::E850) => 382,
            _ if dpi == 360 => 344,
            _ => 172,
        })
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
