//! Keeping the network alive across a reader handoff.
//!
//! Stopping and restarting the stock reader reliably drops the Wi-Fi
//! connection. The reader owns the radio, and the restarted one begins from its
//! own "not connected" state, so the association and the lease are simply gone.
//! On a device managed over Wi-Fi that means every handoff can cost the
//! connection used to run it, which was measured rather than assumed: the
//! device became unreachable after a session and only came back when its owner
//! tapped through the reader's own network UI.
//!
//! There is no supported way to ask the reader to reconnect. It exposes no
//! D-Bus service; the session bus carries only `Fontickel`, `Sickel` and the
//! bus itself. `/tmp/nickel-hardware-status` is one-way reporting rather than
//! control: the stock scripts write `network <action> ip=…` into it to say what
//! has already happened. There is no wifi script to call either, because
//! `libnickel` drives the radio internally.
//!
//! What is left is to put back exactly what was running. This module records
//! the supplicant and DHCP client while they are still alive, and starts those
//! same programs again, with their own arguments and environment, if the
//! connection has not returned on its own. Nothing here invents a
//! configuration, chooses a network, or writes to persistent storage, and
//! everything it does is undone by a reboot.
//!
//! Deliberately not done: writing to `/tmp/nickel-hardware-status` to correct
//! the reader's indicator. That FIFO blocks until something reads it, which is
//! why the stock scripts background every write to it, and a cosmetic icon is
//! not worth a runtime that can hang.

use crate::reader::{read_argv, Reader, ReaderError};
use std::fs;
use std::path::Path;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

/// The supplicant that owns the wireless association.
pub const SUPPLICANT_EXECUTABLE: &str = "/bin/wpa_supplicant";

/// The DHCP client that owns the address and the default route.
pub const DHCP_EXECUTABLE: &str = "/sbin/dhcpcd";

/// Where the kernel lists the interfaces it has.
pub(crate) const NET_CLASS: &str = "/sys/class/net";

/// The interface the device connects with, or `None` on a device with no radio.
///
/// Detected, never named: the name belongs to the driver rather than to Kobo.
/// A Clara BW enumerates its station as `wlan0`, a Clara 2E's Marvell part as
/// `mlan0`, and the constant this replaced said `wlan0` for every device. On an
/// `mlan0` reader that one word made [`crate::wifi::Wifi::open`] find nothing,
/// so the daemon advertised no Wi-Fi capability and answered every Wi-Fi
/// request "unsupported" while the reader was online, and it made this module
/// watch an interface that does not exist for a connection it could never see
/// come back. Anything wanting the name asks here.
///
/// Detection is done once and kept, because the answer cannot change without a
/// reboot and the status band asks every couple of seconds.
#[must_use]
pub fn wireless_link() -> Option<&'static str> {
    static LINK: OnceLock<Option<String>> = OnceLock::new();
    LINK.get_or_init(|| {
        detect_wireless_link(
            Path::new("/proc"),
            Path::new(NET_CLASS),
            &fs::read_to_string(WIRELESS).unwrap_or_default(),
        )
    })
    .as_deref()
}

/// Picks the station interface, preferring the running supplicant's answer.
///
/// The supplicant is authoritative: it is the single owner of the association
/// this module and [`crate::wifi`] both work through, so the interface it was
/// given with `-i` is the interface they must use. Its answer is still held to
/// the same two conditions as a scanned one — a station, and an interface the
/// kernel has — so a supplicant left over from a radio that is gone falls
/// through to the scan rather than naming something absent.
///
/// Scanning is for a device whose radio is present without a supplicant yet.
/// Either way the companion interfaces one radio also publishes have to be left
/// out: `p2p0` is Wi-Fi Direct and `uap0` the soft AP, so joining a network on
/// one, or taking one down, would act on something that is not the station.
/// They appear in `/proc/net/wireless` beside the station, which is why being
/// wireless is necessary evidence but not sufficient.
fn detect_wireless_link(proc_root: &Path, net_class: &Path, wireless: &str) -> Option<String> {
    supplicant_link(proc_root)
        .filter(|link| is_station_role(link) && net_class.join(link).is_dir())
        .or_else(|| station_link(net_class, wireless))
}

