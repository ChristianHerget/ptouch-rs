// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Huang Rui <vowstar@gmail.com>

//! Background worker for non-blocking USB and Bluetooth printer operations.

#[cfg(target_os = "macos")]
use std::io::Write;
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use log::{error, info};
use ptouch_core::device::DeviceFlags;
use ptouch_core::network::PrintOutcome;
use ptouch_core::protocol::PrintQuality;
use ptouch_core::transport::PtouchDevice;

use crate::state::{PrinterCommand, PrinterEvent, PrinterResponse, PrinterTarget};

const POLL_INTERVAL: Duration = Duration::from_secs(3);
const HELPER_ARG: &str = "--ptouch-bluetooth-helper";

pub fn printer_worker(
    cmd_rx: mpsc::Receiver<PrinterCommand>,
    resp_tx: mpsc::Sender<PrinterEvent>,
    ctx: egui::Context,
) {
    info!("Printer worker started");
    let mut current_target = PrinterTarget::Usb;
    // An interrupted job can still be printing. Resume status requests only
    // after an explicit user operation, not the background polling timer.
    let mut poll_enabled = true;
    discover_bluetooth(&resp_tx, &ctx);
    discover_usb(&resp_tx, &ctx);

    loop {
        match cmd_rx.recv_timeout(POLL_INTERVAL) {
            Ok(PrinterCommand::DiscoverBluetooth) => discover_bluetooth(&resp_tx, &ctx),
            Ok(PrinterCommand::Poll(target)) => {
                discover_usb(&resp_tx, &ctx);
                current_target = target;
                poll_enabled = true;
                do_poll(&current_target, &resp_tx, &ctx);
            }
            Ok(PrinterCommand::Print {
                raster_lines,
                expected_media,
                job_timeout_secs,
                chain_print,
                auto_cut,
                precut,
                quality,
                target,
                cancellation,
            }) => {
                current_target = target;
                poll_enabled = do_print(
                    &current_target,
                    (&resp_tx, &ctx),
                    &raster_lines,
                    PrintSettings {
                        expected_media,
                        job_timeout_secs,
                        chain_print,
                        auto_cut,
                        precut,
                        quality,
                    },
                    &cancellation,
                );
            }
            Ok(PrinterCommand::FeedAndCut {
                target,
                cancellation,
            }) => {
                current_target = target;
                poll_enabled = do_feed_and_cut(&current_target, &resp_tx, &ctx, cancellation);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if poll_enabled && !current_target.is_bluetooth() {
                    do_poll(&current_target, &resp_tx, &ctx);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn discover_usb(tx: &mpsc::Sender<PrinterEvent>, ctx: &egui::Context) {
    let devices: Vec<_> = ptouch_core::diagnostics::doctor(false)
        .devices
        .into_iter()
        .filter(|device| device.model.is_some())
        .map(|device| PrinterTarget::UsbAt(device.location))
        .collect();
    #[cfg(windows)]
    let devices = {
        let mut devices = devices;
        match ptouch_core::usbprint::list() {
            Ok(printers) => devices.extend(printers.into_iter().map(|p| PrinterTarget::UsbPrint {
                instance_id: p.instance_id,
            })),
            Err(error) => error!("USBPRINT discovery failed: {error}"),
        }
        devices
    };
    let _ = tx.send(PrinterEvent {
        target: None,
        response: PrinterResponse::UsbDevices(devices),
    });
    ctx.request_repaint();
}

fn discover_bluetooth(resp_tx: &mpsc::Sender<PrinterEvent>, ctx: &egui::Context) {
    #[cfg(target_os = "macos")]
    let devices = run_helper(&["list"], None)
        .map(|output| parse_bluetooth_devices(&output))
        .unwrap_or_else(|message| {
            error!("Bluetooth discovery failed: {message}");
            Vec::new()
        });
    #[cfg(not(target_os = "macos"))]
    let devices = Vec::new();

    let _ = resp_tx.send(PrinterEvent {
        target: None,
        response: PrinterResponse::BluetoothDevices(devices),
    });
    ctx.request_repaint();
}

fn do_poll(target: &PrinterTarget, tx: &mpsc::Sender<PrinterEvent>, ctx: &egui::Context) {
    let response = match target {
        PrinterTarget::Unavailable(e) => Err(e.clone()),
        PrinterTarget::Profile(p) if target.is_network() => poll_network(p),
        PrinterTarget::Profile(_) | PrinterTarget::Usb | PrinterTarget::UsbAt(_) => {
            poll_usb(target)
        }
        #[cfg(windows)]
        PrinterTarget::UsbPrint { .. } => poll_usb(target),
        #[cfg(any(target_os = "macos", test))]
        PrinterTarget::Bluetooth { address, .. } => poll_bluetooth(address),
    }
    .unwrap_or_else(|message| {
        error!("Poll failed: {message}");
        PrinterResponse::ConnectionError(message)
    });
    let _ = tx.send(PrinterEvent {
        target: Some(target.clone()),
        response,
    });
    ctx.request_repaint();
}

fn open_usb(target: &PrinterTarget) -> ptouch_core::Result<PtouchDevice> {
    if let PrinterTarget::Unavailable(e) = target {
        return Err(ptouch_core::PtouchError::StatusError(e.clone()));
    }
    if let PrinterTarget::Profile(p) = target {
        if let ptouch_profiles::Connection::Usb { serial } = &p.connection {
            return PtouchDevice::open_profile(&p.model, serial.as_deref());
        }
        return Err(ptouch_core::PtouchError::UnsupportedOperation(
            "network target cannot use USB",
        ));
    }
    #[cfg(windows)]
    if let PrinterTarget::UsbPrint { instance_id } = target {
        return PtouchDevice::open_usbprint(instance_id);
    }
    target
        .usb_location()
        .map_or_else(PtouchDevice::open_first, PtouchDevice::open_at)
}

fn open_network(
    profile: &ptouch_profiles::PrinterProfile,
) -> Result<ptouch_core::network::NetworkPrinter, String> {
    let ptouch_profiles::Connection::Network { host } = &profile.connection else {
        return Err("Not a network profile".into());
    };
    ptouch_core::network::NetworkPrinter::open(&profile.model, host, profile.allow_experimental)
        .map_err(|e| e.to_string())
}

fn poll_network(profile: &ptouch_profiles::PrinterProfile) -> Result<PrinterResponse, String> {
    let dev = open_network(profile)?;
    let status = dev.status();
    if status.has_error() {
        return Err(status.error_description());
    }
    let band = dev.band().ok_or("Unsupported or missing tape")?;
    Ok(PrinterResponse::Connected {
        model_name: dev.model.name.into(),
        media_width: status.media_width,
        media_type: status.media_type_name().into(),
        media_code: status.media_type,
        max_px: dev.model.raster_pins,
        dpi: dev.model.head_dpi,
        quality_modes: dev.model.high_resolution || dev.model.draft,
        tape_width_px: band.print_pins,
        band_left: Some(band.left_pins),
    })
}

fn poll_usb(target: &PrinterTarget) -> Result<PrinterResponse, String> {
    let mut dev = open_usb(target).map_err(|e| e.to_string())?;
    let max_px = dev.max_px();
    let dpi = dev.device_info().dpi;
    let quality_modes = dev
        .flags()
        .intersects(DeviceFlags::LEGACY_HIRES | DeviceFlags::P750_HIRES);
    let model_name = dev.device_info().name.to_string();
    let status = dev.query_status().map_err(|e| e.to_string())?;
    let media_width = status.media_width;
    let media_code = status.media_type;
    let media_type = status.media_type_name().to_string();
    let tape_width_px = dev.tape_width_px().unwrap_or(max_px);
    let response = PrinterResponse::Connected {
        model_name,
        media_width,
        media_type,
        max_px,
        dpi,
        quality_modes,
        media_code,
        band_left: None,
        tape_width_px,
    };
    let _ = dev.close();
    Ok(response)
}

#[cfg(any(target_os = "macos", test))]
fn poll_bluetooth(address: &str) -> Result<PrinterResponse, String> {
    #[cfg(target_os = "macos")]
    {
        let output = run_helper(&["status", address], None)?;
        parse_bluetooth_status(&output)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = address;
        Err("Bluetooth is available on macOS only".to_string())
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_bluetooth_devices(output: &str) -> Vec<PrinterTarget> {
    output
        .lines()
        .filter_map(|line| {
            let (address, name) = line.split_once('\t')?;
            Some(PrinterTarget::Bluetooth {
                name: name.to_string(),
                address: address.to_string(),
            })
        })
        .collect()
}

#[cfg(any(target_os = "macos", test))]
fn parse_bluetooth_status(output: &str) -> Result<PrinterResponse, String> {
    let fields: Vec<_> = output.trim_end().split('\t').collect();
    if fields.len() != 6 {
        return Err("Bluetooth helper returned invalid status".to_string());
    }
    Ok(PrinterResponse::Connected {
        model_name: fields[0].to_string(),
        media_width: fields[1].parse().map_err(|_| "Invalid media width")?,
        media_type: fields[2].to_string(),
        max_px: fields[3].parse().map_err(|_| "Invalid raster width")?,
        dpi: fields[4].parse().map_err(|_| "Invalid resolution")?,
        quality_modes: false,
        media_code: 1,
        band_left: None,
        tape_width_px: fields[5].parse().map_err(|_| "Invalid tape width")?,
    })
}

struct PrintSettings {
    expected_media: (u8, u8),
    job_timeout_secs: u32,
    chain_print: bool,
    auto_cut: bool,
    precut: bool,
    quality: PrintQuality,
}

fn do_print(
    target: &PrinterTarget,
    response: (&mpsc::Sender<PrinterEvent>, &egui::Context),
    raster_lines: &[Vec<u8>],
    settings: PrintSettings,
    cancellation: &ptouch_core::CancellationToken,
) -> bool {
    let PrintSettings {
        expected_media,
        job_timeout_secs,
        chain_print,
        auto_cut,
        precut,
        quality,
    } = settings;
    let (tx, ctx) = response;
    if let PrinterTarget::Profile(p) = target
        && target.is_network()
    {
        let result = (|| {
            let mut dev = open_network(p)?;
            if (dev.status().media_width, dev.status().media_type) != expected_media {
                return Err("Tape changed after rendering; refresh before printing".to_string());
            }
            dev.set_cancellation_token(cancellation.clone());
            dev.set_job_timeout(Duration::from_secs(u64::from(job_timeout_secs)))
                .map_err(|e| e.to_string())?;
            dev.print_raster(raster_lines, chain_print, precut, quality)
                .map_err(|e| e.to_string())
        })();
        let completed = matches!(result, Ok(ptouch_core::network::PrintOutcome::Complete));
        let response = match result {
            Ok(ptouch_core::network::PrintOutcome::Complete) => PrinterResponse::PrintDone,
            Ok(ptouch_core::network::PrintOutcome::SubmittedUnconfirmed) => {
                PrinterResponse::SubmittedUnconfirmed
            }
            Err(e) => PrinterResponse::Error(e),
        };
        let _ = tx.send(PrinterEvent {
            target: Some(target.clone()),
            response,
        });
        ctx.request_repaint();
        return completed;
    }
    let result = match target {
        PrinterTarget::Unavailable(e) => Err(e.clone()),
        PrinterTarget::Profile(_) | PrinterTarget::Usb | PrinterTarget::UsbAt(_) => print_usb(
            raster_lines,
            chain_print,
            auto_cut,
            quality,
            target,
            job_timeout_secs,
            cancellation.clone(),
        ),
        #[cfg(windows)]
        PrinterTarget::UsbPrint { .. } => print_usb(
            raster_lines,
            chain_print,
            auto_cut,
            quality,
            target,
            job_timeout_secs,
            cancellation.clone(),
        ),
        #[cfg(any(target_os = "macos", test))]
        PrinterTarget::Bluetooth { address, .. } => {
            print_bluetooth(address, raster_lines).map(|()| PrintOutcome::Complete)
        }
    };
    let completed = matches!(result, Ok(PrintOutcome::Complete));
    let response = match result {
        Ok(PrintOutcome::Complete) => PrinterResponse::PrintDone,
        Ok(PrintOutcome::SubmittedUnconfirmed) => PrinterResponse::SubmittedUnconfirmed,
        Err(e) => PrinterResponse::Error(e),
    };
    let _ = tx.send(PrinterEvent {
        target: Some(target.clone()),
        response,
    });
    ctx.request_repaint();
    completed
}

fn print_usb(
    raster_lines: &[Vec<u8>],
    chain_print: bool,
    auto_cut: bool,
    quality: PrintQuality,
    target: &PrinterTarget,
    job_timeout_secs: u32,
    cancellation: ptouch_core::CancellationToken,
) -> Result<PrintOutcome, String> {
    let mut dev = open_usb(target).map_err(|e| format!("Connect error: {e}"))?;
    dev.set_cancellation_token(cancellation);
    dev.set_job_timeout(Duration::from_secs(u64::from(job_timeout_secs)))
        .map_err(|e| e.to_string())?;
    dev.init().map_err(|e| format!("Init error: {e}"))?;
    let result = match dev.print_raster(raster_lines, chain_print, auto_cut, quality) {
        Ok(()) => Ok(PrintOutcome::Complete),
        Err(ptouch_core::PtouchError::CompletionUnknown) => Ok(PrintOutcome::SubmittedUnconfirmed),
        Err(e) => Err(format!("Print error: {e}")),
    };
    let _ = dev.close();
    result
}

#[cfg(any(target_os = "macos", test))]
fn print_bluetooth(address: &str, raster_lines: &[Vec<u8>]) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let bytes: Vec<u8> = raster_lines.iter().flatten().copied().collect();
        run_helper(&["print", address], Some(&bytes)).map(|_| ())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (address, raster_lines);
        Err("Bluetooth is available on macOS only".to_string())
    }
}

fn do_feed_and_cut(
    target: &PrinterTarget,
    tx: &mpsc::Sender<PrinterEvent>,
    ctx: &egui::Context,
    cancellation: ptouch_core::CancellationToken,
) -> bool {
    let result = if target.is_network() {
        Err("Standalone network feed and cut is not supported".to_string())
    } else if target.is_bluetooth() {
        Err("PT-P300BT has a manual cutter".to_string())
    } else {
        (|| {
            let mut dev = open_usb(target).map_err(|e| format!("Connect error: {e}"))?;
            dev.set_cancellation_token(cancellation);
            dev.init().map_err(|e| format!("Init error: {e}"))?;
            let result = dev
                .feed_and_cut()
                .map_err(|e| format!("Feed & cut error: {e}"));
            let _ = dev.close();
            result
        })()
    };
    let completed = result.is_ok();
    let response = result
        .map(|()| PrinterResponse::FeedAndCutDone)
        .unwrap_or_else(PrinterResponse::Error);
    let _ = tx.send(PrinterEvent {
        target: Some(target.clone()),
        response,
    });
    ctx.request_repaint();
    completed
}

#[cfg(target_os = "macos")]
fn run_helper(args: &[&str], stdin: Option<&[u8]>) -> Result<String, String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(executable)
        .arg(HELPER_ARG)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(bytes) = stdin {
        child
            .stdin
            .take()
            .ok_or("Cannot open Bluetooth helper input")?
            .write_all(bytes)
            .map_err(|e| e.to_string())?;
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if output.status.success() {
        String::from_utf8(output.stdout).map_err(|e| e.to_string())
    } else {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if message.is_empty() {
            "Bluetooth helper failed".to_string()
        } else {
            message
        })
    }
}

/// Run a Bluetooth operation before eframe claims the process's main thread.
pub fn run_bluetooth_helper_from_args() -> Option<i32> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some(HELPER_ARG) {
        return None;
    }
    #[cfg(target_os = "macos")]
    let result = run_native_helper(&args[2..]);
    #[cfg(not(target_os = "macos"))]
    let result: Result<(), String> = Err("Bluetooth is available on macOS only".to_string());
    match result {
        Ok(()) => Some(0),
        Err(message) => {
            eprintln!("{message}");
            Some(1)
        }
    }
}

#[cfg(target_os = "macos")]
fn run_native_helper(args: &[String]) -> Result<(), String> {
    use ptouch_core::BluetoothDevice;
    use std::io::Read;

    match args.first().map(String::as_str) {
        Some("list") => {
            for device in BluetoothDevice::paired_devices().map_err(|e| e.to_string())? {
                if device.name.to_ascii_uppercase().starts_with("PT-P300BT") {
                    println!(
                        "{}\t{}",
                        device.address,
                        device.name.replace(['\t', '\n'], " ")
                    );
                }
            }
            Ok(())
        }
        Some("status") => {
            let address = args.get(1).ok_or("Missing Bluetooth address")?;
            let mut device = BluetoothDevice::open(address).map_err(|e| e.to_string())?;
            device.init().map_err(|e| e.to_string())?;
            let status = device.status().ok_or("Printer returned no status")?;
            let tape_width_px = device.tape_width_px().ok_or("Unsupported tape width")?;
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                device.model_name(),
                status.media_width,
                status.media_type_name(),
                device.raster_width_px(),
                device.dpi(),
                tape_width_px
            );
            device.close().map_err(|e| e.to_string())
        }
        Some("print") => {
            let address = args.get(1).ok_or("Missing Bluetooth address")?;
            let mut bytes = Vec::new();
            std::io::stdin()
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            const LINE_BYTES: usize = 16;
            if bytes.is_empty() || bytes.len() % LINE_BYTES != 0 {
                return Err("Invalid PT-P300BT raster data".to_string());
            }
            let lines: Vec<Vec<u8>> = bytes.chunks_exact(LINE_BYTES).map(<[u8]>::to_vec).collect();
            let mut device = BluetoothDevice::open(address).map_err(|e| e.to_string())?;
            device.init().map_err(|e| e.to_string())?;
            device.print_raster(&lines).map_err(|e| e.to_string())?;
            device.close().map_err(|e| e.to_string())
        }
        _ => Err("Unknown Bluetooth helper operation".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bluetooth_device_list() {
        let devices = parse_bluetooth_devices("EC:79:49:61:CC:F8\tPT-P300BT6427\n");
        assert_eq!(
            devices,
            [PrinterTarget::Bluetooth {
                name: "PT-P300BT6427".to_string(),
                address: "EC:79:49:61:CC:F8".to_string(),
            }]
        );
    }

    #[test]
    fn parses_bluetooth_status_geometry() {
        let response = parse_bluetooth_status("PT-P300BT\t12\tLaminated tape\t128\t180\t64\n")
            .expect("valid status");
        assert!(matches!(
            response,
            PrinterResponse::Connected {
                media_width: 12,
                max_px: 128,
                dpi: 180,
                tape_width_px: 64,
                ..
            }
        ));
    }

    #[test]
    fn rejects_malformed_bluetooth_status() {
        assert!(parse_bluetooth_status("PT-P300BT\t12").is_err());
    }
}
