# User-named connectors — design

**Date:** 2026-09-07
**Status:** approved for implementation (bounded change; no design
challenge per REVIEW.md, standard Codex review before merge)

## Goal

Let a user give a physical connector a name of their own, such as
`Left Type-A` or `Rear Right Type-C`, in the preferences file, and show that
name on the connector's heading in the device table. The point is
orientation: when a bottleneck shows, the heading should say which
receptacle on the machine it is, in the user's words, not only `Port 1.4`.

## Decisions

- **Where names live:** a `[connector_names]` table in
  `~/.usbtop-ng/preferences.toml` (the file `--config` can move). Absent by
  default; the `i` toggle's rewrite of the file preserves it; an empty table
  is never written.
- **What a key is.** Two forms are accepted, and a user can use either:
  - the position as the table shows it, `<bus>:<chain>`: `"3:1"` for the
    heading `Port 1 · bus 03 + 04`, `"3:1.4"` for `Port 1.4`. The bus may be
    either side of a paired connector (`"4:1"` names the same connector as
    `"3:1"`), so a user can read it off either device row;
  - the kernel's port object name, `usb3-port1` or `3-1-port4`, for anyone
    who reads sysfs.
  Keys are trimmed. A key that matches nothing is simply unused: a name for
  a connector that has no device on it is not shown and is not an error.
- **How a connector picks its name.** The USB2-side port is tried first,
  then the other side, each in both key forms; the first hit wins. A device
  without a port object (a kernel without them, or an old fixture) is still
  nameable: its port name and position follow from its own sysfs name.
- **How it shows.** `▶ Left Type-A (Port 1) · bus 03 + 04 · hub  rx … tx …`:
  the name leads, the position stays in parentheses so the chain the Port
  column prints is still on the heading. An unnamed connector renders
  exactly as before.
- **What does not change.** The `--once`/`--batch` reports, the `/` search
  (names are not searchable in this wave), the snapshot, the fixtures.

## Architecture

- `config::Preferences` gains `connector_names: BTreeMap<String, String>`
  with `#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]`.
- `ui::UsbTopApp` gains `connector_names` (set by a `with_connector_names`
  builder from `main`), `ConnectorView` gains `name: Option<String>`, and
  `connector_placement` resolves the name from the candidate keys of the
  placement's ports; `connector_line` renders it.
- A small pure helper in `ui`, `connector_name_keys(port: &PortRef) ->
  [String; 2]`, builds `<bus>:<chain>` and the port name for one side; the
  fallback placement builds them from `connector::port_of_device` and
  `connector::port_name`.

## Testing

- `config`: a file with a `[connector_names]` table loads it; a
  `Preferences` with names round-trips through `write_preferences_at`;
  the default file and a `Preferences` with no names serialize without the
  table (the existing exact-content assertions keep holding).
- `ui`: a paired connector named by `"3:1"`, by `"4:1"`, by `usb3-port1`,
  and by `usb4-port1` all render `Left Type-A (Port 1)`; the USB2 side wins
  when both sides are named; a nested hub port named by `"3:1.4"`; a
  fallback device (no port objects) named by its own position; an unnamed
  connector renders exactly the old heading; a key for a connector with no
  device shows nowhere; keys are trimmed.
- `main`: no test beyond compilation; the builder is one line.

## Documentation

- `README.md`: a bullet in "The device table" and a paragraph in
  "Preferences file" with the two key forms and an example table.
- `example-config.toml`: a commented `[connector_names]` example.
- `CHANGELOG.md`: Added.
