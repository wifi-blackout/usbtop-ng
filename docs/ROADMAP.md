# Roadmap

Ideas and follow-up work for usbtop-ng. Nothing here is committed work or a
schedule. Items move to [CHANGELOG.md](../CHANGELOG.md) when they ship.

## Feature ideas

- Device filtering and search in the device table.
- Export of bandwidth data to a file.
- One row per physical connector, using the sysfs port `peer` links. Today
  the USB2 side and the USB3 side of one connector list as sibling buses.
- Bus discovery without debugfs, so the binary interface stands alone. Today
  usbtop-ng finds buses through debugfs even when it reads `/dev/usbmon<bus>`.
- Plugin system for custom monitors.
- Monitoring of remote systems over the network.

## Engineering follow-ups

These came out of code review. Each is small and none blocks a release.

- A warning color for the `dropped:` and `shed:` counters. The palette has no
  warning hue, so both render like ordinary stats.
- An ellipsis on truncated table cells. Truncation is silent today.
- No empty parens in the bus header when the bus speed is unknown.
- One constant for the 60-second window. The device chart bounds and
  `RATE_HISTORY_WINDOW` state it separately.
- Error and log strings brought under the documentation style guide.

## Testing follow-ups

- A committed pty harness for the wedged-terminal checks. The checks run by
  hand today and are recorded in review reports only.
- A pipe-based regression guard proving the terminal-restore bytes bypass
  stdio buffering.
- Age-based tests that do not assume 2 minutes of machine uptime.
- Thunderbolt 3 and newer devices are untested.
- USB 4 and newer devices are untested.
- Generate a log with a normalized schema of hardware devices that have
  been tested. Ideas for this would include the date the test was performed
  along with the conditions (kernel version, relevant drivers, attached port
  chipsets, hubs). Then come up with an option to gather the data which
  could then be submitted as a contribution. But the methodology of this is
  open at this time.


## Notes
