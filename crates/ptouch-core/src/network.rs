// SPDX-License-Identifier: GPL-3.0-or-later
// LPR and SNMP adapted from nhimc0n/ptouch-yuri, commit 10e14876 (see NOTICE).
//! Model-selected LPR printing with SNMP media and completion checks.
use crate::{
    CancellationToken, PrinterStatus, PtouchError, Result,
    printer_model::{NetworkDialect, PrinterModel},
    protocol::{self, PrintQuality},
    snmp::{SnmpValue, snmp_get_request, snmp_parse},
};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket},
    time::{Duration, Instant},
};

const STATUS: &str = "1.3.6.1.4.1.2435.3.3.9.1.6.1.0";
const STATE: &str = "1.3.6.1.2.1.25.3.5.1.1.1";
const ERRORS: &str = "1.3.6.1.2.1.25.3.5.1.2.1";
const DEVICE_ID: &str = "1.3.6.1.4.1.2699.1.2.1.2.1.1.3.1";
const IO_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintOutcome {
    Complete,
    SubmittedUnconfirmed,
}

/// Pure deadline policy, also exercised with a virtual clock in tests.
#[derive(Default)]
struct CompletionWatch {
    saw_printing: bool,
}
impl CompletionWatch {
    fn observe(
        &mut self,
        state: i64,
        elapsed: Duration,
        timeout: Duration,
    ) -> Option<PrintOutcome> {
        if elapsed >= timeout {
            return Some(PrintOutcome::SubmittedUnconfirmed);
        }
        if state == 4 {
            self.saw_printing = true;
        }
        if self.saw_printing && state == 3 {
            return Some(PrintOutcome::Complete);
        }
        if !self.saw_printing && elapsed >= Duration::from_secs(20) {
            return Some(PrintOutcome::SubmittedUnconfirmed);
        }
        None
    }
}

pub struct NetworkPrinter {
    pub model: PrinterModel,
    address: SocketAddr,
    status: PrinterStatus,
    timeout: Duration,
    cancellation: CancellationToken,
}

fn network_error(error: impl std::fmt::Display) -> PtouchError {
    PtouchError::StatusError(format!("Network: {error}"))
}

fn snmp_get(
    address: SocketAddr,
    oids: &[&str],
    timeout: Duration,
) -> Result<Vec<(String, SnmpValue)>> {
    let bind = if address.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind).map_err(network_error)?;
    socket.connect(address).map_err(network_error)?;
    socket
        .set_read_timeout(Some(timeout))
        .map_err(network_error)?;
    let id = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos()
        & 0x7fffffff) as i32;
    socket
        .send(&snmp_get_request("public", id, oids)?)
        .map_err(network_error)?;
    let mut buf = [0; 8192];
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        socket
            .set_read_timeout(Some(
                deadline
                    .saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(1)),
            ))
            .map_err(network_error)?;
        let n = socket.recv(&mut buf).map_err(|e| {
            if matches!(
                e.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) {
                network_error(format!(
                    "No SNMP response from {address}; check power, network, and SNMP access"
                ))
            } else {
                network_error(e)
            }
        })?;
        let (reply_id, values) = snmp_parse(&buf[..n])?;
        if reply_id == id {
            return Ok(values);
        }
    }
    Err(PtouchError::Timeout)
}

fn decode_state(values: &[(String, SnmpValue)]) -> Result<i64> {
    values
        .iter()
        .find_map(|(oid, v)| match v {
            SnmpValue::Int(n) if oid == STATE => Some(*n),
            _ => None,
        })
        .ok_or_else(|| network_error("printer state is unavailable"))
}

fn idle_status(address: SocketAddr) -> Result<PrinterStatus> {
    // Check the print server before requesting detailed engine status.
    if decode_state(&snmp_get(address, &[STATE], Duration::from_secs(2))?)? != 3 {
        return Err(network_error(
            "printer is busy or reports an error; detailed status is deferred",
        ));
    }
    let (status, _) = decode_status(&snmp_get(
        address,
        &[STATUS, STATE],
        Duration::from_secs(2),
    )?)?;
    Ok(status)
}

