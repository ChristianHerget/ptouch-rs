#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Reproduce the bundled MDI catalogue; normal builds never run this script."""

import argparse
import hashlib
import io
import json
from pathlib import Path
import re
import tarfile
import urllib.request
import xml.etree.ElementTree as ET

VERSION = "7.4.47"
ARCHIVE_URL = f"https://registry.npmjs.org/@mdi/svg/-/svg-{VERSION}.tgz"
ARCHIVE_SHA256 = "de92e5dc9ce46c392ab5c53aa7190b19f82b40cb48872a083f788c7e13e91fef"
APACHE_SHA256 = "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30"
DESTINATION = Path(__file__).resolve().parents[1] / "crates/ptouch-gui/assets/symbols"

CATEGORIES = [
    "Electrical & Power", "Networking & Computers", "Tools & Workshop",
    "Home & Storage", "Food & Drink", "Clothing & Laundry", "Animals & Nature",
    "People & Accessibility", "Transport & Places", "Health & Care",
    "Arrows & Navigation", "Warnings & Security", "Shapes, Numbers & Math",
    "Leisure & Celebration", "Other",
]

# One source tag can contribute to more than one label category. Name rules
# below supplement incomplete upstream tags, rather than replacing them.
TAG_GROUPS = {
    "Electrical & Power": ["Battery"],
    "Networking & Computers": ["Cellphone / Phone", "Cloud", "Database",
                               "Developer / Languages", "Device / Tech", "Printer"],
    "Tools & Workshop": ["Hardware / Tools", "Science"],
    "Home & Storage": ["Date / Time", "Files / Folders", "Home Automation", "Shopping"],
    "Food & Drink": ["Food / Drink"],
    "Clothing & Laundry": ["Clothing"],
    "Animals & Nature": ["Agriculture", "Animal", "Nature", "Weather"],
    "People & Accessibility": ["Account / User", "People / Family"],
    "Transport & Places": ["Automotive", "Geographic Information System", "Places",
                           "Transportation + Road", "Transportation + Other",
                           "Transportation + Flying", "Transportation + Water"],
    "Health & Care": ["Health / Beauty", "Medical / Hospital"],
    "Arrows & Navigation": ["Arrow", "Navigation"],
    "Warnings & Security": ["Alert / Error", "Lock", "Notification"],
    "Shapes, Numbers & Math": ["Alpha / Numeric", "Arrange", "Color", "Currency",
                              "Math", "Shape", "Text / Content / Format", "Vector"],
    "Leisure & Celebration": ["Audio", "Drawing / Art", "Emoji", "Gaming / RPG",
                              "Holiday", "Music", "Photography", "Religion",
                              "Sport", "Video / Movie"],
}
NAME_GROUPS = {
    "Electrical & Power": ["power", "plug", "socket", "electric", "electricity",
                           "lightning", "current", "fuse", "battery", "solar",
                           "transmission", "led", "lightbulb", "resistor"],
    "Networking & Computers": ["ethernet", "usb", "wifi", "lan", "wan", "network",
                               "router", "access-point", "server", "nas", "bluetooth",
                               "nfc", "ip", "chip", "cpu", "laptop", "desktop",
                               "monitor", "keyboard", "mouse", "hdmi", "sd"],
    "Tools & Workshop": ["tool", "wrench", "screw", "screwdriver", "hammer",
                         "saw", "drill", "pliers", "ruler", "tape-measure"],
    "Home & Storage": ["home", "house", "storage", "box", "package", "archive",
                       "cupboard", "cabinet", "shelf", "furniture", "key", "lamp"],
    "Clothing & Laundry": ["washing", "tumble", "iron", "hanger", "laundry",
                           "shoe", "tshirt", "sock", "wardrobe"],
    "People & Accessibility": ["wheelchair", "human", "accessibility", "hearing",
                               "braille", "blind", "sign-language"],
    "Health & Care": ["medical", "hospital", "pill", "bandage", "first-aid"],
    "Arrows & Navigation": ["arrow", "chevron", "compass", "direction"],
    "Warnings & Security": ["alert", "warning", "hazard", "security", "shield",
                            "lock", "fire", "biohazard", "radioactive"],
    "Shapes, Numbers & Math": ["circle", "square", "triangle", "rectangle",
                              "hexagon", "octagon", "numeric", "alphabetical"],
}
ACRONYMS = {word.lower(): word for word in ["USB", "LAN", "WAN", "NFC", "IP", "CPU",
                                           "HDMI", "SD", "LED", "AC", "DC", "RFID"]}
