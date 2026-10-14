// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Brother P-Touch printer protocol library.
//!
//! This crate provides low-level communication with Brother P-Touch label
//! printers over USB and optionally native macOS Bluetooth. It handles device
//! discovery, protocol command construction, status parsing, and raster data
//! transmission.

mod model;
pub mod network;
pub mod network_host;
mod p300bt;
pub mod printer_model;
pub mod quality;
mod session;
mod snmp;

/// Native macOS Bluetooth support, enabled with the `bluetooth` feature.
#[cfg(all(feature = "bluetooth", target_os = "macos"))]
pub mod bluetooth;

pub mod control;
pub mod device;
pub mod diagnostics;
pub mod error;
pub mod protocol;
pub mod status;
pub mod tape;
pub mod transport;
pub mod usb_profiles;

// Re-export commonly used types at the crate root.
#[cfg(all(feature = "bluetooth", target_os = "macos"))]
pub use bluetooth::BluetoothDevice;
pub use control::CancellationToken;
pub use device::{DeviceFlags, DeviceInfo};
pub use error::{PtouchError, Result};
pub use protocol::PrintQuality;
pub use quality::PrintResolution;
pub use status::PrinterStatus;
pub use tape::TapeInfo;
pub use transport::PtouchDevice;

/// Windows USBPRINT transport for PT-P710BT.
#[cfg(windows)]
pub mod usbprint;
