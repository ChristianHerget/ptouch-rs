# Printer profiles and network printing

## Two kinds of profiles

Built-in model profiles describe capabilities: command dialect, head and feed
resolution, raster width, media geometry, supported connections, and which
features are experimental. They live in `ptouch-core/src/printer_model.rs`.
Only built-in code can select protocol commands.

User profiles describe particular printers: alias, built-in model, connection,
preferred quality, final cut, and experimental opt-in. The standalone
`ptouch-profiles` crate stores these files and has no dependency on the driver.

## One directory, any number of files

Every immediate `*.toml` file in the selected directory belongs to one
collection. A file can contain one printer, several printers, or just the
default declaration. Subdirectories are not scanned. Files are sorted for
stable diagnostics; file order never creates override precedence.

```toml
# office.toml
version = 1

[[printers]]
alias = "desk"
model = "PT-P750W"
connection = "network"
host = "labelprinter.local" # IPv4, IPv6, or hostname; no URL or port
quality = "high"           # standard, high, or draft, if supported
final_cut = true
allow_experimental = false

[[printers]]
alias = "bench"
model = "PT-P750W"
connection = "usb"
serial = "YOUR-USB-SERIAL" # optional; omit if exactly one model matches
quality = "standard"
final_cut = true
```

```toml
# default.toml
version = 1
default_printer = "desk"
```

Only one file may declare the default. Aliases must be unique across the whole
collection and use ASCII letters, digits, underscores, or hyphens. Duplicate
aliases, unknown fields, invalid versions, dangling defaults, and conflicting
connection fields cause an error. Files use the same versioned schema.

Copy the directory's TOML files to another machine to move the profiles.
Addresses and serial numbers are portable identities; USB bus/address numbers
are not saved. A hostname or IP must still be reachable from the new machine.
The `.profiles.lock` file is local coordination state and need not be copied.
Layouts remain separate self-contained `.ptl` files.

The GUI lists only the connections implemented for the selected model. For
example, PT-P700 offers USB only. Its editable USB serial dropdown lists detected
serials for that model and also accepts a manually entered serial. **Automatic**
requires exactly one matching physical printer. Refreshing the list does not
erase a saved or manually entered serial when the printer is disconnected.

On Windows, PT-P710BT profiles can use the existing USBPRINT driver. The serial
list uses the physical PnP inventory, avoiding duplicate libusb/USBPRINT entries.
Only an instance ID marked unique by the USB bus supplies a serial; generated
port-dependent IDs are never saved as serials. Windows compares serials without
case sensitivity because PnP can normalize their case.

**Reload files** reconnects the active saved alias using its current address,
quality, and final-cut preference. A removed or invalid profile disconnects it.
Temporary connections remain independent of the saved collection. Choosing a
different directory selects that directory's default, or disconnects when there
is no valid default. Loading a layout retains the connected printer's tape
geometry and refreshes the preview before printing.

## Hostnames, IPv4, and IPv6

The host field accepts hostnames, IPv4, and either bare or bracketed IPv6:
`printer.local`, `192.0.2.10`, `2001:db8::10`, or `[2001:db8::10]`. URLs and
addresses with an explicit port are rejected; the protocols supply their ports.
Link-local IPv6 requires a scope, such as `fe80::1%eth0` or `[fe80::1%3]`.
The interface name or index belongs to the local computer and may need changing
when a profile is copied to another machine.

Address parsing uses Rust's `IpAddr`; hostname and named-scope lookup use
`ToSocketAddrs` and the operating system resolver. Preflight tries addresses in
the returned order when an address is unreachable. A responding printer with
the wrong model stops selection. Once preflight identifies the printer, the
job and status requests keep that address, including its IPv6 scope. Submission
never switches addresses or retries a job.

`ptouch printers add ... --file office.toml` adds to a group file; omitting
`--file` creates `<alias>.toml`. The GUI preserves each entry's source file when
editing or removing it. Writes use a temporary file and atomic replacement,
with a lock for application writers and snapshot checks for external edits.
Clear the default before renaming or removing its printer. `printers default`
without an alias clears the default.

## Implemented capabilities

| Model | USB | Network | Quality exposed by this application |
| --- | --- | --- | --- |
| PT-P750W | Existing backend, plus high resolution | LPR + SNMP, tested on hardware | 180×180; 180×360 on laminated tape |
| PT-9500PC, PT-9700PC | Existing backend | Not enabled | 360×360; 360×720 high resolution and 360×180 draft on HG tape |
| PT-E550W | Existing unsupported flag retained | Experimental LPR + SNMP | 180×180 initially |
| PT-P900W, PT-P950NW | Not added in this change | Experimental LPR + SNMP | 360×360 initially |
| PT-E850TKW | Not added in this change | Experimental LPR + SNMP; E850-specific command | 360×360 initially |
| Other existing USB models | Existing support retained | Not enabled by this change | Existing capabilities |

Enable experimental network models using `--allow-experimental` or the
profile's `allow_experimental = true`. This is not a claim of hardware
verification. They require the model identity and 32-byte media status to be
available through SNMP. If firmware does not supply those values, printing
fails before sending a job; the app does not guess tape type from an HTML page.

