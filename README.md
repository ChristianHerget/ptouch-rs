# ptouch-rs

Rust tool for Brother P-Touch USB and network label printers. CLI and GUI.
Optional native macOS Bluetooth support is available in `ptouch-core` for the PT-P300BT.

![P-Touch free layout editor](https://github.com/user-attachments/assets/ce630fd8-c612-4a78-bcd1-e9fef5fbf991)

## Features

- Print text labels with custom font, size, alignment and rotation
- Print images (PNG, JPEG, GIF, BMP, TIFF, WebP, SVG, and more)
- Compose multi-element labels (text + image + cut mark + padding)
- Position fixed text boxes in millimeters, with absolute coordinates or gaps
  after the previous box; preview with a millimeter ruler
- Save and reload designs as self-contained `.ptl` layout files (images
  embedded), then print them from the GUI or CLI
- Template layouts with `{{name}}` placeholders and batch-print from a CSV
- Portable printer profiles in one directory, split across any number of TOML files
- Network printing with model-specific capabilities and media checks
- PT-P750W standard 180×180 and high resolution 180×360 modes
- Chain print and multi-copy support
- PT-9500PC/PT-9700PC quality modes (360×720 high resolution, 360×180 draft)
- GUI with live preview, zoom, and drag-and-drop element reordering
- Export to image (PNG, JPEG, BMP, GIF, TIFF, WebP) without a printer connected
- Feed and cut tape without printing

## Supported Printers

PT-9200DX, PT-2300, PT-2420PC, PT-9500PC, PT-9700PC, PT-2450PC, PT-18R,
PT-1950, PT-2700, PT-1230PC, PT-2430PC, PT-2730, PT-H500, PT-E500, PT-E550W,
PT-P700, PT-P750W, PT-D410, PT-D450, PT-D460BT, PT-D600, PT-D610BT,
PT-P710BT, PT-E310BT, PT-E560BT and more.

Tape widths: 3.5mm, 6mm, 9mm, 12mm, 18mm, 24mm, 36mm.

## Network printing and portable printer profiles

Both apps use the same profile directory. On Linux the default is
`~/.config/ptouch-rs/printers` (under `$XDG_CONFIG_HOME` when set).
On macOS and Windows it is under the platform's user configuration directory.
Pass `--profiles-dir DIRECTORY` to either app to use a portable folder.

```sh
# Direct connection: no saved profile needed
ptouch info --model PT-P750W --host 192.168.20.150
ptouch print --model PT-P750W --host 192.168.20.150 --quality high "Label"

# Save the connection and choose it by alias
ptouch printers add desk --model PT-P750W --host labelprinter.local
ptouch printers default desk
ptouch info --printer desk
ptouch print --printer desk --quality high "Label"
ptouch printers list
ptouch printers validate
ptouch gui
```

Explicit connection flags override the saved default. Explicit print quality
and `--final-cut`/`--chain` override profile preferences. Without a default or
explicit target, USB automatic selection still works. `--usb BUS:ADDRESS` is
available for a one-time USB selection; portable USB profiles use a model and
an optional serial number. Multiple matching USB devices are rejected.

In the GUI, use **Manage printers…** to add/edit connections, choose the
profile directory, set a default, or connect without saving. Saves update the
printer's original file, preserving comments and other entries. Reload after
external edits; stale edits are refused.

See [printer profiles and network behavior](docs/printer-profiles.md) for the
schema, capability matrix, cut options, completion semantics, and limitations.

## Building

Requires Rust stable toolchain.

**Linux** (libusb + udev):

```sh
sudo apt install libusb-1.0-0-dev libudev-dev   # Debian/Ubuntu
sudo pacman -S libusb                            # Arch
sudo emerge dev-libs/libusb                      # Gentoo
cargo build --release --workspace
```

**macOS**: install the Xcode command-line tools.

**Windows**: install the MSVC C++ build tools and Windows SDK. ARM64 builds
require the ARM64 C++ tools. No separate libusb installation is needed.

```sh
cargo build --release --workspace
```

Binaries: `target/release/ptouch` (CLI), `target/release/ptouch-gui` (GUI).

libusb is compiled in statically (`rusb` vendored), so the binaries carry no
external libusb dependency.

**Nix** (flake at the repository root):

```sh
nix build          # ptouch-gui; `nix run .#ptouch` runs the CLI
nix develop        # dev shell
nix flake check    # cargo fmt, clippy and the workspace tests
```

## PT-P300BT Bluetooth (macOS)

The macOS CLI can use the native RFCOMM backend for an already-paired PT-P300BT.
Pair the printer in macOS Bluetooth settings and grant Bluetooth access to the
terminal. Without `--bluetooth`, the CLI continues to select USB printers.

```sh
CARGO_HOME=/tmp/ptouch-bt-cargo cargo run -p ptouch-cli -- bluetooth-list
CARGO_HOME=/tmp/ptouch-bt-cargo cargo run -p ptouch-cli -- info --bluetooth AA:BB:CC:DD:EE:FF
CARGO_HOME=/tmp/ptouch-bt-cargo cargo run -p ptouch-cli -- print --bluetooth AA:BB:CC:DD:EE:FF "Hello"
```

Replace the address with your paired printer's address. Text, images, saved
layouts, CSV batches, and multiple copies use the normal CLI rendering flow.
PT-P300BT rejects `--chain`, `--precut`, and non-standard quality modes before
connecting. Cargo artifacts stay in the checkout and dependencies in the
specified temporary cache. No Python packages or Bluetooth serial device nodes
are required.

The first profile supports the physically verified 12mm tape, with 64 printable
dots centered in 128-dot raster transfer lines at 180 dpi. Other widths and marks
outside that area are rejected. Printing waits for the printer's completion
notification, checks errors, and never automatically retries a failed job.
The PT-P300BT has a manual cutter.

The GUI on macOS also lists paired PT-P300BT printers in its **Connection**
selector. Select the printer and click **Refresh** to query its tape, then compose
and print through the normal preview workflow. Bluetooth operations run outside
the interface process so connecting and printing do not block window updates.

Library users can also open `ptouch_core::BluetoothDevice`, call `init`, prepare
16-byte raster lines using bottom-to-top dot order, then call
`print_raster(&lines, PrintResolution::square(180))` and
`close`. Native objects stay on the main thread and cannot be sent or shared
across threads. These synchronous session calls are suitable for the example;
the GUI runs these calls in a separate process. Close assumes exclusive
ownership and also disconnects the printer's
baseband connection. Apple's baseband close is synchronous. If an async write
never completes, one transfer buffer is conservatively retained to avoid freeing
memory still potentially used by Bluetooth; this only occurs on a failed
connection, which is then closed.

All Rust raster APIs now take explicit input DPI. See the
[raster API migration](docs/raster-api.md) for mode metadata and rendering at
the selected head × feed resolution.

## Prebuilt Packages

The release workflow publishes downloads on the
[releases page](https://github.com/vowstar/ptouch-rs/releases):

- **Linux amd64 / arm64**: `.deb` and `.rpm` that install the CLI, the GUI, the desktop entry,
  the icon, and the udev rule (the post-install step reloads udev), plus the raw
  binaries.
- **macOS**: `ptouch-gui-macos-arm64.app.zip` (a `.app` bundle with the icon)
  and the raw `ptouch` CLI binary. The app is unsigned, so on first launch
  right-click the app and choose Open, or run
  `xattr -dr com.apple.quarantine ptouch-gui.app`.
- **Windows amd64 / arm64**: `ptouch-windows-{arch}.exe` and
  `ptouch-gui-windows-{arch}.exe` (the icon is embedded).
- **Integrity**: `SHA256SUMS` covers every binary and package.

Windows and Linux ARM64 assets are available from v0.8.4. macOS ARM64 assets
already exist in v0.8.3. Check the selected release's asset list.

The GUI requires OpenGL 2.0 or later. Windows CI uses a pinned Mesa software
renderer because hosted machines lack a suitable graphics driver. That CI-only
DLL is not shipped. Native CI rendering does not validate a physical host's GPU driver.

| Platform | Rust target | Native CI runner | Hardware validation |
| --- | --- | --- | --- |
| Linux x64 | `x86_64-unknown-linux-gnu` | `ubuntu-latest` | Separate from CI |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | PT-P710BT acceptance pending |
| Windows x64 | `x86_64-pc-windows-msvc` | `windows-latest` | Separate from CI |
| Windows ARM64 | `aarch64-pc-windows-msvc` | `windows-11-arm` | PT-P710BT single-label printing confirmed |
| macOS ARM64 | `aarch64-apple-darwin` | `macos-latest` | PT-P300BT Bluetooth verified; USB tested separately |

CI builds and tests each target natively, validates binary architecture, and
runs CLI startup and GUI rendering checks. These checks do not exercise a printer.
The Linux ARM64 packages use Debian `arm64` and RPM `aarch64` metadata.
Nix checks cover both Linux architectures.

`ptouch-gui --smoke-test` renders seven frames across legacy and free layouts,
including the symbol picker, then exits without starting the printer worker.
This mode requires a working display and OpenGL context.

## GUI

```sh
ptouch-gui
```

Use `ptouch-gui --offline` to edit and export without opening or polling printer
connections. Combine it with `--profiles-dir DIRECTORY` to select a profile collection.
The offline preview uses the layout's saved design DPI.

- Label preview with Fit, approximate Actual size, and sharp Pixels presets
- Millimeter rulers and Object/Frame editing for free layouts
- Drag objects or frames to move them; drag list handles to reorder
- Legacy labels retain canvas drag-to-reorder and dimension guides
- Duplicate or delete objects through row action menus or right-click menus
- Undo/redo in the top toolbar for content, formatting, object transforms, and explicit layout conversion
- Add Text, Add Image, Add Symbol, and Cut Mark directly below the Objects list
- Pan free layouts with Space+drag, middle-drag, or scroll; legacy labels also allow dragging empty canvas
- Add/edit text, images, cut marks, and legacy padding
- Free-angle text rotation with auto font sizing
- Mirror the whole label or a single element (horizontal/vertical)
- Tape width selection
- Save/Open layout (`.ptl`) with images embedded for portability
- Print to connected printer or export to image file
- Feed and cut tape without printing

With the preview or element list focused, use Ctrl/Cmd+D to duplicate, Delete
or Backspace to delete, and Alt+Up/Down to reorder. Undo with Ctrl/Cmd+Z and redo
with Ctrl/Cmd+Shift+Z (also Ctrl+Y on Windows/Linux). History retains up to 100
edits for the current layout and resets when another layout is opened. Printer
and tape settings remain outside content history. Saved design DPI is undoable.

### Symbols

Use **Add Symbol** to browse 7,188 black-and-white symbols from
[Material Design Icons 7.4.47](https://pictogrammers.com/library/mdi/). Choose a
category or search by name, alias, or category. Select a symbol to inspect its
print preview, then press **Add**. Each symbol is a separate object with the
same frame, movement, sizing, rotation, and mirroring controls as an image.

Use **Change Symbol…** in image or symbol properties to replace the artwork
while retaining its frame, content dimensions and offsets, rotation, and
mirroring. Add and Replace support undo/redo. Cancel and Escape leave the
label unchanged.

The catalogue is bundled with the GUI and works offline. Saved `.ptl` files
embed the SVG artwork, name, author, source, and license; reopening, exporting,
and printing require neither the catalogue nor a symbol font. The layout
format stays unchanged. Symbol rendering preserves black shapes, including
dense filled artwork, without automatic inversion or dithering.

The catalogue excludes deprecated icons and brand/logo artwork. Its 15 label
categories combine upstream tags with supplemental rules for common technical
symbols; unmatched icons appear in **Other**. Regenerate the pinned catalogue
with `python3 scripts/import-symbols.py`, or verify it without writing files
with `python3 scripts/import-symbols.py --check`. The importer verifies the
archive and license checksums. Ordinary builds do not run it or download assets.

## CLI Usage

```sh
# Print text
ptouch print "Hello World"

# Multi-line text
ptouch print "Line 1" "Line 2"

# Print with options
ptouch print "Label" -f "DejaVu Sans" -s 32 -a center

# Print image (PNG, JPEG, BMP, SVG, etc.)
ptouch print -i logo.png

# Text + image + cut mark
ptouch print "Name" -i photo.png -c

# Mirror the whole label left-right (e.g. clear tape read from the back)
ptouch print "MIRROR" --flip-h

# Export to image file (no printer needed, format from extension)
ptouch print "Preview" -o label.png -w 76
ptouch print "Preview" -o label.bmp -w 76

# Print a layout designed in the GUI (images are embedded in the file)
ptouch print --layout label.ptl

# Render a layout to an image without a printer (uses the saved tape width)
ptouch print --layout label.ptl -o label.png

# Show printer info
ptouch info

# List supported models
ptouch list
```

### Free layout and precise patch-panel labels

New GUI layouts give each text, image, or cut mark its own millimeter **frame**
(the slot in the file format). A frame belongs to exactly one object. The frame controls
template position and spacing. An independent **object** box controls content
size and center offsets. In **Object** mode, dragging moves the content within
or beyond its frame. Its square handle scales proportionally, including an
explicit text font size; its round handle rotates the object and its handles
without shrinking it back into the frame. **Frame** mode moves or resizes the
template frame without scaling a manually sized object. Hover an object to
preview its selection; elsewhere on the paper, its frame's X interval selects
the frame. Overlapping frames prefer the nearest Y range, then list order.
Only hovered/selected outlines appear, with dashed related frames shown faintly.
Click selects the target; dragging keeps that target until release. Selection
handles stay visible while hovering another element.

Click a row in **Objects** to select its object. Use the **Object / Frame**
switch at the top of properties, or the context menu, to change editing targets.
A selected dashed border also acts as a move handle when another object covers it. Moving a frame horizontally or changing its width updates the
X positions of consecutive **After previous** objects; an absolute frame ends
that chain. The bottom hint identifies the target and affected follower count.
Hold Shift while rotating to snap to 15° increments. The optional 0.1 mm snap and arrow keys
allow fine position adjustments (Shift moves 1 mm).
Turn off whole-label horizontal mirroring before using mouse transforms or
arrow-key movement. Numeric properties remain available; each object's own
mirroring and whole-label vertical mirroring also allow mouse transforms. This
avoids an ambiguous origin when the horizontally mirrored label changes length.
The X and Y millimeter rulers remain visible while scrolling. Long previews
use tiled textures, preserving every printer dot and its selection coordinates.
The preview shows the full tape with shaded unprintable margins. The Y ruler
starts at the printable top and ends at the actual printable height; a separate
dimension shows the nominal tape width. The Tape section reports printable
height, dot count, DPI, and whether the preview uses a printer or design settings.

New text starts in a 10 mm wide frame with automatic font size and the full
printable height. **Fill printable height** sets an existing frame to Y=0 and
the printable height as one undoable edit. Auto-fit content follows the frame;
manually sized content keeps its size and offsets from the frame center. Saved
frames keep their dimensions when the tape or printer changes.
The first object starts at X=0, Y=0. Subsequent frames start 3 mm after the rightmost frame, using absolute
coordinates. Images keep their aspect ratio inside their object boxes.

Use **Add Image** or **Change File...** to add or replace PNG, SVG, or SVGZ images.
The source bytes are embedded in the saved `.ptl`, so reopening does not require
the original image file. SVG sources should embed any linked images as well.

Choose **After previous** to position a frame after the previous frame's right
edge with a specified gap. Empty text still reserves the whole frame, so template
values do not shift later ports. Dragging a relative frame changes its gap; if
it crosses to the left of its predecessor's right edge, it becomes absolute to
allow overlap. Dragging list handles or using Alt+Up/Down changes composition order:
absolute coordinates stay fixed, while relative objects follow their new
predecessors. Black pixels combine at overlaps; white pixels do not erase other
objects. The list order also determines which overlapping object is selected.

Object width and height describe the unrotated content box; offsets move its
center relative to the frame center. Auto font size fits that box without
stretching letters; an explicit size uses points and clipped text raises a
warning. Rotation keeps the object size and frame spacing. Content can overlap
neighboring frames or extend the label length; the printable tape edges clip it
with a warning. **Auto fit frame** restores the older fit-inside-frame behavior.

The [patch panel example](examples/patch-panel.ptl) has three 10 mm boxes starting
at 5, 18, and 31 mm, with 3 mm gaps:

```sh
# Offline preview at the saved design DPI; an empty second port retains its box
ptouch print --layout examples/patch-panel.ptl \
  --set port1=LAN1 --set port2= --set port3=UPLINK -o patch-panel.png

# Print the complete layout as one label, with no cuts between ports
ptouch print --layout examples/patch-panel.ptl \
  --set port1=LAN1 --set port2=LAN2 --set port3=UPLINK
```

Coordinates start at the **printable area's top-left**, not the physical tape
edge or cut. Printer feed and cutter offsets can add leading tape; verify those
on the device before using the first coordinate as a cut-edge measurement.
Each frame sets the next relative start to its own right edge, even if it was
moved backwards with absolute positioning. Negative/non-finite frame coordinates,
zero or excessive dimensions, and frames beyond the printable tape height are
errors; object center offsets may be negative. Bounds are rounded from absolute
physical positions, so repeated ports do not accumulate pixel-rounding drift.

New free layouts use format version 4, which older builds reject. Versions 1,
2, and 3 retain their original rendering and version when opened and saved
unchanged. Objects without a `content_box` keep fitting inside their frames;
their first Object move, resize, or rotation fixes the current content size and
saves it as v4. Content previously clipped by the frame may reappear on that
first edit. **Convert to free layout** preserves the exact dimensions of
existing millimeter frames and resolves their positions without rounding
through preview pixels. Legacy natural dimensions are frozen into physical
frames at the selected preview DPI.
Review the result before saving: legacy automatic text sizing and rotated
images may fit differently in fixed frames. Conversion keeps Padding as an
empty physical frame, preserving its occupied space. On labels with existing
content, zero-width padding and empty natural text receive selectable blank
frames inside the original label length; an entirely empty label receives a
default frame. New free layouts use coordinates/gaps and do not offer a Padding
insertion button; legacy layouts retain it.

Legacy unboxed text, padding, image sizes, and font margins keep their existing
pixel units. Fixed text frame font sizes use points (1/72 inch).
Offline CLI layout export uses the saved `dpi`; printing uses the selected
head/feed resolution to preserve millimeter positions. Connected GUI previews
and image exports use the selected quality; exports use square pixels while
preserving physical proportions. The GUI saves a separate design DPI and shows the current
preview DPI. A printer disconnect keeps the last printer preview until **Use
design DPI** is selected. `--tape-width` changes the exported printable height in
pixels, not the layout DPI or its millimeter coordinates.

### Layout templates and batch printing

Text in a layout may contain `{{name}}` placeholders. Fill them through the CLI
per print, or drive a batch from a CSV file. The GUI edits and saves placeholders
but does not have a variable-value form; its preview shows the literal tokens.
CLI substitution changes only text content, preserving frames, independent
object dimensions, offsets, and rotation.

By default, USB CSV batches print as one continuous strip, including all
`--copies`, with final feed/cut only after the last row. `--chain` skips that final step. Printers
with manual cutters, such as PT-1230PC, still need manual cutting after feeding.
Bluetooth printing keeps its normal per-label behavior.

CSV input is processed incrementally, with at most one label of lookahead for
USB printing. If a later row cannot be parsed or rendered, the CLI finishes an
already printed USB strip unless `--chain` was requested, then reports the error.
Printer communication failures are not retried.

```sh
# See which placeholders a layout declares
ptouch print --layout badge.ptl --list-vars

# Fill placeholders for a single label
ptouch print --layout badge.ptl --set name=Alice --set id=A001

# One label per CSV row; the header row names the placeholders
ptouch print --layout badge.ptl --csv people.csv

# Batch to image files instead of a printer ({n} is the row number)
ptouch print --layout badge.ptl --csv people.csv -o 'badge-{n}.png'

# CSV from stdin, with a constant value applied to every row
cat people.csv | ptouch print --layout badge.ptl --csv - --set dept=Eng
```

### Print options

| Flag | Long | Description |
|------|------|-------------|
| | `TEXT...` | Text lines (max 4) |
| `-l` | `--layout` | Print a saved layout file (.ptl); overrides content flags |
| | `--set` | Set a layout placeholder, `KEY=VALUE` (repeatable) |
| | `--csv` | Batch-print one label per CSV row (`-` for stdin) |
| | `--list-vars` | List the placeholders a layout declares, then exit |
| | `--allow-missing` | Render placeholders with no value as blank |
| `-i` | `--image` | Image file path |
| `-o` | `--output` | Export to image file instead of printing |
| `-f` | `--font` | Font name (default: DejaVuSans) |
| `-s` | `--size` | Font size in points (auto if omitted) |
| `-m` | `--margin` | Top/bottom margin in pixels |
| `-a` | `--align` | Text alignment: left, center, right |
| `-w` | `--tape-width` | Force tape width in pixels (with `-o`) |
| `-c` | `--cut` | Add cut mark |
| `-p` | `--pad` | Add padding in pixels |
| | `--flip-h` | Mirror the whole label left-right (horizontal) |
| | `--flip-v` | Mirror the whole label top-bottom (vertical) |
| | `--chain` | Skip final feed/cut (chained labels) |
| | `--precut` | Cut before the label |
| | `--binarize` | Binarization: auto, threshold, dither |
| | `--copies` | Number of copies |
| | `--timeout` | Printer timeout in seconds |
| | `--debug` | Enable debug output |

## USB Permissions (Linux)

The `.deb`/`.rpm` install and load this rule for you. To do it manually, copy
the udev rules file:

```sh
sudo cp data/udev/20-usb-ptouch-permissions.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger
```

## Desktop Integration (Linux)

The `.deb`/`.rpm` already install these. To do it manually, install the desktop
entry and icon so `ptouch-gui` appears in your application menu:

```sh
sudo install -Dm644 data/io.github.vowstar.ptouch-gui.desktop \
  /usr/share/applications/io.github.vowstar.ptouch-gui.desktop
sudo install -Dm644 data/io.github.vowstar.ptouch-gui.svg \
  /usr/share/icons/hicolor/scalable/apps/io.github.vowstar.ptouch-gui.svg
sudo gtk-update-icon-cache -f /usr/share/icons/hicolor 2>/dev/null || true
```

## Application Icon

`data/io.github.vowstar.ptouch-gui.svg` is the single source of truth. The
raster forms are generated from it by `scripts/gen-icons.sh` (needs
`rsvg-convert`, `magick`, and `python3`) and committed so normal builds need no
rasterizer:

- `crates/ptouch-gui/assets/icon.png` embedded as the runtime window icon
- `data/windows/ptouch-gui.ico` embedded into the `.exe` by `build.rs`
- `data/macos/ptouch-gui.icns` used by `cargo bundle` for the macOS `.app`

Regenerate after editing the SVG, then commit the result. CI checks that the
committed rasters still match the SVG.

## USB Driver (Windows)

PT-P710BT (`04F9:20AF`) can use the built-in Windows `usbprint.sys` driver
with the native ARM64 and x64 applications from v0.8.6. Keep the existing driver
and close other printer applications. When it is the only supported printer,
`ptouch info` and the GUI select it automatically.

For explicit selection, copy the instance ID from `ptouch doctor`:

```powershell
.\ptouch-windows-arm64.exe info --usbprint 'INSTANCE_ID_FROM_DOCTOR'
.\ptouch-windows-arm64.exe print --usbprint 'INSTANCE_ID_FROM_DOCTOR' 'Hello'
```

The GUI also lists PT-P710BT devices directly. `--usb BUS:ADDRESS` continues to
select the libusb path. Other USBPRINT models are not enabled yet.

For printers already using WinUSB, and for other models, communication continues
through libusb. If a compatible driver is needed, the existing installation path is:

1. Download [Zadig](https://zadig.akeo.ie/).
2. Plug in the printer, then choose `Options > List All Devices`.
3. Select your printer in the list (Brother VID `04F9`).
4. Pick `WinUSB` as the target driver and click `Replace Driver`.
5. Run `ptouch info` again.

After this the normal Brother software no longer sees the printer. Undo it any
time by uninstalling or rolling back the driver in Device Manager.

### USB diagnostics and device selection

```sh
ptouch doctor
ptouch doctor --json
ptouch doctor --probe --json
ptouch info --usb 1:5
ptouch print --usb 1:5 --job-timeout 900 "Label"
```

`doctor` reports the executable and native architectures, linked libusb version,
USB locations, endpoint selection, and Windows PnP driver services. Default
discovery does not open printers or send printer commands. `--probe` also tests
open, claim, and release without detaching kernel drivers or changing alternate
settings. Check each stage in the JSON report. Missing devices remain a valid
report, and enumeration errors appear in `errors`.

Use the actual `BUS:ADDRESS` from your report. USB addresses can change on
reconnect. Multiple matching printers require explicit selection. The GUI lists
USB locations in its Connection selector and preserves connection error messages.
Windows instance IDs can include serial numbers; redact them before sharing.

USB jobs have a ten-minute send/readiness limit. Override it with `--job-timeout`
(`--timeout` remains an alias). During printing, the libusb backend allows up to
30 seconds per write for printer flow control, bounded by the remaining job
budget. It checks cancellation every 250 milliseconds and resumes partial writes
from the first unacknowledged byte. Initialization and status-query commands
retain their five-second transfer limit.

The GUI's Cancel button stops further USB work; sent data cannot be recalled.
Completion uses the remaining job budget. A missing completion/readiness
notification reports an unconfirmed outcome and stops a batch; an intermediate
receiving phase alone does not confirm completion on printers without a separate
readiness handshake. Check the printer before retrying to avoid duplicates.
This also applies to older models that previously treated completion timeouts
as success. Reinitialize a library session after a failed or cancelled job.

### Windows ARM64 installation failures

An ARM64 executable does not change the printer's driver binding. Windows 11
can emulate x64 applications, but kernel drivers need native ARM64 support.
PT-P710BT (`04F9:20AF`) is already in the model table. A device bound to
`usbprint` uses the native backend described above. WinUSB uses libusb.

The Brother printer driver and the Windows USB transport driver are separate.
Windows can load its built-in `usbprint.sys` without the Brother package.
See Microsoft's [USB printer driver documentation](https://learn.microsoft.com/en-us/windows-hardware/drivers/print/usb-printing).
The current libusb backend still requires a compatible binding, normally WinUSB,
as described in the [libusb Windows documentation](https://github.com/libusb/libusb/wiki/Windows).
Zadig is one installation tool, not an application dependency. An existing
WinUSB binding needs no Zadig installation. PT-P710BT bound to `usbprint`
uses the native backend without changing drivers.

Zadig 2.8 added ARM64 WinUSB installation support. Installation can still fail
because Windows rejects a generated driver package's signature. See
[libwdi #289](https://github.com/pbatard/libwdi/issues/289). The generic error
alone does not establish that this is the cause on a particular machine.

For an installation failure, collect the complete Zadig log, Windows build,
device hardware IDs, and the matching section of `%SystemRoot%\inf\setupapi.dev.log`.
Remove unrelated device identifiers before sharing logs.

Selecting the system `winusb.inf` file alone does not supply a device-specific
binding. Microsoft's [WinUSB installation guide](https://learn.microsoft.com/en-us/windows-hardware/drivers/usbcon/winusb-installation)
describes device matching, interface GUID registration, and signed catalog files.
A custom INF requires a valid signed package. Disabling signature enforcement
is not part of the project's installation procedure.

ARM64 acceptance requires status reads, single and chained labels, cutting,
long labels, device removal, and reconnect tests on the actual printer.
Keep driver-binding failures separate from application startup failures.

### Experimental USBPRINT status probe

The separate Windows example queries PT-P710BT status while the device remains
bound to `usbprint`. The CLI and GUI also support this driver from v0.8.6.
It sends no labels, reset commands, or cut commands and changes no driver settings.

Download `usbprint-probe-windows-arm64.exe` for Windows ARM64 or
`usbprint-probe-windows-amd64.exe` for Windows x64 from the release assets.
For example, on Windows ARM64:

```powershell
.\usbprint-probe-windows-arm64.exe --list
.\usbprint-probe-windows-arm64.exe --status 'DEVICE_PATH_FROM_LIST'
```

To build the same tool from source:

```powershell
cargo run -p ptouch-core --example usbprint_probe -- --list
cargo run -p ptouch-core --example usbprint_probe -- --status 'DEVICE_PATH_FROM_LIST'
```

Copy the complete device path from `--list`. The probe checks it against currently
present PT-P710BT interfaces before opening it exclusively. An occupied device
reports an error. Close other printer applications before testing.

Status I/O runs in a child process with a 15-second watchdog. On timeout, the
parent terminates and reaps the child. The probe never retries a short write.
A successful status query establishes only that one exchange worked. It does
not establish printing, cancellation, or spooler coexistence support.

### ARM64 hardware acceptance

Run the following on actual Linux ARM64, Windows ARM64, and macOS ARM64 hosts.
Record the commit, binary checksum, OS version, model, tape, connection type,
driver binding, result, and relevant diagnostic output for each case.

| Case | Required observation |
| --- | --- |
| Architecture and GUI | Native ARM64 executable starts and renders a label preview |
| Discovery and access | `doctor` identifies the device; explicit selection opens the intended printer |
| Status | Tape width and printer errors match the physical printer |
| Single label | Text orientation, margins, output, and cut match the preview |
| Chained labels | Multiple labels finish in order without duplicate or missing output |
| Long label | Printing survives transient silence within the configured job limit |
| Missing confirmation | UI reports an unknown outcome; no automatic retry or later batch page |
| Cancel | Further transmission stops; UI recovers after the current bounded transfer |
| Unplug and reconnect | Error is preserved; refreshed device selection reconnects successfully |
| Multiple devices | Automatic selection refuses ambiguity; explicit selection remains isolated |
| Driver coexistence | A busy device reports an error without driver replacement |

Windows additionally requires testing with ordinary user permissions. Hosted
Windows runners run as administrators and cannot establish this property.
Retain the existing Linux udev and macOS USB access procedures. PT-P300BT
Bluetooth validation remains specific to macOS and does not establish Windows
or Linux Bluetooth support.

A Windows 11 ARM64 user confirmed single-label printing on PT-P710BT with v0.8.6
in [#20](https://github.com/vowstar/ptouch-rs/issues/20#issuecomment-6075544246).
The remaining hardware cases above still require validation.
Do not run untrusted pull-request code on a runner connected to a printer.
The Windows USBPRINT probe remains experimental until separate hardware evidence
establishes bidirectional communication and resource cleanup under failure.

## USB Driver (macOS)

No driver replacement is needed. Install libusb and it works directly:

```sh
brew install libusb
```

If claiming the device fails with a busy or access error, make sure the
printer is not added as a print queue in System Settings.

## Project Structure

```
crates/
  ptouch-core/    -- USB transport, protocol, device/tape tables
  ptouch-render/  -- Bitmap, text rendering, image loading, raster
  ptouch-cli/     -- CLI binary
  ptouch-gui/     -- GUI binary (egui)
```

## Protocol References

The protocol and device layer is derived from ptouch-print (see License
below). Protocol details are additionally cross-checked against Brother's
published command references and the device behavior documented by other
open source drivers:

- [Brother PT-E550W/PT-P750W/PT-P710BT Raster Command Reference](https://download.brother.com/welcome/docp100064/cv_pte550wp750wp710bt_eng_raster_102.pdf)
- [Brother PT-H500/PT-P700/PT-E500 Raster Command Reference](https://download.brother.com/welcome/docp000771/cv_pth500p700e500_eng_raster_111.pdf)
- [Brother PT-P900/P900W/P950NW Raster Command Reference](https://download.brother.com/welcome/docp100407/cv_ptp900_eng_raster_102.pdf)
- [Brother PT-9700PC/PT-9800PCN ESC/P Command Reference](https://download.brother.com/welcome/docp000584/cv_pt9700_eng_escp_103.pdf)
- [printer-driver-ptouch](https://github.com/philpem/printer-driver-ptouch)
- [rasterprynt](https://github.com/boxine/rasterprynt)
- [pyPTouch](https://github.com/amathieson/pyPTouch)

## License

This project's printer protocol and device layer is derived from
[ptouch-print](https://git.familie-radermacher.ch/linux/ptouch-print.git) by
Dominic Radermacher and the ptouch-print contributors, which is licensed under
the GPLv3. Thanks to them for the reverse engineering that made this possible.

Because of that, the project as a whole and the distributed `ptouch` and
`ptouch-gui` binaries are licensed **GPL-3.0-or-later** (see [LICENSE](LICENSE)).

Per-directory licensing (each source file carries an SPDX header):

| Path | License | Notes |
|------|---------|-------|
| `crates/ptouch-core/` (except `printer_model.rs`) | GPL-3.0-or-later | Device table, flags, status protocol, command construction. Derived from ptouch-print. |
| `crates/ptouch-render/src/raster.rs` | GPL-3.0-or-later | Raster bit-packing. Derived from ptouch-print. |
| `crates/ptouch-render/` (other files) | MIT | Bitmap, text, image loading, composition. Original work. |
| `crates/ptouch-core/src/printer_model.rs` | MIT | Independently written capability selection. |
| `crates/ptouch-profiles/` | MIT | Independent portable configuration storage. |
| `crates/ptouch-cli/` | MIT | Original work. |
| `crates/ptouch-gui/` | MIT | Original work. |
| `crates/ptouch-gui/assets/symbols/mdi.json` | Apache-2.0 artwork | MDI symbols; see the bundled licenses and [NOTICE](NOTICE). |

The MIT-licensed files are reusable on their own under the MIT license (see
[LICENSE-MIT](LICENSE-MIT)). Any program that links `ptouch-core`, including the
binaries in this repository, is covered by the GPLv3. See [NOTICE](NOTICE) for
attribution details.

### USBPRINT implementation and verification

The native backend currently accepts PT-P710BT only. It uses SetupAPI device
properties for identification and opens the enumerated interface exclusively.
Multiple supported printers require explicit selection. The application does not
switch backends after sending data, retry a partial write, or replay a failed job.

USBPRINT I/O runs in a private process launched from the same application binary.
Overlapped reads and writes retain their buffers until completion or acknowledged
cancellation. The parent enforces an outer deadline and stops an unresponsive
worker. Cancellation cannot recall data already sent to the printer. Check the
printer before retrying an interrupted job.

Windows CI exercises native I/O through named pipes, worker deadlines and
cancellation, both application worker entry points, and malformed IPC requests.
CI also checks the GUI and console executable subsystems in debug and release
builds. Hidden worker launches must preserve stdin/stdout communication.
These checks do not replace physical printer acceptance.
