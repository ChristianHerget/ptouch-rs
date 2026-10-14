// SPDX-License-Identifier: MIT
//! Print modes and raster resolution, independent of transport and rendering.

use crate::{PtouchError, Result, protocol::PrintQuality};
use serde::{Deserialize, Serialize};

/// Resolution across the print head and along the tape feed, respectively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrintResolution {
    pub head_dpi: u16,
    pub feed_dpi: u16,
}

impl PrintResolution {
    pub const fn square(dpi: u16) -> Self {
        Self {
            head_dpi: dpi,
            feed_dpi: dpi,
        }
    }

    pub fn length_mm(self, columns: usize) -> f64 {
        columns as f64 * 25.4 / f64::from(self.feed_dpi)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaRequirement {
    Any,
    Laminated,
    /// Enforced by the legacy info command. Generic status cannot identify HG tape.
    HighGrade,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QualityMode {
    pub quality: PrintQuality,
    pub resolution: PrintResolution,
    pub media: MediaRequirement,
}

impl QualityMode {
    pub fn label(self) -> String {
        let name = match self.quality {
            PrintQuality::Standard => "Standard",
            PrintQuality::HighRes => "High resolution",
            PrintQuality::Draft => "Draft (high speed)",
        };
        format!(
            "{name} ({}×{} dpi)",
            self.resolution.head_dpi, self.resolution.feed_dpi
        )
    }

    pub fn requirement(self) -> &'static str {
        match self.media {
            MediaRequirement::Any => "",
            MediaRequirement::Laminated => "Requires laminated tape.",
            MediaRequirement::HighGrade => {
                "Requires HG tape; the printer checks tape compatibility."
            }
        }
    }

    pub fn validate_media(self, media_type: u8) -> Result<()> {
        if self.media == MediaRequirement::Laminated && media_type != 0x01 {
            return Err(PtouchError::UnsupportedOperation(
                "high resolution requires laminated tape",
            ));
        }
        Ok(())
    }

    pub fn validate_resolution(self, actual: PrintResolution) -> Result<()> {
        if actual != self.resolution {
            return Err(PtouchError::RasterResolution {
                expected: self.resolution,
                actual,
            });
        }
        Ok(())
    }
}

/// USB command selection is separate from a mode's capabilities and raster DPI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityCommands {
    None,
    LegacyInfo,
    AdvancedMode,
}

#[derive(Debug, Clone, Copy)]
pub struct QualityProfile {
    pub modes: &'static [QualityMode],
    pub usb_commands: QualityCommands,
}

const fn mode(
    quality: PrintQuality,
    head_dpi: u16,
    feed_dpi: u16,
    media: MediaRequirement,
) -> QualityMode {
    QualityMode {
        quality,
        resolution: PrintResolution { head_dpi, feed_dpi },
        media,
    }
}

const STANDARD_180: QualityMode = mode(PrintQuality::Standard, 180, 180, MediaRequirement::Any);
const STANDARD_360: QualityMode = mode(PrintQuality::Standard, 360, 360, MediaRequirement::Any);

const LEGACY_MODES: &[QualityMode] = &[
    STANDARD_360,
    mode(PrintQuality::HighRes, 360, 720, MediaRequirement::HighGrade),
    mode(PrintQuality::Draft, 360, 180, MediaRequirement::HighGrade),
];
const P750_MODES: &[QualityMode] = &[
    STANDARD_180,
    mode(PrintQuality::HighRes, 180, 360, MediaRequirement::Laminated),
];

impl QualityProfile {
    /// The only model-to-quality mapping. Hardware modes without an implemented
    /// backend deliberately remain unavailable.
    pub fn for_model(name: &str, head_dpi: u16) -> Self {
        match name {
            "PT-9500PC" | "PT-9700PC" => Self {
                modes: LEGACY_MODES,
                usb_commands: QualityCommands::LegacyInfo,
            },
            "PT-P750W" => Self {
                modes: P750_MODES,
                usb_commands: QualityCommands::AdvancedMode,
            },
            _ => Self {
                modes: if head_dpi == 360 {
                    &[STANDARD_360]
                } else {
                    &[STANDARD_180]
                },
                usb_commands: QualityCommands::None,
            },
        }
    }

    pub fn mode(self, quality: PrintQuality) -> Result<QualityMode> {
        self.modes
            .iter()
            .copied()
            .find(|m| m.quality == quality)
            .ok_or_else(|| PtouchError::UnsupportedQuality("this model".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_describe_only_implemented_resolutions() {
        for (name, head, feeds) in [
            ("PT-P750W", 180, vec![180, 360]),
            ("PT-9500PC", 360, vec![360, 720, 180]),
            ("PT-9700PC", 360, vec![360, 720, 180]),
            ("PT-P900W", 360, vec![360]),
            ("PT-P700", 180, vec![180]),
        ] {
            let profile = QualityProfile::for_model(name, head);
            assert_eq!(
                profile
                    .modes
                    .iter()
                    .map(|m| m.resolution.feed_dpi)
                    .collect::<Vec<_>>(),
                feeds
            );
            assert!(profile.modes.iter().all(|m| m.resolution.head_dpi == head));
        }
    }

    #[test]
    fn mode_validates_resolution_and_media() {
        let mode = QualityProfile::for_model("PT-P750W", 180)
            .mode(PrintQuality::HighRes)
            .unwrap();
        assert_eq!(mode.label(), "High resolution (180×360 dpi)");
        assert!(mode.validate_media(0x01).is_ok());
        assert!(mode.validate_media(0x03).is_err());
        assert!(
            mode.validate_resolution(PrintResolution::square(180))
                .is_err()
        );
        assert!(mode.validate_resolution(mode.resolution).is_ok());
        assert_eq!(mode.resolution.length_mm(360), 25.4);
    }
}
