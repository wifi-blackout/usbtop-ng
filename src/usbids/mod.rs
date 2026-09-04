//! usb.ids database: VID and PID to name resolution, lsusb parity.
//! Format (see the file's own header): `VVVV  vendor name` at column 0,
//! `\tPPPP  product name` under it. Single-letter section headers (`C `,
//! `AT `, `HID ` and the rest) end the vendor list — everything from the
//! first such line on is class data usbtop-ng does not use.
//!
//! This module also owns the `--update-usbids` downloader: a
//! `std::process::Command` shell-out to curl or wget (no new dependency),
//! https-only with a pinned URL and a TLS floor, that lands the payload in
//! quarantine, validates it with this module's own memory-safe parser (text
//! in, names out — no execution path), diffs it against the active local
//! copy, and only then installs it with a same-directory atomic rename.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::Write as _;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

#[derive(Debug)]
struct Vendor {
    name: String,
    products: HashMap<u16, String>,
}

#[derive(Debug)]
pub struct UsbIds {
    vendors: HashMap<u16, Vendor>,
}

impl UsbIds {
    pub fn parse(text: &str) -> UsbIds {
        let mut vendors: HashMap<u16, Vendor> = HashMap::new();
        let mut current: Option<u16> = None;
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix('\t') {
                // A second tab is an interface line under a class section;
                // `current` is None past the vendor list, so both are skipped.
                if rest.starts_with('\t') {
                    continue;
                }
                let (Some(vid), Some((pid, name))) = (current, split_id_name(rest)) else {
                    continue;
                };
                if let Some(vendor) = vendors.get_mut(&vid) {
                    vendor.products.insert(pid, name.to_string());
                }
                continue;
            }
            match split_id_name(line) {
                Some((id, name)) => {
                    current = Some(id);
                    vendors.insert(
                        id,
                        Vendor {
                            name: name.to_string(),
                            products: HashMap::new(),
                        },
                    );
                }
                // A non-hex column-0 line (class and section headers) ends
                // the vendor list: without this, a `C 03` section's product
                // lines would attach to the last real vendor.
                None => current = None,
            }
        }
        UsbIds { vendors }
    }

    pub fn load(path: &Path) -> std::io::Result<UsbIds> {
        Ok(UsbIds::parse(&std::fs::read_to_string(path)?))
    }

    pub fn vendor_count(&self) -> usize {
        self.vendors.len()
    }

    /// Total product lines across every vendor, used by `diff_summary` to
    /// report a product delta alongside the vendor delta.
    pub fn product_count(&self) -> usize {
        self.vendors.values().map(|v| v.products.len()).sum()
    }

    pub fn vendor_name(&self, vid: u16) -> Option<&str> {
        self.vendors.get(&vid).map(|v| v.name.as_str())
    }

    pub fn product_name(&self, vid: u16, pid: u16) -> Option<&str> {
        self.vendors
            .get(&vid)?
            .products
            .get(&pid)
            .map(String::as_str)
    }
}

/// Split `VVVV  name`: exactly 4 hex digits, whitespace, a non-empty name.
fn split_id_name(line: &str) -> Option<(u16, &str)> {
    let (id, name) = line.split_at_checked(4)?;
    let id = u16::from_str_radix(id, 16).ok()?;
    let name = name.trim();
    (!name.is_empty()).then_some((id, name))
}

/// The distro-packaged locations, Debian and Ubuntu first, then the
/// hwdata path Fedora and openSUSE use (Ubuntu symlinks it to the first).
const DISTRO_PATHS: [&str; 2] = ["/usr/share/misc/usb.ids", "/usr/share/hwdata/usb.ids"];

/// The ordered list of sources `resolve_database` tries, and the same list
/// `--update-usbids` reads dates from: CLI flag, preferences key, the
/// downloaded copy, then the distro files. Factored into one function so
/// resolution and the check/pull table can never drift apart.
///
/// `home_copy` is `None` when `~/.usbtop-ng/usb.ids` could not be located
/// (typically HOME is unset) -- that chain entry is simply omitted rather
/// than the whole call failing, so `--config` still works in a HOME-less
/// environment. `--update-usbids`, which needs a home to write its own
/// download to, resolves that path itself and propagates the error instead
/// of calling this with `None`.
pub fn source_chain(
    cli_path: Option<&Path>,
    pref_path: Option<&Path>,
    home_copy: Option<&Path>,
) -> Vec<PathBuf> {
    let mut chain: Vec<PathBuf> = Vec::new();
    chain.extend(cli_path.map(Path::to_path_buf));
    chain.extend(pref_path.map(Path::to_path_buf));
    chain.extend(home_copy.map(Path::to_path_buf));
    chain.extend(DISTRO_PATHS.map(PathBuf::from));
    chain
}

/// First source that loads wins: CLI flag, preferences key, the downloaded
/// copy, then the distro files. A source that exists but cannot be read or
/// parsed logs one warning and falls through. None when nothing loads. See
/// `source_chain` for what a `None` `home_copy` means.
pub fn resolve_database(
    cli_path: Option<&Path>,
    pref_path: Option<&Path>,
    home_copy: Option<&Path>,
) -> Option<UsbIds> {
    let chain = source_chain(cli_path, pref_path, home_copy);
    let refs: Vec<&Path> = chain.iter().map(PathBuf::as_path).collect();
    resolve_from_chain(&refs)
}

/// One chain entry's load attempt, shared by `resolve_from_chain` and
/// `active_source` so the two literally cannot disagree about which source
/// wins: a missing path is silently skipped, a path that fails to open or
/// parse logs one warning and falls through, and a path that opens and
/// parses but yields zero vendors -- an empty or garbage file -- also warns
/// and falls through rather than silently winning the chain as an empty
/// database.
fn load_source(path: &Path) -> Option<UsbIds> {
    if !path.exists() {
        return None;
    }
    match UsbIds::load(path) {
        Ok(db) if db.vendor_count() == 0 => {
            log::warn!(
                "{} parsed but has no vendors; treating it as unusable and falling through",
                path.display()
            );
            None
        }
        Ok(db) => Some(db),
        Err(e) => {
            log::warn!("could not read {}: {e}", path.display());
            None
        }
    }
}