fn decode_status(values: &[(String, SnmpValue)]) -> Result<(PrinterStatus, i64)> {
    let status = values
        .iter()
        .find_map(|(oid, value)| match value {
            SnmpValue::Bytes(bytes)
                if oid == STATUS
                    && bytes.len() == 32
                    && bytes.starts_with(&[0x80, 0x20, 0x42, 0x30]) =>
            {
                PrinterStatus::from_bytes(bytes)
            }
            _ => None,
        })
        .ok_or_else(|| network_error("printer did not return a valid 32-byte media status"))?;
    let state = values
        .iter()
        .find_map(|(oid, v)| {
            if oid == STATE {
                if let SnmpValue::Int(n) = v {
                    Some(*n)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .ok_or_else(|| network_error("printer state is unavailable"))?;
    Ok((status, state))
}

impl NetworkPrinter {
    pub fn open(model: &str, host: &str, allow_experimental: bool) -> Result<Self> {
        let model = PrinterModel::find(model)
            .ok_or_else(|| network_error(format!("unknown model {model}")))?;
        if model.network.is_none() {
            return Err(PtouchError::UnsupportedOperation(
                "this model has no network backend",
            ));
        }
        if model.experimental && !allow_experimental {
            return Err(PtouchError::UnsupportedOperation(
                "experimental network model; enable allow_experimental or --allow-experimental",
            ));
        }
        if host.trim().is_empty() || host.contains('/') || host.chars().any(char::is_whitespace) {
            return Err(network_error(
                "use a hostname or IP address, without a URL or port",
            ));
        }
        let addresses: Vec<_> = (host.trim_matches(['[', ']']), 161)
            .to_socket_addrs()
            .map_err(network_error)?
            .collect();
        let mut last = PtouchError::DeviceNotFound;
        for address in addresses {
            match snmp_get(address, &[DEVICE_ID], Duration::from_secs(2)) {
                Ok(values) => {
                    let matching = values.iter().any(|(oid, v)| oid == DEVICE_ID && matches!(v, SnmpValue::Bytes(bytes) if String::from_utf8_lossy(bytes).split(';').any(|field| field == format!("MDL:{}", model.name))));
                    if !matching {
                        return Err(network_error(format!(
                            "device identity does not match {}",
                            model.name
                        )));
                    }
                    let status = idle_status(address)?;
                    return Ok(Self {
                        model,
                        address,
                        status,
                        timeout: Duration::from_secs(600),
                        cancellation: CancellationToken::default(),
                    });
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    pub fn status(&self) -> &PrinterStatus {
        &self.status
    }
    pub fn band(&self) -> Option<crate::tape::HeadBand> {
        self.model
            .network_band(self.status.media_type, self.status.media_width)
    }
    pub fn set_job_timeout(&mut self, timeout: Duration) -> Result<()> {
        if timeout.is_zero() {
            return Err(network_error("job timeout must be positive"));
        }
        self.timeout = timeout;
        Ok(())
    }
    pub fn set_cancellation_token(&mut self, token: CancellationToken) {
        self.cancellation = token;
    }

    /// Each call is an independent label. `chain` suppresses the final cut;
    /// it does not create a persistent connection or an automatic retry.
    pub fn print_raster(
        &mut self,
        lines: &[Vec<u8>],
        chain: bool,
        precut: bool,
        quality: PrintQuality,
    ) -> Result<PrintOutcome> {
        self.cancellation.check()?;
        let now = idle_status(self.address)?;
        if now.has_error() {
            return Err(PtouchError::StatusError(now.error_description()));
        }
        if (now.media_width, now.media_type) != (self.status.media_width, self.status.media_type) {
            return Err(network_error(
                "tape changed after rendering; refresh and render again",
            ));
        }
        self.model.validate_quality(quality, now.media_type)?;
        if self.band().is_none() {
            return Err(PtouchError::UnknownTapeWidth(now.media_width));
        }
        if chain && self.model.experimental {
            return Err(PtouchError::UnsupportedOperation(
                "no-cut jobs are not verified for this experimental model",
            ));
        }
        let data = build_job(self.model, &now, lines, !chain, precut, quality)?;
        let started = Instant::now();
        let mut address = self.address;
        address.set_port(515);
        lpr_submit(address, &data, started + self.timeout, &self.cancellation)?;
        // A print-server ACK proves submission only. A busy -> idle transition
        // after our idle preflight is required before reporting completion.
        let mut completion = CompletionWatch::default();
        loop {
            self.cancellation.check()?;
            let remaining = self.timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Ok(PrintOutcome::SubmittedUnconfirmed);
            }
            // The vendor status OID can ask the print engine for a raster
            // status packet. P750W reports a communication error if that
            // happens while printing. Poll only the print-server activity OID.
            match snmp_get(
                self.address,
                &[STATE, ERRORS],
                remaining.min(Duration::from_secs(2)),
            ) {
                Ok(values) => {
                    if let Some(flags) = values.iter().find_map(|(oid, value)| match value {
                        SnmpValue::Bytes(flags) if oid == ERRORS => Some(flags),
                        _ => None,
                    }) && flags.iter().any(|&b| b != 0)
                    {
                        return Err(PtouchError::StatusError(format!(
                            "Job submitted; printer reports fault flags {flags:02x?}. Check the printer before retrying"
                        )));
                    }
                    let Ok(state) = decode_state(&values) else {
                        return Ok(PrintOutcome::SubmittedUnconfirmed);
                    };
                    if let Some(outcome) =
                        completion.observe(state, started.elapsed(), self.timeout)
                    {
                        return Ok(outcome);
                    }
                }
                Err(_) => return Ok(PrintOutcome::SubmittedUnconfirmed),
            }
            std::thread::sleep(Duration::from_millis(200).min(remaining));
        }
    }
}

/// Input is already at the selected feed DPI. No line duplication here.
fn build_job(
    model: PrinterModel,
    status: &PrinterStatus,
    lines: &[Vec<u8>],
    final_cut: bool,
    precut: bool,
    quality: PrintQuality,
) -> Result<Vec<u8>> {
    model.validate_quality(quality, status.media_type)?;
    if lines.is_empty()
        || lines
            .iter()
            .any(|l| l.len() != usize::from(model.raster_pins).div_ceil(8))
    {
        return Err(network_error("invalid raster dimensions"));
    }
    let count = u32::try_from(lines.len()).map_err(network_error)?;
    let mut data = if model.raster_pins == 560 {
        protocol::cmd_init_p900()
    } else {
        protocol::cmd_init()
    };
    data.extend([0x1b, 0x69, 0x61, 1]);
    let media = protocol::info_media_type(status.media_type)
        .ok_or_else(|| network_error("unsupported medium"))?;
    let mut info = protocol::cmd_info_p900(media, status.media_width, count, true, false, false);
    if model.network == Some(NetworkDialect::P750) {
        info[11] = 0;
    }
    data.extend(info);
    data.extend(protocol::cmd_precut(final_cut));
    data.extend(protocol::cmd_cut_every(1));
    data.extend(protocol::cmd_advanced_mode(
        quality, false, final_cut, false,
    ));
    if model.network == Some(NetworkDialect::E850) {
        data.extend(protocol::cmd_esc_i_k_lower());
    }
    let margin_mm = if precut { 2.0 } else { 24.3 };
    let margin = (f64::from(model.feed_dpi(quality)) * margin_mm / 25.4).round() as u16;
    data.extend(protocol::cmd_page_flags(margin));
    data.extend(protocol::cmd_enable_packbits());
    for line in lines {
        data.extend(protocol::cmd_send_raster_packbits(line));
    }
    data.push(0x1a);
    Ok(data)
}

fn lpr_submit(
    address: SocketAddr,
    data: &[u8],
    deadline: Instant,
    cancellation: &CancellationToken,
) -> Result<()> {
    let mut stream = TcpStream::connect_timeout(
        &address,
        IO_TIMEOUT.min(deadline.saturating_duration_since(Instant::now())),
    )
    .map_err(network_error)?;
    let mut sent_data = false;
    let result = (|| -> std::io::Result<()> {
        let mut transfer = |bytes: &[u8], ack: bool| -> std::io::Result<()> {
            if cancellation.is_cancelled() {
                return Err(std::io::Error::other("cancelled"));
            }
            let remaining = || -> std::io::Result<Duration> {
                if cancellation.is_cancelled() {
                    return Err(std::io::Error::other("cancelled"));
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "job deadline",
                    ));
                }
                Ok(left.min(IO_TIMEOUT))
            };
            let mut written = 0;
            while written < bytes.len() {
                stream.set_write_timeout(Some(remaining()?))?;
                let n = stream.write(&bytes[written..])?;
                if n == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "LPR write stopped",
                    ));
                }
                written += n;
            }
            stream.set_read_timeout(Some(remaining()?))?;
            if ack {
                let mut reply = [0];
                stream.read_exact(&mut reply)?;
                if reply[0] != 0 {
                    return Err(std::io::Error::other(format!(
                        "LPR rejected command ({})",
                        reply[0]
                    )));
                }
            }
            Ok(())
        };
        let job = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
            % 1000;
        let file = format!("dfA{job:03}ptouch");
        let control = format!("Hptouch\nPptouch\nJLabel\nl{file}\nU{file}\nNLabel\n");
        transfer(b"\x02BINARY_P1\n", true)?;
        transfer(
            format!("\x02{} cfA{job:03}ptouch\n", control.len()).as_bytes(),
            true,
        )?;
        transfer(control.as_bytes(), false)?;
        transfer(&[0], true)?;
        transfer(format!("\x03{} {file}\n", data.len()).as_bytes(), true)?;
        sent_data = true;
        for chunk in data.chunks(16384) {
            transfer(chunk, false)?;
        }
        transfer(&[0], true)
    })();
    result.map_err(|e| if sent_data { PtouchError::SendFailed(format!("Submission uncertain: {e}. Check the printer before retrying; data may have printed")) } else { network_error(e) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;

    #[test]
    fn virtual_clock_allows_long_printing_but_never_assumes_completion() {
        let timeout = Duration::from_secs(600);
        let mut watch = CompletionWatch::default();
        assert_eq!(watch.observe(3, Duration::ZERO, timeout), None);
        assert_eq!(watch.observe(4, Duration::from_secs(1), timeout), None);
        for second in [30, 60, 120, 500] {
            assert_eq!(watch.observe(4, Duration::from_secs(second), timeout), None);
        }
        assert_eq!(
            watch.observe(3, Duration::from_secs(501), timeout),
            Some(PrintOutcome::Complete)
        );
        assert_eq!(
            watch.observe(4, timeout, timeout),
            Some(PrintOutcome::SubmittedUnconfirmed)
        );
        let mut unseen = CompletionWatch::default();
        assert_eq!(
            unseen.observe(3, Duration::from_secs(20), timeout),
            Some(PrintOutcome::SubmittedUnconfirmed)
        );
    }

    fn lpr_server(reject_data: bool) -> (SocketAddr, std::thread::JoinHandle<Vec<u8>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = std::io::BufReader::new(socket);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "\x02BINARY_P1\n");
            reader.get_mut().write_all(&[0]).unwrap();
            let mut data = Vec::new();
            for command in [2u8, 3u8] {
                line.clear();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line.as_bytes()[0], command);
                let len: usize = line[1..]
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap();
                reader.get_mut().write_all(&[0]).unwrap();
                let mut bytes = vec![0; len + 1];
                reader.read_exact(&mut bytes).unwrap();
                assert_eq!(bytes.pop(), Some(0));
                if command == 3 {
                    data = bytes;
                }
                reader
                    .get_mut()
                    .write_all(&[if reject_data && command == 3 { 1 } else { 0 }])
                    .unwrap();
            }
            data
        });
        (addr, worker)
    }

    #[test]
    fn lpr_acknowledgements_and_partial_submission_are_distinct() {
        for reject in [false, true] {
            let (addr, server) = lpr_server(reject);
            let result = lpr_submit(
                addr,
                b"raster bytes",
                Instant::now() + Duration::from_secs(3),
                &CancellationToken::default(),
            );
            if reject {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("Submission uncertain")
                );
            } else {
                result.unwrap();
            }
            assert_eq!(server.join().unwrap(), b"raster bytes");
        }
    }
    #[test]
    fn model_commands_and_real_high_resolution() {
        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&[0x80, 0x20, 0x42, 0x30]);
        bytes[10] = 12;
        bytes[11] = 1;
        let status = PrinterStatus::from_bytes(&bytes).unwrap();
        let model = PrinterModel::find("PT-P750W").unwrap();
        let data = build_job(
            model,
            &status,
            &[vec![0xff; 16]; 1],
            true,
            true,
            PrintQuality::HighRes,
        )
        .unwrap();
        assert!(data.windows(4).any(|w| w == [0x1b, 0x69, 0x4b, 0x48]));
        assert_eq!(
            data.windows(4).filter(|w| *w == [0x47, 17, 0, 15]).count(),
            1
        );
        assert!(model.validate_quality(PrintQuality::HighRes, 3).is_err());
        assert!(model.validate_quality(PrintQuality::Draft, 1).is_err());
        assert!(PrinterModel::find("PT-P950NW").unwrap().experimental);
    }
    #[test]
    fn status_requires_valid_frame() {
        assert!(
            decode_status(&[
                (STATUS.into(), SnmpValue::Bytes(vec![0; 32])),
                (STATE.into(), SnmpValue::Int(3))
            ])
            .is_err()
        );
    }
}