/// Reads the interface from the `-i` argument of a running `wpa_supplicant`.
///
/// Recognised by the file name of the zeroth argument rather than by a full
/// path, because which directory a firmware keeps the supplicant in is a
/// per-model detail, and being wrong about it here would silently fall through
/// to the scan.
fn supplicant_link(proc_root: &Path) -> Option<String> {
    let mut pids = fs::read_dir(proc_root)
        .ok()?
        .filter_map(|entry| {
            let name = entry.ok()?.file_name();
            name.to_str()?.parse::<i32>().ok()
        })
        .collect::<Vec<_>>();
    // Lowest pid first, so a device with a second supplicant answers the same
    // way twice rather than however the directory happened to be ordered.
    pids.sort_unstable();
    pids.into_iter()
        .filter_map(|pid| read_argv(proc_root, pid))
        .filter(|argv| argv.first().is_some_and(|zeroth| is_supplicant(zeroth)))
        .find_map(|argv| interface_argument(&argv))
}

fn is_supplicant(zeroth: &str) -> bool {
    Path::new(zeroth)
        .file_name()
        .is_some_and(|name| name == "wpa_supplicant")
}

/// Reads both spellings the supplicant accepts: `-i mlan0` and `-imlan0`.
fn interface_argument(argv: &[String]) -> Option<String> {
    let mut arguments = argv.iter().skip(1);
    while let Some(argument) = arguments.next() {
        let link = if argument == "-i" {
            arguments.next()?.as_str()
        } else if let Some(link) = argument.strip_prefix("-i") {
            link
        } else {
            continue;
        };
        if !link.is_empty() && !link.contains('/') {
            return Some(link.to_owned());
        }
    }
    None
}

/// The lowest-named station interface the kernel has, if it has one.
fn station_link(net_class: &Path, wireless: &str) -> Option<String> {
    let mut stations = fs::read_dir(net_class)
        .ok()?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| is_station_role(name))
        .filter(|name| is_wireless(net_class, name, wireless))
        .collect::<Vec<_>>();
    stations.sort_unstable();
    stations.into_iter().next()
}

/// Interfaces of the same radio that are not the station: Wi-Fi Direct and the
/// soft AP.
const COMPANION_PREFIXES: [&str; 2] = ["p2p", "uap"];

fn is_station_role(name: &str) -> bool {
    !COMPANION_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// Whether the driver calls `name` wireless, in any of the ways it might say
/// so: `cfg80211` drivers publish `phy80211`, wireless-extensions ones a
/// `wireless` directory, and a vendor driver that publishes neither still
/// appears in `/proc/net/wireless`.
fn is_wireless(net_class: &Path, name: &str, wireless: &str) -> bool {
    let interface = net_class.join(name);
    interface.join("phy80211").exists()
        || interface.join("wireless").exists()
        || lists_interface(wireless, name)
}

/// Whether `/proc/net/wireless` has a row for `name`.
fn lists_interface(table: &str, name: &str) -> bool {
    let wanted = format!("{name}:");
    table
        .lines()
        .skip(2)
        .any(|line| line.split_whitespace().next() == Some(wanted.as_str()))
}

const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How long to keep watching before concluding the connection survived.
///
/// The restarted reader takes the link down some seconds after it starts, not
/// while it is starting. Checking once on the way past therefore reads the
/// routing table while the old default route is still in it and concludes,
/// wrongly, that nothing needs doing. Measured on a Clara BW: the summary said
/// the connection was unaffected, and the device was unreachable moments later.
const SETTLE: Duration = Duration::from_secs(12);

/// The state of the connection after an attempt to restore it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Restored {
    /// The connection was still up and nothing was started.
    Unaffected,
    /// Daemons were started again and the connection came back.
    Restarted,
    /// The connection did not come back within the time allowed.
    ///
    /// This is reported rather than raised as an error, because a session that
    /// has already put the reader back has succeeded at the thing that matters;
    /// the owner can reconnect by hand exactly as before.
    StillDown,
}

/// The connection as it was before the reader was stopped.
#[derive(Debug, Default)]
pub struct Connection {
    /// In start order: the association has to exist before a lease can.
    daemons: Vec<Reader>,
    /// The interface as it was found, so what is watched afterwards is the one
    /// that was carrying the connection.
    link: Option<&'static str>,
    /// Whether there was a connection to lose in the first place.
    was_online: bool,
}