fn resolve_from_chain(paths: &[&Path]) -> Option<UsbIds> {
    for path in paths {
        if let Some(db) = load_source(path) {
            log::debug!("usb.ids loaded from {}", path.display());
            return Some(db);
        }
    }
    None
}

/// The path in `paths` that `resolve_from_chain` would pick: the first one
/// that exists and parses to a non-empty database. Used only for the
/// check/pull table's "(active)" marker and for reading the diff baseline;
/// resolution itself still goes through `resolve_from_chain`, and both call
/// the same `load_source` predicate, so the two never disagree by
/// construction rather than by convention.
pub(crate) fn active_source<'a>(paths: &[&'a Path]) -> Option<&'a Path> {
    paths.iter().find(|p| load_source(p).is_some()).copied()
}

/// The `# Date:` header of the *active* source in `paths` (the one
/// `resolve_from_chain` would actually pick), or `None` when there is no
/// active source or it carries no such header. This is what `check_usbids`
/// gates its advice on: a stale file elsewhere in the chain that is
/// shadowed by a newer active source is not worth advice, and a stale
/// active source is worth advice even when a newer copy sits shadowed
/// behind it (nobody is using that copy).
fn active_source_date(paths: &[&Path]) -> Option<(u16, u8, u8)> {
    let path = active_source(paths)?;
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| parse_header_date(&t))
}

/// The newest `# Date:` header across every *existing* source in `paths`,
/// active or not. This is what `pull_usbids`'s "already up to date" gate
/// uses: a pull is worth skipping the moment any local copy is already
/// current, since `--usbids`/`usbids_path` could point at whichever one
/// that is on the next run. Deliberately different from
/// `active_source_date` above.
fn newest_local_date(paths: &[&Path]) -> Option<(u16, u8, u8)> {
    paths
        .iter()
        .filter(|p| p.exists())
        .filter_map(|p| {
            std::fs::read_to_string(p)
                .ok()
                .and_then(|t| parse_header_date(&t))
        })
        .max()
}

/// The `# Date:` header of the file `pull_usbids` is about to overwrite, or
/// `None` when nothing is there yet. One of the two inputs `validation_floor`
/// takes the max of — the copy being replaced, never the chain-wide newest
/// (some other shadowed source being newer says nothing about whether the
/// payload about to land on `dest` is backdated).
fn replaced_copy_date(dest: &Path) -> Option<(u16, u8, u8)> {
    if !dest.exists() {
        return None;
    }
    std::fs::read_to_string(dest)
        .ok()
        .and_then(|t| parse_header_date(&t))
}

/// The floor a freshly fetched payload's date must clear before
/// `pull_usbids` installs it at `dest`: the newer of the file about to be
/// replaced (`replaced_copy_date`) and the active source in `chain_paths`
/// (`active_source_date`). Neither alone suffices — see each function's own
/// doc comment for why — so this combined floor is the only one
/// `pull_usbids` (and its tests) should ever pass to `validate_payload`.
/// `Option::max` orders `None` below `Some`, so a missing replaced copy
/// never lowers a floor the active source provides.
fn validation_floor(dest: &Path, chain_paths: &[&Path]) -> Option<(u16, u8, u8)> {
    replaced_copy_date(dest).max(active_source_date(chain_paths))
}

/// The compiled-in upstream URL. https-only, no redirect may leave https
/// (curl `--proto =https --proto-redir =https`, wget `--https-only`), and
/// this constant is the only place either tool's target comes from — never
/// a value passed in.
pub const UPSTREAM_URL: &str = "https://www.linux-usb.org/usb.ids";

/// A real usb.ids has ~3000 vendors; far fewer means a truncated or error
/// payload, and installing it would silently break every lookup.
const MIN_VENDORS: usize = 1000;

/// Upper bound on a fetched usb.ids payload's raw byte length. The real file
/// is about 700 KB, so this is generous headroom against a compromised or
/// misconfigured mirror streaming an unbounded body -- not a tight fit to
/// the real size. curl enforces this itself via `--max-filesize`; wget has
/// no size-limiting flag that works with the `-O-` streaming-to-stdout mode
/// this module uses, so `check_payload_size` enforces the same bound on the
/// buffered body after either tool returns.
const MAX_PAYLOAD_BYTES: u64 = 16 * 1024 * 1024;

/// Result of a payload that passed validation: its own `# Date:` header and
/// vendor count, printed by `pull_usbids` after the diff summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadSummary {
    pub date: (u16, u8, u8),
    pub vendor_count: usize,
}

fn fmt_date(d: (u16, u8, u8)) -> String {
    format!("{:04}-{:02}-{:02}", d.0, d.1, d.2)
}

/// Parse the `# Date:  YYYY-MM-DD HH:MM:SS` header line usb.ids carries.
/// Tab or run of spaces after the colon both occur across mirrors, so this
/// trims rather than matching either literally. `None` when no such line
/// exists (a payload with a missing or unrecognized header is never trusted
/// with a freshness comparison; see `validate_payload`).
pub(crate) fn parse_header_date(text: &str) -> Option<(u16, u8, u8)> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# Date:") {
            let date_part = rest.split_whitespace().next()?;
            let mut parts = date_part.split('-');
            let year: u16 = parts.next()?.parse().ok()?;
            let month: u8 = parts.next()?.parse().ok()?;
            let day: u8 = parts.next()?.parse().ok()?;
            return Some((year, month, day));
        }
    }
    None
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Parse an RFC 1123 HTTP date, e.g. `Mon, 18 Mar 2024 20:34:02 GMT`, the
/// format a `Last-Modified` header carries. std only: no dependency pulls in
/// a general date parser for this one field.
fn parse_http_date(s: &str) -> Option<(u16, u8, u8)> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }
    let day: u8 = parts[1].parse().ok()?;
    let month = MONTHS.iter().position(|m| *m == parts[2])? as u8 + 1;
    let year: u16 = parts[3].parse().ok()?;
    Some((year, month, day))
}

/// Pull `Last-Modified` out of a HEAD response's header text. curl `-I`
/// writes headers to stdout; wget's `-S` writes them to stderr regardless of
/// `-q`, so callers pass both streams concatenated.
fn extract_last_modified(headers: &str) -> Option<(u16, u8, u8)> {
    for line in headers.lines() {
        // Not every header line has a colon (e.g. the leading status line),
        // so a line without one is skipped rather than ending the search.
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("last-modified") {
            return parse_http_date(value.trim());
        }
    }
    None
}

