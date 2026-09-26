#!/usr/bin/env python3
"""Generates `collector/src/windows_zones.rs` from CLDR's windowsZones.xml.

    curl -sS -o collector/tools/windowsZones.xml \
      https://raw.githubusercontent.com/unicode-org/cldr/<tag>/common/supplemental/windowsZones.xml
    python3 collector/tools/generate-windows-zones.py collector/tools/windowsZones.xml <tag> \
      > collector/src/windows_zones.rs

The XML is committed beside this script, and `zones::tests` derives the table
from it again in Rust and compares: the generated file is therefore pinned to
its source rather than trusted, and updating CLDR means replacing the XML,
re-running the line above, and watching that test.

Only the `territory="001"` rows are read. CLDR maps one Windows zone name to
several IANA zones, one per territory, and `001` is the row that says which of
them is *the* zone for that name — the same row every calendar client uses.
The territory-specific rows would need a territory to choose with, and an
iCalendar TZID carries none.

The table is data with a provenance, not a mapping this project invents: the
header records the CLDR tag and the two versions the file itself declares, so a
reader can fetch the same file and diff it.
"""
import sys
import xml.etree.ElementTree as ET

path, tag = sys.argv[1], sys.argv[2]
tree = ET.parse(path)
root = tree.getroot()
declared = root.find(".//mapTimezones")
rows = sorted(
    (zone.get("other"), zone.get("type").split()[0])
    for zone in root.iter("mapZone")
    if zone.get("territory") == "001"
)
print(f"""//! Windows time-zone names, and the IANA zone each one means.
//!
//! Generated, not written: see `collector/tools/generate-windows-zones.py`.
//!
//! - source: CLDR `{tag}`, `common/supplemental/windowsZones.xml`
//! - the file's own versions: `otherVersion="{declared.get('otherVersion')}"`, \
`typeVersion="{declared.get('typeVersion')}"`
//! - rows: the `territory="001"` ones, {len(rows)} of them — the row CLDR uses
//!   to say which IANA zone *is* a Windows name, which is what a TZID needs,
//!   since a TZID carries no territory to choose one with.
//!
//! Sorted by the Windows name, so the lookup is a binary search and a diff of
//! two CLDR releases is readable.

/// `(Windows name, IANA zone)`, sorted by the first.
pub(crate) const WINDOWS_ZONES: &[(&str, &str)] = &[""")
for other, iana in rows:
    print(f'    ("{other}", "{iana}"),')
print("];")
