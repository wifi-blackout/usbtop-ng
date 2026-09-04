//! The privacy rules, as pure functions with table tests. The boundary is
//! "host identity out, device identity in": nothing here ever touches a
//! device serial or descriptor; it rewrites the user's home directory to
//! `~`, masks the login name wherever it stands as a whole path component
//! (a removable-media mount, say), masks host MAC addresses in kernel log
//! lines, masks filesystem UUIDs in the kernel command line, and decides
//! which environment variables the bundle may record. Every substitution is
//! counted so the manifest can say what was changed.

use std::collections::BTreeMap;
use std::path::Path;

/// The only environment variables whose values a bundle records.
pub const ENV_ALLOWLIST: [&str; 5] = ["TERM", "COLORTERM", "LANG", "LC_ALL", "RUST_LOG"];

/// Variables recorded as present or absent only (see
/// `tui::sync::remote_session` for why any one of them means "over ssh").
pub const SSH_MARKERS: [&str; 3] = ["SSH_TTY", "SSH_CONNECTION", "SSH_CLIENT"];

/// Applies the rules and counts what it changed.
#[derive(Debug, Clone)]
pub struct Redactor {
    /// The home directory to rewrite, without a trailing slash; `None`
    /// disables the path rule (no home known, or a home of `/`, which would
    /// rewrite every absolute path).
    home: Option<String>,
    /// The login name (the home directory's last path component); `None`
    /// when `home` is `None`.
    user_name: Option<String>,
    counts: BTreeMap<&'static str, usize>,
}

impl Redactor {
    pub fn new(home: Option<&Path>) -> Redactor {
        let home = home
            .map(|h| h.to_string_lossy().trim_end_matches('/').to_string())
            .filter(|h| !h.is_empty());
        let user_name = home
            .as_deref()
            .and_then(|h| h.rsplit('/').next())
            .filter(|n| !n.is_empty())
            .map(str::to_string);
        Redactor {
            home,
            user_name,
            counts: BTreeMap::new(),
        }
    }

    fn bump(&mut self, rule: &'static str) {
        *self.counts.entry(rule).or_insert(0) += 1;
    }

    /// A path under the home directory becomes `~/…`; the home itself
    /// becomes `~`. Anything else is returned as written.
    pub fn path(&mut self, path: &Path) -> String {
        let text = path.to_string_lossy().into_owned();
        self.text(&text)
    }

    /// Every occurrence of the home directory inside free text (a
    /// preferences file, a command line, a report) becomes `~`, then every
    /// occurrence of the login name as a whole path component elsewhere in
    /// the text becomes `<user>` (see [`Redactor::mask_user_name`]). An
    /// occurrence of the home directory counts only at a path boundary: both
    /// the character before and the character after the match must be
    /// absent or not a path character. `/home/alice/x` matches,
    /// `/home/alice2/x` and `/opt/home/alice/data` do not. The home rule
    /// runs first and owns whatever text it examines, matched or not, so a
    /// login name that only ever appears as part of an unredacted home
    /// occurrence (`/opt/home/alice/data`) is never separately masked.
    pub fn text(&mut self, text: &str) -> String {
        let Some(home) = self.home.clone() else {
            return self.mask_user_name(text);
        };
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find(&home) {
            out.push_str(&self.mask_user_name(&rest[..at]));
            let before_ok = rest[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !is_path_char(c));
            let after = &rest[at + home.len()..];
            let after_ok = after.chars().next().is_none_or(|c| !is_path_char(c));
            if before_ok && after_ok {
                out.push('~');
                self.bump("home_path");
            } else {
                out.push_str(&home);
            }
            rest = after;
        }
        out.push_str(&self.mask_user_name(rest));
        out
    }

    /// Every occurrence of the login name as a whole path component becomes
    /// `<user>`. A whole component requires the character right before the
    /// match to be `/` (an occurrence at the very start of the text does not
    /// count: it is not a path component) and the character right after it
    /// to be absent or not a path character. `/media/alice/stick` and a
    /// trailing `.../alice` match; `/home/alice2/x`, `/opt/xalice/y`, and a
    /// bare `alice` do not.
    fn mask_user_name(&mut self, text: &str) -> String {
        let Some(name) = self.user_name.clone() else {
            return text.to_string();
        };
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find(&name) {
            let before_ok = rest[..at].ends_with('/');
            let after = &rest[at + name.len()..];
            let after_ok = after.chars().next().is_none_or(|c| !is_path_char(c));
            out.push_str(&rest[..at]);
            if before_ok && after_ok {
                out.push_str("<user>");
                self.bump("user_name");
            } else {
                out.push_str(&name);
            }
            rest = after;
        }
        out.push_str(rest);
        out
    }