Network jobs use TCP 515, queue `BINARY_P1`. Status uses SNMPv1 over UDP 161
with the read community `public`. Raw TCP 9100, custom queues/communities,
discovery, network provisioning, and printer firmware updates are outside
this implementation. There is no automatic switch to another transport.

PT-P750W network geometry follows Brother's raster reference (12 mm tape has
70 printable dots in a 128-dot transfer line). Legacy USB tape geometry is
retained. The 560-pin profiles use the imported model-specific media bands;
unsupported tape types and widths are rejected.

Quality labels show head × feed DPI. High resolution keeps the head resolution
fixed and doubles feed resolution: 180×360 on PT-P750W, and 360×720 on PT-9500PC
and PT-9700PC. The apps render source content with extra detail while preserving
physical dimensions. Draft reduces feed resolution in the renderer. The protocol
sends each supplied raster column once in every mode. HG tape requirements on
legacy models are enforced by the printer's existing quality commands.
PT-P750W does not expose draft; PT-P900W/PT-P950NW remain standard-only.

GUI previews and image exports retain their square document resolution. Saved
profile and layout formats and CLI quality names are unchanged. Library callers
must adopt the explicit raster resolution argument; see the
[Rust raster API migration](raster-api.md).

A plain CLI image export without `--tape-width` uses the selected/default
printer's geometry. Forced-width exports, layout exports, and `--list-vars` work
offline without loading the default profile. An explicit `--printer` still
resolves that alias for exports; `--list-vars` needs only the layout.

## Cutting and batches

`final_cut` is a preference for the end of a job. Precut, number of copies,
CSV chaining, and job timeout remain job settings.

Network jobs default to short 2 mm margins, which can produce a leading offcut.
`--no-precut` requests model-specific margins to avoid that cut, using more tape
on short labels: PT-P750W uses 172 standard or 344 high-resolution feed dots
(about 24.3 mm), and PT-P900W/PT-P950NW use 382 dots (about 27 mm).
PT-E850TKW provisionally uses the latter margin and remains experimental.
`--precut` explicitly selects short margins. The GUI calls
this **Short margins (leading cut)** and enables it by default. The printer's
cutter position also limits minimum physical label length.

Network copies and CSV rows are separate jobs with separate final cuts unless
`--chain` suppresses them. Experimental models reject no-cut jobs. USB retains
its existing chaining defaults; `--cut-each` cuts every copy or row separately.
Standalone **Feed & Cut** is available only through the existing USB backend.

## Completion and timeouts

LPR acknowledgement means the print server accepted bytes. It does not mean
the label printed. The app requires an observed printing-to-idle transition
after an idle preflight to report completion. A very short job can be missed
between polls; that produces **Submitted, completion unconfirmed**.

Detailed vendor media status is requested only when the printer is idle.
Querying it while printing caused a transient communication error on the
PT-P750W even though the label printed. Progress polling uses general
print-server activity and fault status instead.

The default job deadline is 600 seconds, adjustable with `--job-timeout` or
**Job timeout (s)** in the GUI. USB models without a readiness handshake use
this completion deadline; models with a readiness handshake also get a silence
allowance based on rendered length. Network busy states may continue up to the
absolute deadline. Silence, acknowledgement, and an idle printer that was never
observed printing are not treated as proof of completion.

If completion is unconfirmed, the CLI exits with code 2 and stops the rest of
the batch. Failures after raster transmission warn that submission is uncertain.
Neither app resends a job, performs a cleanup feed/cut, nor falls back to USB.
Cancellation stops further host work but cannot recall bytes already accepted.

## Evidence and verification

PT-P750W network status was checked against the attached printer's 12 mm
laminated white tape with black text. Standard and high-resolution labels
printed; the user confirmed finer detail in the 360-dpi label. The corrected
activity polling reported completion for both modes. One longer label also
completed. Further timing regression tests use a virtual clock to conserve tape.

Automated tests cover split and grouped files, copying collections, comment
preservation, stale edits, conflicting aliases/defaults, invalid schemas,
SNMP parsing, LPR acknowledgement failures, raster offsets, real high-resolution
rendering, and completion deadlines with a virtual clock. Use mocks for long
jobs and repeated cutter cycles to avoid wasting tape.

References:

- [Brother PT-E550W/PT-P750W/PT-P710BT raster reference v1.02](https://download.brother.com/welcome/docp100064/cv_pte550wp750wp710bt_eng_raster_102.pdf)
- [Brother PT-P900/P950NW raster reference v1.02](https://download.brother.com/welcome/docp100407/cv_ptp900_eng_raster_102.pdf)
- [Network implementation adapted from ptouch-yuri](https://github.com/nhimc0n/ptouch-yuri/tree/10e14876c35ef0e09889657d53bd8a9c34cd3808)

Adapted protocol code remains GPL-3.0-or-later. Independent profile storage and
UI additions are MIT. See the repository's LICENSE, LICENSE-MIT, and NOTICE.