/// Build the hardened download command: curl preferred, wget as fallback,
/// `None` when neither is on PATH. `probe` is the tool-on-PATH check,
/// injected so tests never touch the real PATH or spawn a process.
fn build_command(probe: impl Fn(&str) -> bool, head_only: bool) -> Option<std::process::Command> {
    if probe("curl") {
        let mut cmd = std::process::Command::new("curl");
        // `-L` (inside `-fsSL`) follows redirects; `--proto` alone only
        // pins the *initial* connection's protocol, so a plain https URL
        // could still redirect to http without `--proto-redir` pinning
        // every hop the same way.
        cmd.arg("-fsSL")
            .arg("--proto")
            .arg("=https")
            .arg("--proto-redir")
            .arg("=https")
            .arg("--tlsv1.2");
        if head_only {
            cmd.arg("-I");
        } else {
            // Cap the downloaded body so a compromised or misconfigured
            // mirror cannot stream an unbounded response; see
            // MAX_PAYLOAD_BYTES.
            cmd.arg("--max-filesize").arg(MAX_PAYLOAD_BYTES.to_string());
        }
        cmd.arg(UPSTREAM_URL);
        Some(cmd)
    } else if probe("wget") {
        let mut cmd = std::process::Command::new("wget");
        cmd.arg("-q");
        if head_only {
            // wget has no HEAD-only flag; --spider skips the body and -S
            // prints the response headers (to stderr, wget's own quirk).
            cmd.arg("--spider").arg("-S");
        } else {
            cmd.arg("-O-");
        }
        cmd.arg("--https-only")
            .arg("--secure-protocol=PFS")
            .arg(UPSTREAM_URL);
        Some(cmd)
    } else {
        None
    }
}

/// The full-body fetch command (`pull_usbids`).
pub fn fetch_command_from(probe: impl Fn(&str) -> bool) -> Option<std::process::Command> {
    build_command(probe, false)
}

/// The headers-only command (`check_usbids`'s single network touch, and
/// `pull_usbids`'s up-to-date gate).
pub fn head_command_from(probe: impl Fn(&str) -> bool) -> Option<std::process::Command> {
    build_command(probe, true)
}

/// Real tool-on-PATH probe used by the entry points; every unit test injects
/// a closure instead so command construction is verifiable without touching
/// the environment.
fn tool_on_path(name: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| dir.join(name).is_file())
}

/// Validate a fetched payload before it is ever installed: it must parse,
/// carry at least `MIN_VENDORS` vendors, and (when `prior` is given) carry a
/// `# Date:` no older than the copy it would replace. This is the only gate
/// between a downloaded payload and the filesystem — `pull_usbids` calls it
/// on the quarantined copy, never on live data.
pub fn validate_payload(text: &str, prior: Option<(u16, u8, u8)>) -> Result<PayloadSummary> {
    let db = UsbIds::parse(text);
    if db.vendor_count() < MIN_VENDORS {
        anyhow::bail!(
            "payload has only {} vendors, expected at least {MIN_VENDORS}; refusing to install a truncated or error payload",
            db.vendor_count()
        );
    }
    let date = parse_header_date(text).ok_or_else(|| {
        anyhow::anyhow!(
            "payload has no '# Date:' header; cannot confirm it is not older than the copy it replaces"
        )
    })?;
    if let Some(prior) = prior {
        if date < prior {
            anyhow::bail!(
                "payload is dated {}, older than the current copy dated {}; refusing to install a backdated payload",
                fmt_date(date),
                fmt_date(prior)
            );
        }
    }
    Ok(PayloadSummary {
        date,
        vendor_count: db.vendor_count(),
    })
}

/// Summarize old vs. new: both dates, the vendor count delta, and the
/// product count delta, printed by `pull_usbids` right before the install.
pub fn diff_summary(old: &str, new: &str) -> String {
    let old_db = UsbIds::parse(old);
    let new_db = UsbIds::parse(new);
    let old_date = parse_header_date(old)
        .map(fmt_date)
        .unwrap_or_else(|| "unknown".to_string());
    let new_date = parse_header_date(new)
        .map(fmt_date)
        .unwrap_or_else(|| "unknown".to_string());
    let vendor_delta = new_db.vendor_count() as i64 - old_db.vendor_count() as i64;
    let product_delta = new_db.product_count() as i64 - old_db.product_count() as i64;
    format!("{old_date} -> {new_date}: {vendor_delta:+} vendors, {product_delta:+} products")
}

/// Whether upstream is newer than the newest local `# Date:` in the chain.
/// No local copy at all counts as "pull it".
pub fn should_pull(upstream: (u16, u8, u8), newest_local: Option<(u16, u8, u8)>) -> bool {
    match newest_local {
        None => true,
        Some(local) => upstream > local,
    }
}

/// Advice for catching a local copy up to `upstream`, package-manager route
/// first: apt names the `usb.ids` package directly; dnf, zypper, and pacman
/// all fold usb.ids into `hwdata`. `--update-usbids pull` is offered last,
/// as the explicit hardened fetch, regardless of which package manager (if
/// any) `probe` finds.
pub fn advise(probe: impl Fn(&str) -> bool) -> String {
    let mut out = String::new();
    if probe("apt") {
        out.push_str(
            "Prefer the distro package: sudo apt update && sudo apt install --only-upgrade usb.ids\n",
        );
    } else if probe("dnf") {
        out.push_str("Prefer the distro package: sudo dnf update hwdata\n");
    } else if probe("zypper") {
        out.push_str("Prefer the distro package: sudo zypper update hwdata\n");
    } else if probe("pacman") {
        out.push_str("Prefer the distro package: sudo pacman -Syu hwdata\n");
    }
    out.push_str("Or fetch the upstream copy directly: usbtop-ng --update-usbids pull\n");
    out
}

