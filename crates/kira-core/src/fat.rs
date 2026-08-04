//! Names the watch's FAT volume cannot hold, or that a desktop resolves to
//! something other than a file.
//!
//! The watch presents a FAT mass-storage volume, and Kira writes to it through
//! whichever operating system the page happens to be running on. So a name has to
//! survive two things: FAT itself, and the host's own ideas about what a file name
//! means. Windows has the stronger opinions of the two, and they are the ones that
//! turn a validated name into a different name.
//!
//! Kept here rather than beside either caller because both the on-device folder a
//! submission claims and the settings file it declares are checked against it, and
//! they had drifted apart -- folders refused a device name while file names did
//! not.

/// Names MS-DOS reserved. Windows still resolves them to devices rather than
/// files, whatever directory they appear in and whatever extension they carry.
///
/// The full set, not the ports a machine actually has: the resolution is in the
/// name, not the hardware.
const DEVICES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Whether Windows would resolve this name to a device instead of a file.
///
/// Tests the stem rather than the whole name, which is the part that catches
/// `nul.json`: an extension does not stop the resolution, so a file "safely"
/// named after a device is still a write that goes nowhere.
#[must_use]
pub fn is_reserved_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    DEVICES.iter().any(|d| d.eq_ignore_ascii_case(stem))
}

/// Whether a host would strip characters off the end of this name.
///
/// Windows discards trailing dots and spaces when it creates a file, so a name
/// ending in either is a request to create a *different* name than the one that
/// was checked. That gap is what lets `evil.uapp.` pass a rule about `.uapp` and
/// then land as `evil.uapp` -- a file the watch may try to boot.
#[must_use]
pub fn is_trimmed_by_host(name: &str) -> bool {
    name.ends_with('.') || name.ends_with(' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_name_is_recognised_whatever_it_wears() {
        // An extension does not stop Windows resolving these to a device, so the
        // stem is what matters. `nul.json` was accepted before this existed.
        for name in [
            "NUL", "nul", "CON", "aux", "COM1", "LPT9", "nul.json", "CoM3.txt",
        ] {
            assert!(is_reserved_device(name), "{name} should be a device");
        }
    }

    #[test]
    fn an_ordinary_name_is_not_a_device() {
        for name in [
            "input.json",
            "settings.json",
            "console.json",
            "nullable.json",
            "com.json",
            "com10",
            "lpt0",
        ] {
            assert!(!is_reserved_device(name), "{name} should be usable");
        }
    }

    #[test]
    fn a_name_the_host_would_shorten_is_recognised() {
        // The whole point: what gets checked has to be what gets created.
        for name in ["evil.uapp.", "evil.uapp ", "input.json.", "x."] {
            assert!(is_trimmed_by_host(name), "{name} would be trimmed");
        }
        for name in ["input.json", "evil.uapp", "a.b.c"] {
            assert!(!is_trimmed_by_host(name), "{name} survives as written");
        }
    }
}