ACRONYMS["wifi"] = "Wi-Fi"


def groups(icon):
    tags = set(icon["tags"])
    found = {group for group, sources in TAG_GROUPS.items() if tags.intersection(sources)}
    for group, words in NAME_GROUPS.items():
        if any(re.search(r"(?:^|-)" + re.escape(word) + r"(?:-|$)", icon["name"])
               for word in words):
            found.add(group)
    return [group for group in CATEGORIES if group in found] or ["Other"]


def generate(blob):
    if hashlib.sha256(blob).hexdigest() != ARCHIVE_SHA256:
        raise ValueError("MDI archive checksum mismatch")
    with tarfile.open(fileobj=io.BytesIO(blob), mode="r:gz") as archive:
        metadata = json.load(archive.extractfile("package/meta.json"))
        icons = []
        for icon in sorted(metadata, key=lambda item: item["name"]):
            if icon["deprecated"] or "Brand / Logo" in icon["tags"]:
                continue
            svg = ET.fromstring(archive.extractfile(f"package/svg/{icon['name']}.svg").read())
            children = list(svg)
            # This release is path-only artwork on a 24-unit square viewport.
            # Fail on future structural changes rather than silently losing art.
            if (svg.tag != "{http://www.w3.org/2000/svg}svg"
                    or svg.attrib["viewBox"] != "0 0 24 24"
                    or len(children) != 1
                    or children[0].tag != "{http://www.w3.org/2000/svg}path"
                    or set(children[0].attrib) != {"d"}):
                raise ValueError(f"Unexpected SVG structure: {icon['name']}")
            icons.append({
                "id": icon["name"],
                "name": " ".join(ACRONYMS.get(word, word.capitalize())
                                 for word in icon["name"].split("-")),
                "aliases": icon["aliases"],
                "categories": groups(icon),
                "author": icon["author"],
                "path": children[0].attrib["d"],
            })
        assert len(icons) == 7188
        assert len({icon["id"] for icon in icons}) == len(icons)
        catalogue = {"version": VERSION, "categories": CATEGORIES, "icons": icons}
        return {
            "mdi.json": (json.dumps(catalogue, ensure_ascii=False, separators=(",", ":")) + "\n").encode(),
            "LICENSE-MDI.txt": archive.extractfile("package/LICENSE").read(),
        }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, help="Use a previously downloaded, verified archive")
    parser.add_argument("--check", action="store_true", help="Compare without writing files")
    args = parser.parse_args()
    blob = args.archive.read_bytes() if args.archive else urllib.request.urlopen(ARCHIVE_URL).read()
    outputs = generate(blob)
    license_path = DESTINATION / "LICENSE-APACHE-2.0.txt"
    apache = (license_path.read_bytes() if license_path.exists() else
              urllib.request.urlopen("https://www.apache.org/licenses/LICENSE-2.0.txt").read())
    if hashlib.sha256(apache).hexdigest() != APACHE_SHA256:
        raise ValueError("Apache license checksum mismatch")
    outputs[license_path.name] = apache
    if not args.check:
        DESTINATION.mkdir(parents=True, exist_ok=True)
    for name, content in outputs.items():
        path = DESTINATION / name
        if args.check:
            if not path.exists() or path.read_bytes() != content:
                parser.error(f"Generated catalogue differs: {path}")
        else:
            path.write_bytes(content)
    print(f"Verified {VERSION}: 7,188 symbols" if args.check else f"Imported {VERSION}: 7,188 symbols")


if __name__ == "__main__":
    main()
