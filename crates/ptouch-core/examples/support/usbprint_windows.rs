// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    io,
    mem::{offset_of, size_of},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    process::{Command, Stdio},
    ptr,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Devices::DeviceAndDriverInstallation::*,
    Foundation::{
        ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS, GENERIC_READ, GENERIC_WRITE, GetLastError,
        INVALID_HANDLE_VALUE,
    },
    Graphics::Printing::GUID_DEVINTERFACE_USBPRINT,
    Storage::FileSystem::{CreateFileW, OPEN_EXISTING, ReadFile, WriteFile},
};

struct DeviceSet(HDEVINFO);
impl Drop for DeviceSet {
    fn drop(&mut self) {
        // SAFETY: this set is owned and was returned by SetupDiGetClassDevsW.
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

fn supported_path(path: &str) -> bool {
    let lowercase = path.to_ascii_lowercase();
    lowercase
        .strip_prefix(r"\\?\usb#vid_04f9&pid_20af")
        .is_some_and(|rest| rest.starts_with('#') || rest.starts_with('&'))
}

fn interfaces() -> io::Result<Vec<String>> {
    // SAFETY: static GUID, null optional arguments, valid flags.
    let raw = unsafe {
        SetupDiGetClassDevsW(
            &GUID_DEVINTERFACE_USBPRINT,
            ptr::null(),
            ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if raw == -1 {
        return Err(io::Error::last_os_error());
    }
    let set = DeviceSet(raw);
    let mut paths = Vec::new();
    for index in 0..4096 {
        let mut interface = SP_DEVICE_INTERFACE_DATA {
            cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: interface is writable with cbSize set and set remains alive.
        if unsafe {
            SetupDiEnumDeviceInterfaces(
                set.0,
                ptr::null(),
                &GUID_DEVINTERFACE_USBPRINT,
                index,
                &mut interface,
            )
        } == 0
        {
            if unsafe { GetLastError() } == ERROR_NO_MORE_ITEMS {
                return Ok(paths);
            }
            return Err(io::Error::last_os_error());
        }
        let mut required = 0u32;
        // SAFETY: null buffer with zero length requests the required byte count.
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &interface,
                ptr::null_mut(),
                0,
                &mut required,
                ptr::null_mut(),
            );
        }
        if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            return Err(io::Error::last_os_error());
        }
        if required < size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32 || required > 65536 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid interface detail length",
            ));
        }
        // usize storage provides sufficient alignment for the Windows detail structure.
        let mut storage = vec![0usize; (required as usize).div_ceil(size_of::<usize>())];
        let detail = storage
            .as_mut_ptr()
            .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
        // SAFETY: allocation is aligned and at least required bytes, including the header.
        unsafe {
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        }
        if unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &interface,
                detail,
                required,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let offset = offset_of!(SP_DEVICE_INTERFACE_DETAIL_DATA_W, DevicePath);
        // SAFETY: DevicePath is UTF-16 aligned and the slice stays within the advertised byte count.
        let units = unsafe {
            std::slice::from_raw_parts(
                storage.as_ptr().cast::<u8>().add(offset).cast::<u16>(),
                (required as usize - offset) / 2,
            )
        };
        let end = units.iter().position(|unit| *unit == 0).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "Unterminated device path")
        })?;
        let path = String::from_utf16(&units[..end])
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if supported_path(&path) {
            paths.push(path);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Too many USBPRINT interfaces",
    ))
}

fn query_status(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Do not accept arbitrary file/device paths even in the internal worker mode.
    if !interfaces()?.iter().any(|candidate| candidate == path) {
        return Err("Selected PT-P710BT interface disappeared".into());
    }
    let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    // SAFETY: NUL-terminated path, synchronous I/O, no security attributes or template handle.
    let raw = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: CreateFileW returned an owned handle. OwnedHandle closes it after synchronous I/O completes.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let command = ptouch_core::protocol::cmd_status_request();
    let mut written = 0;
    // SAFETY: the command and count remain live until this synchronous call returns.
    if unsafe {
        WriteFile(
            handle.as_raw_handle(),
            command.as_ptr(),
            command.len() as u32,
            &mut written,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    if written as usize != command.len() {
        return Err("Short status-request write; not retrying".into());
    }
    let mut pending = Vec::new();
    for _ in 0..64 {
        let mut bytes = [0u8; 64];
        let mut received = 0;
        // SAFETY: synchronous read into a live, writable buffer of the advertised size.
        if unsafe {
            ReadFile(
                handle.as_raw_handle(),
                bytes.as_mut_ptr(),
                bytes.len() as u32,
                &mut received,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        if received == 0 || received as usize > bytes.len() {
            return Err("Invalid or empty status read".into());
        }
        pending.extend_from_slice(&bytes[..received as usize]);
        while pending.len() >= 32 {
            let status = ptouch_core::PrinterStatus::from_bytes(&pending[..32])
                .ok_or("Invalid status packet")?;
            pending.drain(..32);
            if status.print_head_mark != 0x80 || status.size != 0x20 || status.brother_code != 0x42
            {
                return Err("Unexpected Brother status header".into());
            }
            if status.status_type == 0 {
                println!(
                    "USBPRINT status received: tape={}mm, status errors={}",
                    status.media_width,
                    status.error_description()
                );
                return Ok(());
            }
        }
    }
    Err("No query response within 64 reads".into())
}

pub(super) fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--help"] {
        println!(
            "usbprint_probe [--list | --status DEVICE_PATH]\nExperimental PT-P710BT query only; no printing or driver changes."
        );
        return Ok(());
    }
    if args.len() == 2 && args[0] == "--worker" {
        return query_status(&args[1]);
    }
    if args.is_empty() || args == ["--list"] {
        let paths = interfaces()?;
        for path in &paths {
            println!("{path}");
        }
        if paths.is_empty() {
            println!("No PT-P710BT USBPRINT interfaces found");
        }
        return Ok(());
    }
    if args.len() != 2 || args[0] != "--status" {
        return Err("Use --list or --status DEVICE_PATH".into());
    }
    let path = &args[1];
    if !interfaces()?.contains(path) {
        return Err("Device path is not present; run --list again".into());
    }
    // The child owns synchronous I/O buffers. Timeout terminates that process;
    // no caller buffer is freed while a background thread still uses it.
    let mut child = Command::new(std::env::current_exe()?)
        .args(["--worker", path])
        .stdin(Stdio::null())
        .spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(format!("USBPRINT probe failed: {status}").into())
            };
        }
        if started.elapsed() >= Duration::from_secs(15) {
            child.kill()?;
            child.wait()?;
            return Err("USBPRINT probe timed out after 15 seconds; child terminated".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_only_exact_p710bt_usb_paths() {
        assert!(supported_path(r"\\?\usb#vid_04f9&pid_20af#serial#{guid}"));
        assert!(supported_path(
            r"\\?\USB#VID_04F9&PID_20AF&MI_00#serial#{guid}"
        ));
        for path in [
            r"C:\file",
            r"\\?\usb#vid_04f9&pid_20aff#serial",
            r"\\?\usb#vid_04f9&pid_205e#serial",
        ] {
            assert!(!supported_path(path));
        }
    }
}