    /// Masks each stand-alone `hh:hh:hh:hh:hh:hh` token as
    /// `xx:xx:xx:xx:xx:xx`. Applied to kernel log lines, where a USB
    /// network adapter's line names the host's own MAC; never to the device
    /// inventory, whose serial strings are device identity and stay.
    pub fn mac_addresses(&mut self, text: &str) -> String {
        const LEN: usize = 17;
        let mut bytes = text.as_bytes().to_vec();
        let mut i = 0;
        while i + LEN <= bytes.len() {
            if is_mac(&bytes[i..i + LEN])
                && !i.checked_sub(1).is_some_and(|p| is_mac_byte(bytes[p]))
                && !bytes.get(i + LEN).is_some_and(|&b| is_mac_byte(b))
            {
                bytes[i..i + LEN].copy_from_slice(b"xx:xx:xx:xx:xx:xx");
                self.bump("mac_address");
                i += LEN;
            } else {
                i += 1;
            }
        }
        // Only ASCII bytes were replaced by ASCII bytes, so the text is
        // still valid UTF-8.
        String::from_utf8(bytes).expect("ASCII-for-ASCII substitution keeps UTF-8 valid")
    }

    /// Masks the value after `UUID=` and `PARTUUID=` in a kernel command
    /// line; every other token is kept whole.
    pub fn cmdline(&mut self, text: &str) -> String {
        let tokens: Vec<String> = text
            .split_whitespace()
            .map(|token| match token.find("UUID=") {
                Some(at) => {
                    self.bump("fs_uuid");
                    format!("{}UUID=<redacted>", &token[..at])
                }
                None => token.to_string(),
            })
            .collect();
        tokens.join(" ")
    }

    pub fn env_allowlisted(name: &str) -> bool {
        ENV_ALLOWLIST.contains(&name)
    }

    /// Whether any ssh marker is set, given a predicate that answers "is this
    /// variable set and non-empty?". The values are never read here.
    pub fn ssh_present(present: impl Fn(&str) -> bool) -> bool {
        SSH_MARKERS.iter().any(|name| present(name))
    }

    /// Every rule that fired and how often, sorted by rule name.
    pub fn summary(&self) -> Vec<(String, usize)> {
        self.counts
            .iter()
            .map(|(rule, n)| (rule.to_string(), *n))
            .collect()
    }
}

fn is_path_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.')
}

fn is_mac_byte(b: u8) -> bool {
    b.is_ascii_hexdigit() || b == b':'
}

