//! The man page `--print-man` prints and `install.sh` installs.
//!
//! clap_mangen renders everything the command line already declares. The one
//! thing it cannot know is how the tool reads usbmon and what to do when it
//! lands on a fallback interface, which is what a fallback warning in the log
//! sends the reader here for (see `usbmon::monitor`'s `SEE_DOCS`). That
//! section is written out by hand below and spliced into the rendered page.

use std::io;

use clap::Command;

/// The heading the log's fallback warnings name. A test-only constant so
/// the warning and the page cannot drift apart unnoticed.
#[cfg(test)]
pub const CAPTURE_INTERFACES_HEADING: &str = "CAPTURE INTERFACES";

/// The CAPTURE INTERFACES section, in roff. It says the same thing as the
/// README's "Reading usbmon" section: the three interfaces best first, what
/// each fallback costs and why, and how to get back to the mmap ring.
///
/// Every `-` in a command, a path or an interface name is written `\-` so it
/// renders as an ASCII minus that survives a copy into a shell, and no text
/// line starts with `.` or `'`, which roff would read as a request.
const CAPTURE_INTERFACES: &str = r#".SH "CAPTURE INTERFACES"
usbtop\-ng reads USB traffic from the usbmon kernel module, through the best of three interfaces that works on the host.
The log names the choice at startup, as \fBusing usbmon mmap\-ring interface\fR, \fBbinary\fR or \fBtext\fR.
When the choice is not the mmap ring, a warning follows that says which fallback had to be used and points here.
.TP
\fBmmap\-ring\fR
The preferred interface, and the one to aim for.
usbtop\-ng maps the ring buffer of /dev/usbmon\fIN\fR read\-only, fetches events in batches, and copies only the 48\-byte header of each event, never its captured payload.
Its byte counts are exact.
.TP
\fBbinary\fR
The first fallback: read(2) on /dev/usbmon\fIN\fR, used when the node opens but its ring cannot be mapped.
The figures are the same as on the mmap ring.
What it adds is work: every event takes a system call of its own, which also copies its captured payload out of the kernel to be thrown away, since that is the only way past it to the next header, so a busy bus can outrun the reader, and the kernel's ring then drops events, shown as \fBkdropped\fR in the header.
.TP
\fBtext\fR
The last fallback: the debugfs text interface, /sys/kernel/debug/usb/usbmon/\fIN\fRu, used when /dev/usbmon\fIN\fR cannot be opened at all.
The kernel formats every event as a line of text and usbtop\-ng parses it back.
The text does not carry the exact size of an isochronous transfer, so the rates of webcams and audio devices are estimates, marked with a leading \fB\(ti\fR.
The kernel's own count of events it dropped, \fBkdropped\fR in the header, is not available.
.PP
The order is one of what is lost, not of CPU cost: read(2) delivers everything the mmap ring does, and text is last because it is the only one that loses data.
Measured against a webcam on two hosts, the mmap ring cost about 0.5% and 0.9% of one core, read(2) about 0.9% and 1.3%, and text about 1.0% and 1.7%.
Those figures add the reader thread's CPU to the kernel's own work on each event, which was timed separately, and they are two hosts' figures for one kind of traffic at one rate.
docs/BENCHMARK.md in the source tree has the method, the tables and what they do not show.
.SS "Getting back to the mmap ring"
.IP \(bu 2
On \fBtext\fR, the warning names the node that did not open and the error.
"No such file or directory": usbmon is loaded (\fBlsmod | grep usbmon\fR) but nothing is creating device nodes in /dev, the usual case in a container.
Pass the node in from the host with the container runtime's device option, for example \fBdocker run \-\-device /dev/usbmon0\fR; usbmon0 is enough, because it is the all\-buses node and usbtop\-ng reads it alone when it is there.
.IP \(bu 2
"Permission denied" or "Operation not permitted": the node is there but closed to this process, by its mode (\fBls \-l /dev/usbmon*\fR; readable by root alone unless a udev rule grants more) or by the container's device rules.
Run as root, \fBsudo usbtop\-ng\fR, or allow the device.
On an ordinary host a run that is not root never gets this far: with neither interface readable, usbtop\-ng stops and says to use sudo.
.IP \(bu 2
If the nodes open and the session is still on \fBbinary\fR, the kernel refused the ring: either a sandbox, a seccomp filter or a security module denies the ioctl or the mmap on the device, or the kernel is too old to have the ring.
Run usbtop\-ng outside the sandbox, or let it map /dev/usbmon\fIN\fR.
.IP \(bu 2
A warning that names one bus means that bus alone fell back, after its own node failed the same checks or failed while it was being read; the others stay on the interface chosen at startup.
Bus 0 is the exception: it is the aggregate reader that covers every bus, so a warning that names bus 0 means the whole session fell back.
.PP
debugfs is needed for the text interface and nothing else.
A host on the mmap ring or on read(2) does not need it mounted.
.PP
A script reads the interface from the \fBsource\fR field of each report: \fBmmap\fR, \fBbinary\fR, \fBtext\fR or \fBebpf\fR.
It is the state of the readers when the report is built, \fBtext\fR as soon as any bus is on it, so it follows a later fallback; it does not say which interface each counted packet came from.
The run record that \fB\-\-output\fR writes also has a \fBbackend\fR field, but it is a snapshot taken as the readers start: it can say \fBbinary\fR before the mmap reader has reported in, and it never changes afterwards.
"#;

/// The whole page: clap_mangen's rendering of `command`, with the CAPTURE
/// INTERFACES section placed after the options and before VERSION, where a
/// page's own sections conventionally go. If a future clap_mangen stops
/// emitting a VERSION section the same way, the section goes at the end
/// instead of going missing.
pub fn render(command: Command) -> io::Result<Vec<u8>> {
    let mut rendered = Vec::new();
    clap_mangen::Man::new(command).render(&mut rendered)?;
    let page =
        String::from_utf8(rendered).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let spliced = match page.find("\n.SH VERSION\n") {
        Some(at) => {
            // Keep the newline that ends the options section, then the new
            // section, then VERSION on a line of its own as before.
            let (before, after) = page.split_at(at + 1);
            format!("{before}{CAPTURE_INTERFACES}{after}")
        }
        None if page.ends_with('\n') => format!("{page}{CAPTURE_INTERFACES}"),
        None => format!("{page}\n{CAPTURE_INTERFACES}"),
    };
    Ok(spliced.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in command: `render` only needs something clap_mangen can
    /// render, and the real `Cli` lives in `main`.
    fn command() -> Command {
        Command::new("usbtop-ng")
            .version("0.0.0")
            .author("someone")
            .about("about")
            .arg(clap::Arg::new("once").long("once"))
    }

    fn page() -> String {
        String::from_utf8(render(command()).unwrap()).unwrap()
    }

    #[test]
    fn the_section_sits_between_the_options_and_the_version() {
        let page = page();
        let position = |heading: &str| {
            page.find(heading)
                .unwrap_or_else(|| panic!("{heading} is missing from:\n{page}"))
        };
        let options = position("\n.SH OPTIONS\n");
        let capture = position("\n.SH \"CAPTURE INTERFACES\"\n");
        let version = position("\n.SH VERSION\n");
        let authors = position("\n.SH AUTHORS\n");
        assert!(options < capture, "after the options");
        assert!(capture < version, "before the version");
        assert!(version < authors, "the tail of the page is unchanged");
        assert_eq!(page.matches(".SH \"CAPTURE INTERFACES\"").count(), 1);
    }

    #[test]
    fn a_page_without_a_version_section_still_carries_the_section() {
        let command = Command::new("usbtop-ng").about("about");
        let page = String::from_utf8(render(command).unwrap()).unwrap();
        assert!(!page.contains(".SH VERSION"), "{page}");
        assert!(
            page.ends_with(&format!("\n{CAPTURE_INTERFACES}")),
            "with no VERSION to sit before, the section ends the page, on a line of its own:\n{page}"
        );
    }

    #[test]
    fn the_heading_is_the_one_the_log_warnings_name() {
        assert!(CAPTURE_INTERFACES.starts_with(&format!(".SH \"{CAPTURE_INTERFACES_HEADING}\"\n")));
    }

    /// The three names the startup log line prints (see
    /// `usbmon::monitor`'s `SourceKind::label`) each have an entry, written
    /// the way roff needs them.
    #[test]
    fn every_interface_the_log_names_has_an_entry() {
        for entry in [
            ".TP\n\\fBmmap\\-ring\\fR\n",
            ".TP\n\\fBbinary\\fR\n",
            ".TP\n\\fBtext\\fR\n",
        ] {
            assert!(CAPTURE_INTERFACES.contains(entry), "missing {entry:?}");
        }
    }

    #[test]
    fn the_section_says_how_to_fix_each_fallback() {
        assert!(CAPTURE_INTERFACES.contains(".SS \"Getting back to the mmap ring\"\n"));
        assert!(CAPTURE_INTERFACES.contains("\\fBsudo usbtop\\-ng\\fR"));
        assert!(CAPTURE_INTERFACES.contains("\\fBls \\-l /dev/usbmon*\\fR"));
        assert!(CAPTURE_INTERFACES.contains("\\fBdocker run \\-\\-device /dev/usbmon0\\fR"));
        // The one CPU statement is the measured one, with what it adds up
        // and a pointer to the benchmark; no unqualified ranking beside it.
        assert!(CAPTURE_INTERFACES.contains(
            "Measured against a webcam on two hosts, the mmap ring cost about 0.5% and 0.9% \
             of one core, read(2) about 0.9% and 1.3%, and text about 1.0% and 1.7%."
        ));
        assert!(CAPTURE_INTERFACES
            .contains("add the reader thread's CPU to the kernel's own work on each event"));
        assert!(CAPTURE_INTERFACES.contains("docs/BENCHMARK.md in the source tree"));
        assert!(!CAPTURE_INTERFACES.contains("most CPU"));
        // The read() fallback has a remedy of its own, not the text one's.
        assert!(CAPTURE_INTERFACES.contains("a seccomp filter or a security module"));
        assert!(CAPTURE_INTERFACES.contains("let it map /dev/usbmon\\fIN\\fR"));
    }

    /// A text line that starts with `.` or `'` is a roff request, and a bare
    /// `-` renders as a typographic hyphen that breaks a pasted command. Both
    /// are easy to introduce when editing prose, so both are checked.
    #[test]
    fn the_roff_has_no_accidental_requests_or_bare_hyphens() {
        const REQUESTS: [&str; 5] = [".SH", ".SS", ".TP", ".IP", ".PP"];
        for line in CAPTURE_INTERFACES.lines() {
            assert!(
                !line.is_empty(),
                "a blank line is a paragraph break in roff"
            );
            assert!(!line.starts_with('\''), "{line:?}");
            if line.starts_with('.') {
                // The request is the whole first word: `.SHfoo` is not `.SH`.
                let request = line.split(' ').next().unwrap_or(line);
                assert!(
                    REQUESTS.contains(&request),
                    "{line:?} starts with a dot but is not a known request"
                );
            }
            let unescaped = line.replace("\\-", "");
            assert!(!unescaped.contains('-'), "bare hyphen in {line:?}");
        }
    }
}
