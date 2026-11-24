# 0024 Labels as files for the Niimbot app, no own driver

Status: accepted, 2026-11-24 (owner)

The spec plans an own Rust driver for the NIIMBOT B21 Pro, ported from NiimBlueLib and
niimprint (Technology stack; Risks). NiimBlueLib's authors ask that it is not used
commercially without Niimbot's consent, and the protocol is reverse-engineered, so a
firmware update can break it.

Decision: no driver. For product labels (F18) and freezer labels (F18a) the system
produces, per batch:

- one PNG per label design at 300 dpi and its exact size (50 × 30 mm by default, longer
  when the text needs it), rendered by Typst with the pinned fonts, plus the copy count;
- CSV rows with the label fields for the Niimbot app's spreadsheet import.

The owner prints through the official Niimbot app. Labels are only downloadable once
allergens and, where needed, nutrition values are approved, as the spec requires. The
spec's printer tests and printer-error alerts fall away. The exact import format (CSV or
XLSX, image size limits) is checked against the app when Phase 2 builds this.