/// Read every local source's `# Date:` header and print a table (path,
/// date, which one is active), then make one HTTPS HEAD request for the
/// upstream `Last-Modified` date and advise how to catch up. This is the
/// only network touch in check mode: headers, no body. Exits (via the
/// returned `Err`) only when upstream is unreachable; every other outcome,
/// including "nothing is outdated", is `Ok`.
pub fn check_usbids(chain_paths: &[&Path]) -> Result<()> {
    let active = active_source(chain_paths);
    let active_date = active_source_date(chain_paths);
    println!("Local usb.ids sources:");
    let mut any_local = false;
    for path in chain_paths {
        if !path.exists() {
            continue;
        }
        any_local = true;
        let date = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| parse_header_date(&t));
        let marker = if active == Some(*path) {
            "  (active)"
        } else {
            ""
        };
        let date_str = date
            .map(fmt_date)
            .unwrap_or_else(|| "no date header".to_string());
        println!("  {} - {}{}", path.display(), date_str, marker);
    }
    if !any_local {
        println!("  (none found; names come from device strings only)");
    }

    let Some(mut cmd) = head_command_from(tool_on_path) else {
        println!("upstream unreachable: neither curl nor wget found on PATH");
        anyhow::bail!("no download tool available to check the upstream date");
    };
    let output = cmd
        .output()
        .context("running the upstream HEAD request")
        .and_then(|o| {
            if o.status.success() {
                Ok(o)
            } else {
                // Include stderr like the pull fetch path does below: without
                // it, a TLS failure surfaces as nothing but an exit code
                // (e.g. "exited with exit status: 60"), hiding the actual
                // curl/wget message a user would need to diagnose it.
                Err(anyhow::anyhow!(
                    "upstream HEAD request exited with {}: {}",
                    o.status,
                    String::from_utf8_lossy(&o.stderr)
                ))
            }
        });
    let output = match output {
        Ok(o) => o,
        Err(e) => {
            println!("upstream unreachable: {UPSTREAM_URL} ({e})");
            anyhow::bail!("upstream usb.ids HEAD request failed");
        }
    };
    let headers = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let Some(upstream_date) = extract_last_modified(&headers) else {
        println!("upstream unreachable: no parseable Last-Modified header");
        anyhow::bail!("upstream usb.ids response had no parseable Last-Modified header");
    };
    println!("Upstream ({UPSTREAM_URL}): {}", fmt_date(upstream_date));

    // Gated on the active source specifically, not the chain-wide newest: a
    // stale file shadowed by a newer active source is not worth advice, and
    // a stale active source is worth advice even with a newer copy shadowed
    // behind it -- nobody is using that copy.
    if should_pull(upstream_date, active_date) {
        print!("{}", advise(tool_on_path));
    } else {
        println!("Local copy is up to date; nothing to do.");
    }
    Ok(())
}

/// Reject a fetched body before any further processing when its raw byte
/// length exceeds `MAX_PAYLOAD_BYTES`. See `MAX_PAYLOAD_BYTES` for why this
/// exists alongside curl's own `--max-filesize`.
fn check_payload_size(len: usize) -> Result<()> {
    if len as u64 > MAX_PAYLOAD_BYTES {
        anyhow::bail!(
            "fetched usb.ids payload is {len} bytes, exceeding the {MAX_PAYLOAD_BYTES}-byte cap; refusing to process it"
        );
    }
    Ok(())
}

/// Write `payload` to `quarantine` without ever writing through a
/// pre-existing symlink there. A leftover tmp file from an earlier,
/// interrupted pull is removed first (errors ignored -- there may be
/// nothing to remove), then the file is opened with `create_new` so
/// anything that exists at that path by the time of the open -- the
/// original stale file if the remove somehow didn't clear it, a symlink an
/// attacker planted, or a file an attacker recreated in the gap -- makes the
/// open fail instead of being written through.
fn write_quarantine_file(quarantine: &Path, payload: &str) -> Result<()> {
    let _ = std::fs::remove_file(quarantine);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(quarantine)
        .with_context(|| format!("creating quarantine file {}", quarantine.display()))?;
    file.write_all(payload.as_bytes())
        .with_context(|| format!("writing quarantine file {}", quarantine.display()))?;
    // Chowns the fd this call itself just created with `create_new`, not a
    // re-resolved path -- see `chown_created_to_invoker`'s doc comment. The
    // eventual same-directory rename into `dest` (in `pull_usbids`, below)
    // preserves this ownership, so nothing needs to chown `dest` again after
    // the rename.
    crate::config::chown_created_to_invoker(quarantine, file.as_raw_fd());
    Ok(())
}