impl Connection {
    /// Records the networking daemons that are currently running.
    ///
    /// This never fails. A daemon that is not running is one this module will
    /// not try to restore, which is the correct behaviour for a device that was
    /// already offline when the session began.
    #[must_use]
    pub fn capture() -> Self {
        let daemons = [SUPPLICANT_EXECUTABLE, DHCP_EXECUTABLE]
            .into_iter()
            .filter_map(|executable| Reader::find_running(executable).ok())
            .collect();
        let link = wireless_link();
        Self {
            daemons,
            link,
            was_online: link.is_some_and(is_online),
        }
    }

    /// Returns whether anything was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.daemons.is_empty()
    }

    /// Puts the connection back if it has gone, waiting up to `within`.
    ///
    /// # Errors
    ///
    /// Returns an error only when a recorded daemon could not be started at
    /// all. Failing to reach the network in time is reported as
    /// [`Restored::StillDown`].
    pub fn restore(&self, within: Duration) -> Result<Restored, ReaderError> {
        // A device that was already offline has nothing to put back, and
        // starting a supplicant it was not running would be inventing state.
        let Some(link) = self.link.filter(|_| self.was_online) else {
            return Ok(Restored::Unaffected);
        };
        if !went_offline(link, SETTLE) {
            return Ok(Restored::Unaffected);
        }
        for daemon in &self.daemons {
            // Starting a second copy of a daemon that is already running would
            // leave two of them fighting over one interface, which is worse
            // than the problem being fixed.
            if Reader::find_running(daemon.executable()).is_ok() {
                continue;
            }
            daemon.start(within)?;
        }
        Ok(if wait_until_online(link, within) {
            Restored::Restarted
        } else {
            Restored::StillDown
        })
    }
}

/// Returns whether `link` currently has a default route.
///
/// A default route is used rather than the presence of an address because it is
/// what actually decides whether the device can be reached, and because it is a
/// plain file read that needs no socket and no `unsafe`.
#[must_use]
pub fn is_online(link: &str) -> bool {
    fs::read_to_string("/proc/net/route").is_ok_and(|table| has_default_route(&table, link))
}