fn is_mac(window: &[u8]) -> bool {
    window.iter().enumerate().all(|(i, &b)| {
        if i % 3 == 2 {
            b == b':'
        } else {
            b.is_ascii_hexdigit()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_under_home_becomes_tilde() {
        let mut r = Redactor::new(Some(std::path::Path::new("/home/alice")));
        assert_eq!(
            r.path(std::path::Path::new("/home/alice/.usbtop-ng")),
            "~/.usbtop-ng"
        );
        assert_eq!(r.path(std::path::Path::new("/home/alice")), "~");
        assert_eq!(r.summary(), vec![("home_path".to_string(), 2)]);
    }

    #[test]
    fn a_sibling_directory_sharing_the_prefix_is_left_alone() {
        let mut r = Redactor::new(Some(std::path::Path::new("/home/alice")));
        assert_eq!(
            r.path(std::path::Path::new("/home/alice2/x")),
            "/home/alice2/x"
        );
        assert_eq!(
            r.path(std::path::Path::new("/home/alice-old/x")),
            "/home/alice-old/x"
        );
        assert_eq!(r.text("/opt/home/alice/data"), "/opt/home/alice/data");
        assert!(r.summary().is_empty());
    }

    #[test]
    fn free_text_rewrites_every_occurrence_and_counts_each() {
        let mut r = Redactor::new(Some(std::path::Path::new("/home/alice/")));
        let prefs = "usbids_path = \"/home/alice/usb.ids\"\n# was /home/alice/old\n";
        assert_eq!(r.text(prefs), "usbids_path = \"~/usb.ids\"\n# was ~/old\n");
        assert_eq!(r.summary(), vec![("home_path".to_string(), 2)]);

        let mut r = Redactor::new(Some(std::path::Path::new("/home/alice")));
        assert_eq!(r.text("x=/home/alice"), "x=~");
        assert_eq!(r.summary(), vec![("home_path".to_string(), 1)]);

        let mut r = Redactor::new(Some(std::path::Path::new("/home/alice")));
        assert_eq!(r.text("file:///home/alice/x"), "file://~/x");
        assert_eq!(r.summary(), vec![("home_path".to_string(), 1)]);
    }

    #[test]
    fn no_home_or_a_root_home_disables_the_path_rule() {
        let mut none = Redactor::new(None);
        assert_eq!(none.text("/home/alice/x"), "/home/alice/x");
        let mut root = Redactor::new(Some(std::path::Path::new("/")));
        assert_eq!(root.text("/etc/passwd"), "/etc/passwd");
        assert!(root.summary().is_empty());
    }

    #[test]
    fn mac_addresses_are_masked_only_when_they_stand_alone() {
        let mut r = Redactor::new(None);
        let line =
            "usb 1-3: r8152 eth0: MAC 00:1a:2b:3c:4d:5e ready; id 00:1a:2b:3c:4d:5e:ff stays";
        assert_eq!(
            r.mac_addresses(line),
            "usb 1-3: r8152 eth0: MAC xx:xx:xx:xx:xx:xx ready; id 00:1a:2b:3c:4d:5e:ff stays"
        );
        assert_eq!(r.summary(), vec![("mac_address".to_string(), 1)]);
    }

    #[test]
    fn cmdline_masks_filesystem_uuids_and_keeps_everything_else() {
        let mut r = Redactor::new(None);
        let cmd = "BOOT_IMAGE=/boot/vmlinuz root=UUID=307c1732-bacd-4ef4-9050-b4c9e99e5648 ro quiet resume=PARTUUID=abcd-1234";
        assert_eq!(
            r.cmdline(cmd),
            "BOOT_IMAGE=/boot/vmlinuz root=UUID=<redacted> ro quiet resume=PARTUUID=<redacted>"
        );
        assert_eq!(r.summary(), vec![("fs_uuid".to_string(), 2)]);
    }

    #[test]
    fn the_environment_allowlist_is_exactly_five_names() {
        for name in ["TERM", "COLORTERM", "LANG", "LC_ALL", "RUST_LOG"] {
            assert!(Redactor::env_allowlisted(name), "{name}");
        }
        for name in ["HOME", "USER", "LOGNAME", "SSH_CLIENT", "PATH", "term"] {
            assert!(!Redactor::env_allowlisted(name), "{name}");
        }
    }

    #[test]
    fn ssh_presence_is_any_marker_set_and_never_its_value() {
        assert!(Redactor::ssh_present(|n| n == "SSH_CONNECTION"));
        assert!(Redactor::ssh_present(|n| n == "SSH_TTY"));
        assert!(!Redactor::ssh_present(|_| false));
        assert!(!Redactor::ssh_present(|n| n == "SSH_AUTH_SOCK"));
    }

    #[test]
    fn summary_is_sorted_by_rule_name() {
        let mut r = Redactor::new(Some(std::path::Path::new("/home/alice")));
        r.mac_addresses("aa:bb:cc:dd:ee:ff");
        r.text("/home/alice/x");
        r.text("/media/alice/stick");
        assert_eq!(
            r.summary(),
            vec![
                ("home_path".to_string(), 1),
                ("mac_address".to_string(), 1),
                ("user_name".to_string(), 1),
            ]
        );
    }

    #[test]
    fn the_login_name_is_masked_as_a_path_component() {
        let home = std::path::Path::new("/home/alice");
        for (input, expected, summary) in [
            (
                "/media/alice/stick",
                "/media/<user>/stick",
                vec![("user_name".to_string(), 1)],
            ),
            (
                "--support /run/user/1000/alice",
                "--support /run/user/1000/<user>",
                vec![("user_name".to_string(), 1)],
            ),
            ("/home/alice2/x", "/home/alice2/x", vec![]),
            ("/opt/xalice/y", "/opt/xalice/y", vec![]),
            ("Alice's iPhone", "Alice's iPhone", vec![]),
            ("alice", "alice", vec![]),
        ] {
            let mut r = Redactor::new(Some(home));
            assert_eq!(r.text(input), expected, "{input}");
            assert_eq!(r.summary(), summary, "{input}");
        }

        // The home rule runs first: a path under home is rewritten to `~`
        // with no separate user_name count.
        let mut r = Redactor::new(Some(home));
        assert_eq!(r.text("/home/alice/x"), "~/x");
        assert_eq!(r.summary(), vec![("home_path".to_string(), 1)]);
    }
}