/// Fetch the upstream usb.ids and install it, but only after it earns the
/// spot: skip outright when nothing local is older, quarantine the payload
/// next to `dest`, validate and diff it, then install by same-directory
/// atomic rename. Any failure before that rename leaves every existing file
/// untouched.
pub fn pull_usbids(dest: &Path, chain_paths: &[&Path]) -> Result<()> {
    // The "already up to date" gate looks at every local copy (the reader
    // could be using any of them); the validation floor below looks only
    // at the one file this call is about to overwrite.
    let newest_local = newest_local_date(chain_paths);

    let Some(mut head_cmd) = head_command_from(tool_on_path) else {
        anyhow::bail!("neither curl nor wget found on PATH; cannot check the upstream date");
    };
    let head_output = head_cmd
        .output()
        .context("running the upstream HEAD request")?;
    if !head_output.status.success() {
        anyhow::bail!("upstream HEAD request exited with {}", head_output.status);
    }
    let headers = format!(
        "{}\n{}",
        String::from_utf8_lossy(&head_output.stdout),
        String::from_utf8_lossy(&head_output.stderr)
    );
    let upstream_date = extract_last_modified(&headers).ok_or_else(|| {
        anyhow::anyhow!("upstream response had no parseable Last-Modified header")
    })?;

    if !should_pull(upstream_date, newest_local) {
        println!("already up to date ({})", fmt_date(upstream_date));
        return Ok(());
    }

    let Some(mut fetch_cmd) = fetch_command_from(tool_on_path) else {
        anyhow::bail!("neither curl nor wget found on PATH; cannot fetch usb.ids");
    };
    let fetch_output = fetch_cmd.output().context("running the upstream fetch")?;
    if !fetch_output.status.success() {
        anyhow::bail!(
            "fetching usb.ids failed: {}",
            String::from_utf8_lossy(&fetch_output.stderr)
        );
    }
    // curl already enforces MAX_PAYLOAD_BYTES itself via --max-filesize; this
    // check is the backstop for wget, which has no size limit that works
    // with the -O- streaming mode used here, and it costs nothing to apply
    // to both before the body is processed any further.
    check_payload_size(fetch_output.stdout.len())?;
    let payload = String::from_utf8(fetch_output.stdout)
        .context("upstream usb.ids response was not valid UTF-8")?;

    // Quarantine: the payload lands next to `dest` and is parsed only by our
    // own memory-safe parser above -- never executed, never installed as-is.
    if let Some(parent) = dest.parent() {
        crate::config::ensure_private_config_dir(parent)?;
    }
    let quarantine = dest.with_extension("ids.tmp");
    write_quarantine_file(&quarantine, &payload)?;

    // The floor is the newer of the file about to be replaced and the
    // active source in the chain (not the chain-wide newest). On a first
    // pull there is no replaced copy at all, so without the active
    // source's date a replayed, older-but-otherwise-valid payload could
    // install and shadow whatever newer copy (e.g. a distro package) is
    // actually in use. See `validation_floor`.
    let floor = validation_floor(dest, chain_paths);
    let validated = validate_payload(&payload, floor);
    let summary = match validated {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_file(&quarantine);
            return Err(e);
        }
    };

    let active_text = active_source(chain_paths)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    println!("{}", diff_summary(&active_text, &payload));

    // Same-directory atomic rename: the only way this payload ever reaches
    // `dest`, and only after the checks above passed. A failed rename must
    // not leave the quarantine file behind either.
    // The quarantine file was already chowned to the invoker (in
    // `write_quarantine_file`, above) before it ever existed at this path;
    // `rename(2)` changes an entry's name, not its inode's ownership, so
    // `dest` inherits that ownership as-is and needs no chown of its own
    // here -- doing one would mean re-resolving `dest` as a path after the
    // rename, exactly the kind of post-hoc path lookup this module no
    // longer does.
    if let Err(e) = std::fs::rename(&quarantine, dest) {
        let _ = std::fs::remove_file(&quarantine);
        return Err(e).with_context(|| format!("installing {}", dest.display()));
    }
    println!("installed {} ({})", dest.display(), fmt_date(summary.date));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