/// Watches `link` for `settle`, returning whether it was ever seen offline.
///
/// Returning early on the first offline reading keeps the common case cheap;
/// only a connection that genuinely survives costs the full wait.
fn went_offline(link: &str, settle: Duration) -> bool {
    let deadline = Instant::now() + settle;
    loop {
        if !is_online(link) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_until_online(link: &str, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if is_online(link) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Parses the kernel's routing table for a default route on `link`.
///
/// The destination column is a hexadecimal address in the host's byte order, so
/// the default route is the all-zero one.
fn has_default_route(table: &str, link: &str) -> bool {
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut columns = line.split_whitespace();
            Some((columns.next()?, columns.next()?))
        })
        .any(|(interface, destination)| {
            interface == link && destination.chars().all(|digit| digit == '0')
        })
}

/// Returns whether the kernel reports a carrier on `link`.
///
/// Used for reporting rather than for decisions: a link can be up with no
/// address, which is not a usable connection.
#[must_use]
pub fn has_carrier(link: &str) -> bool {
    fs::read_to_string(Path::new("/sys/class/net").join(link).join("operstate"))
        .is_ok_and(|state| state.trim() == "up")
}

/// Where the kernel publishes wireless link quality.
const WIRELESS: &str = "/proc/net/wireless";

/// Reads the signal level on `link`, in dBm.
///
/// Read-only, like everything else in this module that reports rather than
/// changes: a text file the kernel publishes, no socket, no ioctl and no
/// `unsafe`. Returns `None` when the interface is not wireless, is not
/// associated, or the device has no wireless stack at all, all of which are
/// legitimately "no signal to report" rather than "a signal of zero".
#[must_use]
pub fn signal_dbm(link: &str) -> Option<i32> {
    signal_dbm_in(&fs::read_to_string(WIRELESS).ok()?, link)
}

/// The same, against arbitrary contents, so the parsing is testable on a
/// machine with no radio.
///
/// The file has two header lines and then one row per interface:
///
/// ```text
/// Inter-| sta-|   Quality        |   Discarded packets ...
///  face | tus | link level noise |  nwid  crypt  frag ...
///  wlan0: 0000   54.  -56.  -256        0      0     0 ...
/// ```
///
/// The level column carries a trailing full stop, and some drivers report it
/// as an unsigned byte biased by 256 rather than as a negative number. Both
/// spellings are accepted, because which one a device uses is a property of
/// its driver and not something worth having a different build for.
#[must_use]
pub fn signal_dbm_in(table: &str, link: &str) -> Option<i32> {
    let wanted = format!("{link}:");
    for line in table.lines().skip(2) {
        let mut columns = line.split_whitespace();
        let interface = columns.next()?;
        if interface != wanted {
            continue;
        }
        // status, quality, then level.
        let level = columns.nth(2)?.trim_end_matches('.');
        let level: i32 = level.parse().ok()?;
        // A level above zero is the biased spelling. Real Wi-Fi is never
        // stronger than about -20 dBm, so there is no ambiguity to resolve.
        return Some(if level > 0 { level - 256 } else { level });
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_signal_is_read_from_the_row_for_the_interface_asked_for() {
        let table = "Inter-| sta-|   Quality        |   Discarded packets\n \
                     face | tus | link level noise |  nwid  crypt   frag\n \
                     lo: 0000    0.    0.    0        0      0      0\n \
                     wlan0: 0000   54.  -56.  -256        0      0      0\n";
        assert_eq!(signal_dbm_in(table, "wlan0"), Some(-56));
        assert_eq!(
            signal_dbm_in(table, "eth0"),
            None,
            "an interface that is not in the table reported a signal anyway"
        );
    }

    #[test]
    fn a_driver_that_biases_the_level_by_a_byte_is_understood() {
        // Some drivers report the level as an unsigned byte offset by 256
        // rather than as a negative number. Read literally that is a signal
        // stronger than any radio produces, which would pin the mark to full
        // on a device that is barely associated.
        let table = "header\nheader\n wlan0: 0000   54.  200.  -256        0\n";
        assert_eq!(signal_dbm_in(table, "wlan0"), Some(-56));
    }

    #[test]
    fn no_wireless_stack_reports_nothing_rather_than_silence() {
        // "" is a device with no radio; the mark for that is different in
        // shape from a weak one, so the distinction has to survive the read.
        assert_eq!(signal_dbm_in("", "wlan0"), None);
        assert_eq!(signal_dbm_in("only\nheaders\n", "wlan0"), None);
    }

    use super::{
        detect_wireless_link, has_default_route, interface_argument, signal_dbm_in, Connection,
    };
    use std::fs;
    use std::path::PathBuf;

    /// `/proc/net/wireless` from a Clara 2E, whose Marvell part calls its
    /// station `mlan0`. The station row is verbatim from the device; the two
    /// companion rows are the idle ones it lists beside it.
    const MARVELL_WIRELESS: &str =
        "Inter-| sta-|   Quality        |   Discarded packets               | Missed | WE\n \
         face | tus | link level noise |  nwid  crypt   frag  retry   misc | beacon | 22\n \
         mlan0: 0002    5.  -34.  -88.        0      0      0      0      0        0\n \
         uap0: 0000    0.    0.    0.         0      0      0      0      0        0\n \
         p2p0: 0000    0.    0.    0.         0      0      0      0      0        0\n";

    #[test]
    fn a_station_that_is_not_named_wlan0_still_reports_its_signal() {
        assert_eq!(signal_dbm_in(MARVELL_WIRELESS, "mlan0"), Some(-34));
        assert_eq!(
            signal_dbm_in(MARVELL_WIRELESS, "wlan0"),
            None,
            "asking for a name this device does not use showed no signal at -34 dBm"
        );
    }

    #[test]
    fn the_companion_interfaces_are_listed_beside_the_station() {
        // Wi-Fi Direct and the soft AP have rows of their own, which is why
        // this table cannot be what decides which interface is the station.
        assert_eq!(signal_dbm_in(MARVELL_WIRELESS, "p2p0"), Some(0));
        assert_eq!(signal_dbm_in(MARVELL_WIRELESS, "uap0"), Some(0));
    }

    /// Builds a fake `/sys/class/net`, marking the wireless interfaces the way
    /// a `cfg80211` driver does.
    fn fake_net_class(label: &str, interfaces: &[(&str, bool)]) -> PathBuf {
        // Tests run in parallel, so the label keeps each test's fake sysfs
        // separate. A shape-derived name silently collides.
        let root =
            std::env::temp_dir().join(format!("kobo-netclass-test-{}-{label}", std::process::id()));
        let _ignored = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create fake net class");
        for (name, wireless) in interfaces {
            let interface = root.join(name);
            fs::create_dir_all(&interface).expect("create interface");
            if *wireless {
                fs::create_dir_all(interface.join("phy80211")).expect("create phy80211");
            }
        }
        root
    }

    /// Builds a fake `/proc` holding one process with `argv`, or none at all.
    fn fake_proc(label: &str, argv: &[&str]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("kobo-netproc-test-{}-{label}", std::process::id()));
        let _ignored = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create fake proc");
        if argv.is_empty() {
            return root;
        }
        let directory = root.join("401");
        fs::create_dir_all(&directory).expect("create pid directory");
        let mut cmdline = Vec::new();
        for argument in argv {
            cmdline.extend_from_slice(argument.as_bytes());
            cmdline.push(0);
        }
        fs::write(directory.join("cmdline"), cmdline).expect("write cmdline");
        root
    }

    /// The supplicant as the Clara 2E runs it, argument for argument.
    const MARVELL_SUPPLICANT: [&str; 11] = [
        "wpa_supplicant",
        "-D",
        "nl80211",
        "-s",
        "-i",
        "mlan0",
        "-c",
        "/etc/wpa_supplicant/wpa_supplicant.conf",
        "-C",
        "/var/run/wpa_supplicant",
        "-B",
    ];

    #[test]
    fn the_running_supplicant_names_the_interface() {
        let proc_root = fake_proc("supplicant", &MARVELL_SUPPLICANT);
        let net_class = fake_net_class(
            "supplicant",
            &[
                ("lo", false),
                ("mlan0", true),
                ("p2p0", true),
                ("uap0", true),
            ],
        );
        assert_eq!(
            detect_wireless_link(&proc_root, &net_class, MARVELL_WIRELESS).as_deref(),
            Some("mlan0")
        );
    }

    #[test]
    fn the_station_is_found_with_no_supplicant_running() {
        // Nothing to ask, so the interfaces themselves have to answer, and
        // both companions have to be left out even though the kernel's
        // wireless table lists all three.
        let net_class = fake_net_class(
            "scan",
            &[
                ("lo", false),
                ("mlan0", true),
                ("p2p0", true),
                ("uap0", true),
            ],
        );
        assert_eq!(
            detect_wireless_link(&fake_proc("scan", &[]), &net_class, MARVELL_WIRELESS).as_deref(),
            Some("mlan0")
        );
    }

    #[test]
    fn a_vendor_driver_is_wireless_by_the_kernels_table_alone() {
        // No `phy80211` and no `wireless` directory, which an out-of-tree
        // driver need not publish. Refusing it would report no Wi-Fi on a
        // device that has it.
        let net_class = fake_net_class("vendor", &[("lo", false), ("mlan0", false)]);
        assert_eq!(
            detect_wireless_link(&fake_proc("vendor", &[]), &net_class, MARVELL_WIRELESS)
                .as_deref(),
            Some("mlan0")
        );
    }

    #[test]
    fn a_wlan0_device_is_unaffected() {
        let net_class = fake_net_class("wlan", &[("lo", false), ("usb0", false), ("wlan0", true)]);
        assert_eq!(
            detect_wireless_link(&fake_proc("wlan", &[]), &net_class, "").as_deref(),
            Some("wlan0")
        );
    }

    #[test]
    fn a_device_with_no_radio_names_nothing() {
        // The caller registers the Wi-Fi capability from this answer, so
        // guessing a name here would advertise a backend that cannot work.
        let net_class = fake_net_class("wired", &[("lo", false), ("usb0", false)]);
        assert_eq!(
            detect_wireless_link(&fake_proc("wired", &[]), &net_class, ""),
            None
        );
    }

    #[test]
    fn an_interface_the_supplicant_names_but_the_kernel_lacks_is_refused() {
        // A supplicant left over from a radio that is gone must not outvote
        // the interfaces the kernel actually has.
        let net_class = fake_net_class("stale", &[("lo", false), ("mlan0", true)]);
        assert_eq!(
            detect_wireless_link(
                &fake_proc("stale", &["wpa_supplicant", "-i", "wlan0"]),
                &net_class,
                MARVELL_WIRELESS
            )
            .as_deref(),
            Some("mlan0")
        );
    }

    #[test]
    fn a_supplicant_on_a_companion_interface_does_not_name_the_station() {
        // Wi-Fi Direct has a supplicant of its own on some firmwares. Taking
        // its interface would point every join and every link change at the
        // wrong half of the radio.
        let net_class = fake_net_class("companion", &[("mlan0", true), ("p2p0", true)]);
        assert_eq!(
            detect_wireless_link(
                &fake_proc("companion", &["wpa_supplicant", "-i", "p2p0"]),
                &net_class,
                MARVELL_WIRELESS
            )
            .as_deref(),
            Some("mlan0")
        );
    }

    #[test]
    fn both_spellings_of_the_interface_flag_are_read() {
        let joined = ["wpa_supplicant".to_owned(), "-imlan0".to_owned()];
        assert_eq!(interface_argument(&joined).as_deref(), Some("mlan0"));
        let separate = MARVELL_SUPPLICANT.map(str::to_owned);
        assert_eq!(interface_argument(&separate).as_deref(), Some("mlan0"));
        let none = ["wpa_supplicant".to_owned(), "-B".to_owned()];
        assert_eq!(interface_argument(&none), None);
    }

    /// Taken verbatim from the device, header row included.
    const REAL_TABLE: &str =
        "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
wlan0\t00000000\t0101A8C0\t0003\t0\t0\t312\t00000000\t0\t0\t0\n\
wlan0\t0001A8C0\t00000000\t0001\t0\t0\t312\t00FFFFFF\t0\t0\t0\n";

    /// The same reader as [`MARVELL_WIRELESS`], online over `mlan0`: its
    /// `default via 192.168.100.1 dev mlan0 metric 335`, in the kernel's own
    /// hexadecimal spelling.
    const MARVELL_ROUTE: &str =
        "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
mlan0\t00000000\t0164A8C0\t0003\t0\t0\t335\t00000000\t0\t0\t0\n\
mlan0\t0064A8C0\t00000000\t0001\t0\t0\t335\t00FFFFFF\t0\t0\t0\n";

    #[test]
    fn the_real_routing_table_reads_as_online() {
        assert!(has_default_route(REAL_TABLE, "wlan0"));
    }

    #[test]
    fn a_reader_online_over_another_interface_reads_as_online() {
        assert!(has_default_route(MARVELL_ROUTE, "mlan0"));
        assert!(
            !has_default_route(MARVELL_ROUTE, "wlan0"),
            "watching a name this device does not use reads as offline while it is online"
        );
    }

    #[test]
    fn a_subnet_route_alone_is_not_a_connection() {
        let table = "Iface\tDestination\tGateway\n\
wlan0\t0001A8C0\t00000000\n";
        assert!(
            !has_default_route(table, "wlan0"),
            "an interface with only a local route cannot reach anything"
        );
    }

    #[test]
    fn another_interfaces_default_route_does_not_count() {
        let table = "Iface\tDestination\tGateway\n\
usb0\t00000000\t0101A8C0\n";
        assert!(!has_default_route(table, "wlan0"));
    }

    #[test]
    fn an_empty_table_reads_as_offline() {
        assert!(!has_default_route("Iface\tDestination\tGateway\n", "wlan0"));
    }

    #[test]
    fn the_header_row_is_never_mistaken_for_a_route() {
        // "Destination" contains no digits at all, so a careless all-zero test
        // over an empty iterator would accept it.
        let table = "Iface\tDestination\tGateway\n\
wlan0\tDestination\tGateway\n";
        assert!(!has_default_route(table, "wlan0"));
    }

    #[test]
    fn capturing_on_a_host_without_those_daemons_records_nothing() {
        assert!(
            Connection::capture().is_empty(),
            "capture must never fail when the daemons are absent"
        );
    }
}

#[cfg(test)]
mod race_tests {
    use super::{Connection, Restored};
    use std::time::Duration;

    /// The defect this module was rewritten for.
    ///
    /// The first on-device run reported the connection as unaffected and then
    /// went unreachable, because the reader takes the link down after it has
    /// started rather than while it is starting. A connection that was never up
    /// must still be left alone, which is what this pins.
    #[test]
    fn a_device_that_was_offline_has_nothing_to_restore() {
        let connection = Connection::default();
        assert!(!connection.was_online);
        let outcome = connection
            .restore(Duration::from_secs(1))
            .expect("restoring nothing cannot fail");
        assert_eq!(outcome, Restored::Unaffected);
    }

    /// `restore` must not start daemons it never recorded.
    #[test]
    fn an_empty_capture_starts_nothing() {
        assert!(Connection::default().is_empty());
    }
}
