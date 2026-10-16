// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Optional job placement, separate from portable label coordinates.

use crate::{RenderError, Result, bitmap::LabelBitmap};
use serde::{Deserialize, Serialize};

/// A user-measured profile for one printer and print/cut mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrintCalibration {
    pub name: String,
    /// Positive adds leading blank tape; negative removes only leading blank.
    pub offset_mm: f64,
}

impl PrintCalibration {
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() || self.name.len() > 256 {
            return Err(RenderError::Layout(
                "calibration profile needs a name (up to 256 bytes)".into(),
            ));
        }
        if !self.offset_mm.is_finite() || self.offset_mm.abs() > 100.0 {
            return Err(RenderError::Layout(
                "calibration offset must be between -100 and 100 mm".into(),
            ));
        }
        Ok(())
    }

    pub fn from_toml(text: &str) -> Result<Self> {
        let profile: Self = toml::from_str(text)?;
        profile.validate()?;
        Ok(profile)
    }

    pub fn to_toml(&self) -> Result<String> {
        self.validate()?;
        Ok(toml::to_string(self)?)
    }
}

/// Apply a signed correction after rendering/mirroring, before raster encoding.
/// Default and zero corrections preserve every pixel and the label length.
/// The trailing space stays unchanged; all content moves together.
pub fn apply_calibration(
    bitmap: &LabelBitmap,
    dpi: u16,
    offset_mm: Option<f64>,
) -> Result<LabelBitmap> {
    let Some(offset) = offset_mm else {
        return Ok(bitmap.clone());
    };
    if dpi == 0 || !offset.is_finite() || offset.abs() > 100.0 {
        return Err(RenderError::Layout(
            "calibration needs a valid DPI and an offset between -100 and 100 mm".into(),
        ));
    }
    let dots = (offset * f64::from(dpi) / 25.4).round() as i64;
    if dots == 0 {
        return Ok(bitmap.clone());
    }
    let width = i64::from(bitmap.width()) + dots;
    if width <= 0 || width > 1_000_000 || width as u64 * u64::from(bitmap.height()) > 67_108_864 {
        return Err(RenderError::DimensionMismatch(
            "calibration would create an invalid or excessive label length".into(),
        ));
    }
    if dots < 0 {
        for x in 0..(-dots) as u32 {
            if (0..bitmap.height()).any(|y| bitmap.get_pixel(x, y)) {
                return Err(RenderError::Layout("negative calibration would remove printed content; reduce the correction or add leading space".into()));
            }
        }
    }
    let mut corrected = LabelBitmap::new(width as u32, bitmap.height());
    for x in 0..bitmap.width() {
        let target = i64::from(x) + dots;
        if target >= 0 {
            for y in 0..bitmap.height() {
                if bitmap.get_pixel(x, y) {
                    corrected.set_pixel(target as u32, y, true);
                }
            }
        }
    }
    Ok(corrected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_corrections_preserve_ink_and_trailing_space_at_both_dpis() {
        for dpi in [180, 360] {
            let mut original = LabelBitmap::new(80, 16);
            original.set_pixel(30, 5, true);
            original.set_pixel(50, 6, true);
            for mm in [None, Some(0.0), Some(1.0), Some(-1.0)] {
                let dots = (mm.unwrap_or(0.0) * f64::from(dpi) / 25.4).round() as i64;
                let out = apply_calibration(&original, dpi, mm).unwrap();
                assert_eq!(out.width() as i64, 80 + dots);
                assert!(out.get_pixel((30 + dots) as u32, 5));
                assert!(out.get_pixel((50 + dots) as u32, 6));
                if dots == 0 {
                    assert_eq!(out.data(), original.data());
                }
            }
            assert!(apply_calibration(&original, dpi, Some(-10.0)).is_err());
        }
    }

    #[test]
    fn negative_correction_never_discards_ink() {
        let mut original = LabelBitmap::new(80, 16);
        original.set_pixel(5, 3, true);
        assert!(apply_calibration(&original, 180, Some(-1.0)).is_err());
        assert!(apply_calibration(&original.mirrored(true, false), 180, Some(-1.0)).is_ok());
    }

    #[test]
    fn profiles_validate_round_trip_and_reject_invalid_values() {
        let profile = PrintCalibration {
            name: "Desk PT-18R / no cut".into(),
            offset_mm: -0.4,
        };
        assert_eq!(
            PrintCalibration::from_toml(&profile.to_toml().unwrap()).unwrap(),
            profile
        );
        for value in [f64::NAN, f64::INFINITY, 100.1] {
            assert!(
                PrintCalibration {
                    name: "Desk".into(),
                    offset_mm: value
                }
                .validate()
                .is_err()
            );
        }
        assert!(PrintCalibration::from_toml("name = ''\noffset_mm = 1").is_err());
    }
}
