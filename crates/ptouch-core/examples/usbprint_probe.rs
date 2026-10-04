// SPDX-License-Identifier: GPL-3.0-or-later
//! Experimental PT-P710BT status probe through the existing Windows USBPRINT driver.
//! Does not print, cut, reset, install drivers, or modify registry settings.

#[cfg(windows)]
#[path = "support/usbprint_windows.rs"]
mod platform;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    return platform::run();
    #[cfg(not(windows))]
    Err(
        "usbprint_probe requires Windows; the application still uses libusb for USB printing"
            .into(),
    )
}