# Date:    2024-03-18 20:34:02
#
0430  Fujitsu Component Limited
\t0100  3-button Mouse
\t0a02  Keyboard
05e3  Genesys Logic, Inc.
\t0610  Hub
1a6e  Global Unichip Corp.
garbage line that is not a vendor
ffff  Last Vendor
C 03  HID (Human Interface Device)
\t01  Boot Interface Subclass
";

    #[test]
    fn parses_vendors_and_products() {
        let db = UsbIds::parse(FIXTURE);
        assert_eq!(db.vendor_name(0x0430), Some("Fujitsu Component Limited"));
        assert_eq!(db.product_name(0x0430, 0x0100), Some("3-button Mouse"));
        assert_eq!(db.product_name(0x0430, 0x0a02), Some("Keyboard"));
        assert_eq!(db.vendor_name(0x1a6e), Some("Global Unichip Corp."));
        assert_eq!(
            db.product_name(0x1a6e, 0x089a),
            None,
            "vendor listed, product not"
        );
        assert_eq!(db.vendor_name(0x9999), None);
        assert_eq!(db.vendor_count(), 4);
        assert_eq!(db.product_count(), 3);
    }

    #[test]
    fn class_sections_and_garbage_are_ignored() {
        let db = UsbIds::parse(FIXTURE);
        // "C 03" must not be read as vendor 0xC03 or 0x03.
        assert_eq!(db.vendor_name(0x0c03), None);
        assert_eq!(db.vendor_name(0x0003), None);
        // The interface line under it must not become a product of any vendor.
        assert_eq!(db.product_name(0xffff, 0x0001), None);
    }

    #[test]
    fn parse_stops_attributing_products_after_the_class_section_starts() {
        let db = UsbIds::parse("0001  V\nC 03  HID\n\t0100  NotAProduct\n");
        assert_eq!(db.product_name(0x0001, 0x0100), None);
    }

    #[test]
    fn chain_returns_the_first_source_that_loads() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.ids");
        let good = temp.path().join("good.ids");
        std::fs::write(&good, "0430  Fujitsu Component Limited\n").unwrap();
        let later = temp.path().join("later.ids");
        std::fs::write(&later, "0001  Wrong Winner\n").unwrap();

        let db = resolve_from_chain(&[&missing, &good, &later]).expect("good.ids loads");
        assert_eq!(db.vendor_name(0x0430), Some("Fujitsu Component Limited"));
        assert_eq!(
            db.vendor_name(0x0001),
            None,
            "later sources must not merge in"
        );
    }

    #[test]
    fn chain_with_no_readable_source_is_none() {
        let temp = tempfile::tempdir().unwrap();
        assert!(resolve_from_chain(&[&temp.path().join("nope.ids")]).is_none());
    }

    #[test]
    fn an_empty_file_ahead_of_a_good_file_falls_through_and_the_good_file_wins() {
        // A file that exists and parses cleanly but yields zero vendors (an
        // empty file, or one that is all comments) must not silently win the
        // chain as an empty database -- it has to warn and fall through just
        // like an unreadable or unparseable file does.
        let temp = tempfile::tempdir().unwrap();
        let empty = temp.path().join("empty.ids");
        std::fs::write(&empty, "").unwrap();
        let good = temp.path().join("good.ids");
        std::fs::write(&good, "0430  Fujitsu Component Limited\n").unwrap();

        let db = resolve_from_chain(&[&empty, &good]).expect("good.ids loads after the empty one");
        assert_eq!(db.vendor_name(0x0430), Some("Fujitsu Component Limited"));

        // active_source must agree: it shares the same load_source predicate,
        // so it cannot pick the empty file either.
        let chain: Vec<&Path> = vec![&empty, &good];
        assert_eq!(active_source(&chain), Some(good.as_path()));
    }

    #[test]
    fn a_chain_of_only_empty_files_resolves_none() {
        let temp = tempfile::tempdir().unwrap();
        let empty = temp.path().join("empty.ids");
        std::fs::write(&empty, "").unwrap();
        let comments_only = temp.path().join("comments.ids");
        std::fs::write(&comments_only, "# nothing but a header\n").unwrap();

        assert!(resolve_from_chain(&[&empty, &comments_only]).is_none());

        let chain: Vec<&Path> = vec![&empty, &comments_only];
        assert!(
            active_source(&chain).is_none(),
            "active_source must agree with resolve_from_chain"
        );
    }

    #[test]
    fn resolve_database_prefers_the_cli_path_over_the_rest_of_the_chain() {
        // A hermetic exercise of the public entry point itself (the chain
        // logic is covered above via resolve_from_chain): the CLI path
        // loads and wins on the first iteration, so the distro constants
        // are never `.exists()`-checked and /usr/share is never touched.
        let temp = tempfile::tempdir().unwrap();
        let cli = temp.path().join("cli.ids");
        std::fs::write(&cli, "0430  Fujitsu Component Limited\n").unwrap();
        let home_copy = temp.path().join("home.ids");

        let db = resolve_database(Some(&cli), None, Some(&home_copy)).expect("cli path loads");
        assert_eq!(db.vendor_name(0x0430), Some("Fujitsu Component Limited"));
    }

    #[test]
    fn resolve_database_prefers_the_pref_path_over_the_home_copy() {
        let temp = tempfile::tempdir().unwrap();
        let pref = temp.path().join("pref.ids");
        std::fs::write(&pref, "0430  Fujitsu Component Limited\n").unwrap();
        let home_copy = temp.path().join("home.ids");
        std::fs::write(&home_copy, "0001  Wrong Winner\n").unwrap();

        let db = resolve_database(None, Some(&pref), Some(&home_copy)).expect("pref path loads");
        assert_eq!(db.vendor_name(0x0430), Some("Fujitsu Component Limited"));
        assert_eq!(db.vendor_name(0x0001), None, "home copy must not win");
    }

    #[test]
    fn resolve_database_prefers_the_home_copy_over_the_distro_paths() {
        // The home copy loads and wins on this iteration, so resolve_from_chain
        // returns before ever `.exists()`-checking the real DISTRO_PATHS
        // constants -- /usr/share is never touched, keeping this hermetic
        // even though those constants point to real system paths.
        let temp = tempfile::tempdir().unwrap();
        let home_copy = temp.path().join("home.ids");
        std::fs::write(&home_copy, "0430  Fujitsu Component Limited\n").unwrap();

        let db = resolve_database(None, None, Some(&home_copy)).expect("home copy loads");
        assert_eq!(db.vendor_name(0x0430), Some("Fujitsu Component Limited"));
    }

    #[test]
    fn source_chain_orders_cli_then_pref_then_home_then_distro() {
        let cli = Path::new("/cli.ids");
        let pref = Path::new("/pref.ids");
        let home = Path::new("/home/user/.usbtop-ng/usb.ids");
        let chain = source_chain(Some(cli), Some(pref), Some(home));
        assert_eq!(
            chain,
            vec![
                PathBuf::from(cli),
                PathBuf::from(pref),
                PathBuf::from(home),
                PathBuf::from(DISTRO_PATHS[0]),
                PathBuf::from(DISTRO_PATHS[1]),
            ]
        );
    }

    #[test]
    fn source_chain_omits_the_home_copy_entry_when_it_is_none() {
        // `home_copy` is `None` when `preferences_path()` failed (typically
        // HOME unset). That must simply drop the entry from the chain, not
        // fail the whole call -- monitoring with `--config` still has to
        // work without a home.
        let cli = Path::new("/cli.ids");
        let pref = Path::new("/pref.ids");
        let chain = source_chain(Some(cli), Some(pref), None);
        assert_eq!(
            chain,
            vec![
                PathBuf::from(cli),
                PathBuf::from(pref),
                PathBuf::from(DISTRO_PATHS[0]),
                PathBuf::from(DISTRO_PATHS[1]),
            ]
        );

        // Also true with nothing else in the chain either.
        assert_eq!(
            source_chain(None, None, None),
            vec![
                PathBuf::from(DISTRO_PATHS[0]),
                PathBuf::from(DISTRO_PATHS[1]),
            ]
        );
    }

    #[test]
    fn header_date_parses_from_the_hash_date_line() {
        assert_eq!(
            parse_header_date("# Date:\t2024-03-18 20:34:02\n0001  V\n"),
            Some((2024, 3, 18))
        );
        assert_eq!(parse_header_date("0001  V\n"), None, "no header at all");
    }

    #[test]
    fn http_last_modified_parses_rfc1123() {
        assert_eq!(
            parse_http_date("Mon, 18 Mar 2024 20:34:02 GMT"),
            Some((2024, 3, 18))
        );
        assert_eq!(parse_http_date("not a date"), None);
        assert_eq!(parse_http_date(""), None);

        // The month table covers all 12 entries.
        let expected = [
            ("Jan", 1),
            ("Feb", 2),
            ("Mar", 3),
            ("Apr", 4),
            ("May", 5),
            ("Jun", 6),
            ("Jul", 7),
            ("Aug", 8),
            ("Sep", 9),
            ("Oct", 10),
            ("Nov", 11),
            ("Dec", 12),
        ];
        for (name, num) in expected {
            let http_date = format!("Mon, 01 {name} 2024 00:00:00 GMT");
            assert_eq!(
                parse_http_date(&http_date),
                Some((2024, num, 1)),
                "month {name}"
            );
        }
    }

    #[test]
    fn extract_last_modified_finds_the_header_case_insensitively() {
        let headers = "HTTP/2 200\r\ndate: Wed, 19 Aug 2026 12:00:00 GMT\r\nLast-Modified: Mon, 18 Mar 2024 20:34:02 GMT\r\n";
        assert_eq!(extract_last_modified(headers), Some((2024, 3, 18)));
        assert_eq!(extract_last_modified("HTTP/2 200\r\n"), None);
    }

    #[test]
    fn commands_are_https_hardened_and_prefer_curl() {
        let cmd = fetch_command_from(|tool| tool == "curl").expect("curl found");
        assert_eq!(cmd.get_program(), "curl");
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(args.contains(&"--proto".to_string()));
        assert!(args.contains(&"=https".to_string()));
        assert!(
            args.contains(&"--proto-redir".to_string()),
            "a redirect must not be allowed to leave https either: {args:?}"
        );
        assert!(args.contains(&"--tlsv1.2".to_string()));
        assert!(args.contains(&UPSTREAM_URL.to_string()));
        assert!(
            args.contains(&"--max-filesize".to_string())
                && args.contains(&MAX_PAYLOAD_BYTES.to_string()),
            "the body fetch must cap the download size: {args:?}"
        );

        let head = head_command_from(|tool| tool == "curl").expect("curl found");
        let head_args: Vec<_> = head
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(head_args.contains(&"-I".to_string()));

        let wget_only = fetch_command_from(|tool| tool == "wget").expect("wget found");
        assert_eq!(wget_only.get_program(), "wget");
        let wget_args: Vec<_> = wget_only
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(wget_args.contains(&"--https-only".to_string()));
        assert!(wget_args.contains(&"--secure-protocol=PFS".to_string()));

        assert!(fetch_command_from(|_| false).is_none());
        assert!(head_command_from(|_| false).is_none());
    }

    fn generate_payload(date: &str, vendor_count: usize) -> String {
        let mut text = format!("# Date:   {date} 00:00:00\n#\n");
        for i in 0..vendor_count {
            text.push_str(&format!("{i:04x}  Vendor {i}\n"));
        }
        text
    }

    #[test]
    fn validate_rejects_small_and_backdated_payloads() {
        assert!(
            validate_payload("0001  V\n", None).is_err(),
            "under MIN_VENDORS must fail"
        );

        let backdated = generate_payload("2020-01-01", 1000);
        let err = validate_payload(&backdated, Some((2024, 3, 18)))
            .expect_err("older than the prior copy must fail");
        assert!(err.to_string().contains("backdated") || err.to_string().contains("older"));

        let fresh = generate_payload("2025-06-01", 1000);
        let summary =
            validate_payload(&fresh, Some((2024, 3, 18))).expect("newer payload must validate");
        assert_eq!(summary.date, (2025, 6, 1));
        assert_eq!(summary.vendor_count, 1000);
    }

    #[test]
    fn diff_summary_reports_date_and_count_deltas() {
        let old = "# Date:   2024-03-18 00:00:00\n0001  A\n\t0001  P\n";
        let new =
            "# Date:   2025-06-01 00:00:00\n0001  A\n\t0001  P\n0002  B\n\t0001  P2\n\t0002  P3\n";
        let summary = diff_summary(old, new);
        assert!(summary.contains("2024-03-18"));
        assert!(summary.contains("2025-06-01"));
        assert!(summary.contains("+1 vendors"));
        assert!(summary.contains("+2 products"));
    }

    #[test]
    fn advice_prefers_the_distro_package_route() {
        let apt = advise(|tool| tool == "apt");
        assert!(apt.contains("apt"));
        assert!(apt.contains("usb.ids"));
        assert!(apt.contains("--update-usbids pull"));

        for pm in ["dnf", "zypper", "pacman"] {
            let advice = advise(move |tool| tool == pm);
            assert!(advice.contains("hwdata"), "{pm} advice: {advice}");
            assert!(advice.contains("--update-usbids pull"));
        }

        let none = advise(|_| false);
        assert!(none.contains("--update-usbids pull"));
    }

    #[test]
    fn pull_skips_when_upstream_is_not_newer() {
        assert!(!should_pull((2024, 3, 18), Some((2024, 3, 18))));
        assert!(should_pull((2024, 3, 19), Some((2024, 3, 18))));
        assert!(should_pull((2024, 3, 18), None));
    }

    fn write_dated_fixture(path: &Path, date: &str) {
        std::fs::write(path, format!("# Date:   {date} 00:00:00\n0001  V\n")).unwrap();
    }

    // --- Three separately pinned staleness gates -----------------------
    //
    // check_usbids gates advice on the ACTIVE source's date; pull_usbids's
    // "already up to date" skip gates on the NEWEST local date across the
    // whole chain; pull_usbids's validate_payload floor gates on the date
    // of the file actually being REPLACED. These must not be conflated --
    // each has its own helper and its own tests below.

    #[test]
    fn check_gate_advises_when_the_active_source_is_stale_even_with_a_newer_shadowed_file() {
        // The active (first-in-chain) source is older than upstream; a
        // later, shadowed source is newer than upstream. Nobody is using
        // the shadowed copy, so advice must still fire.
        let temp = tempfile::tempdir().unwrap();
        let active = temp.path().join("active.ids");
        write_dated_fixture(&active, "2020-01-01");
        let shadowed = temp.path().join("shadowed.ids");
        write_dated_fixture(&shadowed, "2025-01-01");

        let chain: Vec<&Path> = vec![&active, &shadowed];
        assert_eq!(active_source_date(&chain), Some((2020, 1, 1)));
        assert!(should_pull((2024, 3, 18), active_source_date(&chain)));
    }

    #[test]
    fn check_gate_stays_quiet_when_the_active_source_is_fresh_despite_a_stale_shadowed_file() {
        // The active source is newer than upstream; a later, shadowed
        // source is stale. The shadowed staleness must not trigger advice.
        let temp = tempfile::tempdir().unwrap();
        let active = temp.path().join("active.ids");
        write_dated_fixture(&active, "2025-01-01");
        let shadowed = temp.path().join("shadowed.ids");
        write_dated_fixture(&shadowed, "2020-01-01");

        let chain: Vec<&Path> = vec![&active, &shadowed];
        assert_eq!(active_source_date(&chain), Some((2025, 1, 1)));
        assert!(!should_pull((2024, 3, 18), active_source_date(&chain)));
    }

    #[test]
    fn pull_gate_uses_the_newest_local_date_across_the_whole_chain() {
        // Unlike the check gate above, a stale active source shadowing a
        // newer copy elsewhere in the chain is enough to skip the pull:
        // any local file being current is grounds to skip re-fetching.
        let temp = tempfile::tempdir().unwrap();
        let active = temp.path().join("active.ids");
        write_dated_fixture(&active, "2020-01-01");
        let newer_elsewhere = temp.path().join("newer.ids");
        write_dated_fixture(&newer_elsewhere, "2025-01-01");

        let chain: Vec<&Path> = vec![&active, &newer_elsewhere];
        assert_eq!(newest_local_date(&chain), Some((2025, 1, 1)));
        assert!(!should_pull((2024, 3, 18), newest_local_date(&chain)));
    }

    #[test]
    fn validate_floor_is_the_replaced_copy_not_the_chain_newest() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("usb.ids");
        write_dated_fixture(&dest, "2020-01-01"); // the copy about to be replaced

        assert_eq!(replaced_copy_date(&dest), Some((2020, 1, 1)));

        let missing = temp.path().join("nope.ids");
        assert_eq!(replaced_copy_date(&missing), None, "nothing to replace yet");

        // No active source in this chain, so `validation_floor` reduces to
        // just the replaced copy's date.
        let chain: Vec<&Path> = vec![];

        // A payload dated after the replaced copy must validate.
        let payload = generate_payload("2022-06-01", 1000);
        assert!(validate_payload(&payload, validation_floor(&dest, &chain)).is_ok());
        // But it must still fail against the file it actually replaces if
        // that file is newer than the payload.
        write_dated_fixture(&dest, "2024-03-18");
        let err = validate_payload(&payload, validation_floor(&dest, &chain))
            .expect_err("payload predates the copy it would replace");
        assert!(err.to_string().contains("backdated") || err.to_string().contains("older"));
    }

    #[test]
    fn validate_floor_rises_to_the_active_source_when_there_is_no_home_copy() {
        // First pull: no home copy exists yet, so `replaced_copy_date` alone
        // gives no floor at all. The floor must still be the active
        // source's date (e.g. a distro-installed copy), so a replayed
        // payload older than it cannot install and shadow it.
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("usb.ids"); // no home copy yet
        let active = temp.path().join("active.ids");
        write_dated_fixture(&active, "2024-03-18");

        let chain: Vec<&Path> = vec![&dest, &active];
        assert_eq!(replaced_copy_date(&dest), None, "no home copy yet");
        let floor = validation_floor(&dest, &chain);
        assert_eq!(floor, Some((2024, 3, 18)));

        let older_payload = generate_payload("2020-01-01", 1000);
        let err = validate_payload(&older_payload, floor).expect_err(
            "older than the active source must fail even with no home copy to floor on",
        );
        assert!(err.to_string().contains("backdated") || err.to_string().contains("older"));

        let newer_payload = generate_payload("2025-01-01", 1000);
        assert!(
            validate_payload(&newer_payload, floor).is_ok(),
            "newer than the active source must still validate"
        );
    }

    #[test]
    fn validate_floor_is_the_max_of_replaced_copy_and_active_source() {
        // The floor can only rise: whichever of the replaced copy and the
        // active source is newer wins. Here the active source (earlier in
        // the chain, e.g. a --usbids override) is newer than the home copy
        // pull_usbids is about to replace.
        let temp = tempfile::tempdir().unwrap();
        let active = temp.path().join("active.ids");
        write_dated_fixture(&active, "2024-03-18"); // active source: newer
        let dest = temp.path().join("usb.ids");
        write_dated_fixture(&dest, "2020-01-01"); // replaced copy: older

        let chain: Vec<&Path> = vec![&active, &dest];
        let floor = validation_floor(&dest, &chain);
        assert_eq!(
            floor,
            Some((2024, 3, 18)),
            "the newer of the two dates wins"
        );

        let between = generate_payload("2022-01-01", 1000);
        let err = validate_payload(&between, floor).expect_err(
            "newer than the replaced copy but older than the active source must still fail",
        );
        assert!(err.to_string().contains("backdated") || err.to_string().contains("older"));
    }

    #[test]
    fn validate_floor_is_the_replaced_copy_when_it_is_newer_than_the_active_source() {
        // Mirror of the test above with the roles swapped: here the
        // replaced copy (the home file `pull_usbids` is about to overwrite)
        // is newer than the active source earlier in the chain. `max` must
        // still pick the newer one -- the replaced copy -- so reverting
        // `validation_floor` to `active_source_date` alone would let a
        // payload between the two dates wrongly validate.
        let temp = tempfile::tempdir().unwrap();
        let active = temp.path().join("active.ids");
        write_dated_fixture(&active, "2020-01-01"); // active source: older
        let dest = temp.path().join("usb.ids");
        write_dated_fixture(&dest, "2024-03-18"); // replaced copy: newer

        let chain: Vec<&Path> = vec![&active, &dest];
        let floor = validation_floor(&dest, &chain);
        assert_eq!(
            floor,
            Some((2024, 3, 18)),
            "the newer of the two dates wins"
        );

        let between = generate_payload("2022-01-01", 1000);
        let err = validate_payload(&between, floor).expect_err(
            "newer than the active source but older than the replaced copy must still fail",
        );
        assert!(err.to_string().contains("backdated") || err.to_string().contains("older"));
    }

    #[test]
    fn quarantine_write_does_not_follow_a_pre_existing_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let canary = temp.path().join("canary");
        std::fs::write(&canary, "untouched").unwrap();
        let quarantine = temp.path().join("usb.ids.tmp");
        std::os::unix::fs::symlink(&canary, &quarantine).unwrap();

        write_quarantine_file(&quarantine, "0430  Fujitsu Component Limited\n")
            .expect("stale symlink is removed and replaced, not written through");

        assert_eq!(
            std::fs::read_to_string(&canary).unwrap(),
            "untouched",
            "the symlink's target must never be written to"
        );
        assert!(
            !std::fs::symlink_metadata(&quarantine)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the quarantine path must now be a real file, not a symlink"
        );
        assert_eq!(
            std::fs::read_to_string(&quarantine).unwrap(),
            "0430  Fujitsu Component Limited\n"
        );
    }

    #[test]
    fn payload_size_check_rejects_bodies_over_the_cap() {
        assert!(check_payload_size(1024).is_ok());
        assert!(check_payload_size(MAX_PAYLOAD_BYTES as usize).is_ok());
        assert!(
            check_payload_size(MAX_PAYLOAD_BYTES as usize + 1).is_err(),
            "one byte over the cap must be rejected before further processing"
        );
    }
}
