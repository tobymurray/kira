//! The UNA Watch `.uapp` container.
//!
//! Layout (little-endian), per `app_merging.py` in the UNA SDK:
//!
//! ```text
//! offset  size  field
//! 0       8     AppID          u64
//! 8       4     AppVersion     u32  0x00AABBCC = A.B.C
//! 12      4     LibCVersion    u32  ABI the app was linked against
//! 16      4     serviceLen     u32  bytes of the service image
//! 20      4     flags          u32  type in bits 0-1, see `Flags`
//! 24      16    AppName        char[16], NUL-padded, max 15 chars
//! 40      4     normalIconLen  u32  60x60 ABGR2222 = 3600
//! 44      4     smallIconLen   u32  30x30 ABGR2222 = 900
//! 48            normal icon, small icon, service image, GUI image
//! len-4   4     CRC32          u32  over everything preceding it
//! ```
//!
//! The GUI image is absent for Glance apps, so its length is whatever remains
//! between the service image and the CRC footer.

use std::fmt;
use std::str::FromStr;

use bitflags::bitflags;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Length of the fixed header.
pub const HEADER_LEN: usize = 48;
/// Length of the trailing CRC-32 footer.
pub const CRC_LEN: usize = 4;
/// A 60x60 ABGR2222 icon, one byte per pixel.
pub const NORMAL_ICON_LEN: usize = 60 * 60;
/// A 30x30 ABGR2222 icon, one byte per pixel.
pub const SMALL_ICON_LEN: usize = 30 * 30;

/// Everything that can go wrong reading a `.uapp`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Fewer bytes than the fixed header needs.
    #[error("too short for a .uapp header: {got} < {HEADER_LEN}")]
    TooShort {
        /// Bytes actually available.
        got: usize,
    },
    /// The header's own lengths exceed the file.
    #[error(
        "header describes {declared} bytes but the file is {actual}: not a .uapp, or truncated"
    )]
    Truncated {
        /// Bytes the header accounts for.
        declared: usize,
        /// Bytes actually present.
        actual: usize,
    },
    /// The name field is not valid UTF-8.
    #[error("app name is not valid UTF-8")]
    NameNotUtf8,
}

/// A 64-bit application identity, rendered as 16 uppercase hex digits.
///
/// This is the only stable identity an app has: folder names are arbitrary and
/// display names are neither unique nor path-safe.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AppId(u64);

impl AppId {
    /// Wrap a raw identifier.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for AppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016X}", self.0)
    }
}

impl fmt::Debug for AppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AppId({self})")
    }
}

/// Failure to parse an [`AppId`] from text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("AppID must be exactly 16 hex digits")]
pub struct ParseAppIdError;

impl FromStr for AppId {
    type Err = ParseAppIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 16 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ParseAppIdError);
        }
        u64::from_str_radix(s, 16)
            .map(Self)
            .map_err(|_| ParseAppIdError)
    }
}

impl Serialize for AppId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for AppId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse().map_err(de::Error::custom)
    }
}

/// A three-component version packed into `0x00AABBCC`.
///
/// Ordering is derived from the packed representation, which orders correctly
/// because each component occupies its own byte.
///
/// Note that these are *release* versions, not per-app ones: `una-version.sh`
/// stamps every app in a release with its `apps-v*` tag, so equal versions do
/// not imply equal code.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Version(u32);

impl Version {
    /// Build from components. Each must fit in a byte.
    #[must_use]
    pub const fn new(major: u8, minor: u8, patch: u8) -> Self {
        Self(((major as u32) << 16) | ((minor as u32) << 8) | patch as u32)
    }

    /// Wrap the packed representation as stored in the header.
    #[must_use]
    pub const fn from_packed(packed: u32) -> Self {
        Self(packed)
    }

    /// The packed representation.
    #[must_use]
    pub const fn packed(self) -> u32 {
        self.0
    }

    /// Major component.
    #[must_use]
    pub const fn major(self) -> u8 {
        (self.0 >> 16) as u8
    }

    /// Minor component.
    #[must_use]
    pub const fn minor(self) -> u8 {
        (self.0 >> 8) as u8
    }

    /// Patch component.
    #[must_use]
    pub const fn patch(self) -> u8 {
        self.0 as u8
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major(), self.minor(), self.patch())
    }
}

impl fmt::Debug for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Version({self})")
    }
}

/// Failure to parse a [`Version`] from text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("version must start with A.B.C, each component 0-255")]
pub struct ParseVersionError;

impl FromStr for Version {
    type Err = ParseVersionError;

    /// Parses a leading `A.B.C`, tolerating a `v` prefix and any suffix, so tags
    /// like `v0.1.9-rc3` work — upstream publishes those as full releases.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim().trim_start_matches(['v', 'V']);
        let mut parts = s.splitn(3, '.');
        let major = parts.next().ok_or(ParseVersionError)?;
        let minor = parts.next().ok_or(ParseVersionError)?;
        let patch = parts.next().ok_or(ParseVersionError)?;

        // The patch component may carry a pre-release suffix.
        let patch = patch
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .filter(|s| !s.is_empty())
            .ok_or(ParseVersionError)?;

        let parse = |part: &str| part.parse::<u8>().map_err(|_| ParseVersionError);
        Ok(Self::new(parse(major)?, parse(minor)?, parse(patch)?))
    }
}

impl Serialize for Version {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse().map_err(de::Error::custom)
    }
}

/// What kind of app this is, from the low two bits of the flags.
///
/// The four kinds behave quite differently: a Glance is a widget for the 240x60
/// notification area, an Activity records a session, a Utility is a full-screen
/// app, and a Clockface is a watch face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AppType {
    /// Records a session.
    Activity,
    /// Full-screen app launched from the launcher.
    Utility,
    /// Widget for the 240x60 notification area.
    Glance,
    /// Watch face.
    Clockface,
}

impl AppType {
    /// Decode from the flags word. Only two bits are involved, so this is total.
    #[must_use]
    pub const fn from_flags(flags: u32) -> Self {
        match flags & 0b11 {
            0 => Self::Activity,
            1 => Self::Utility,
            2 => Self::Glance,
            _ => Self::Clockface,
        }
    }
}

impl fmt::Display for AppType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Activity => "Activity",
            Self::Utility => "Utility",
            Self::Glance => "Glance",
            Self::Clockface => "Clockface",
        };
        f.write_str(name)
    }
}

bitflags! {
    /// Header flag bits.
    ///
    /// Bits 0-1 hold the [`AppType`] rather than a flag, and unknown bits are
    /// retained rather than rejected, so `_` covers everything unnamed.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Flags: u32 {
        /// The app starts automatically at boot.
        const AUTOSTART = 0x0000_0008;
        /// Marked as Glance-capable.
        ///
        /// Carries no information in practice: `una_app_build_app()` in
        /// `cmake/una-app.cmake` passes `-glance_capable` unconditionally, so
        /// every officially built app sets it. Never surface it as a feature.
        const GLANCE_CAPABLE = 0x0000_0020;
        const _ = !0;
    }
}

/// The fixed 48-byte header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Stable application identity.
    pub app_id: AppId,
    /// Version stamped from the release tag.
    pub version: Version,
    /// `LibC` ABI the app was linked against.
    pub libc_version: Version,
    /// Length of the service image in bytes.
    pub service_len: usize,
    /// Raw flag bits.
    pub flags: Flags,
    /// Display name, at most 15 characters. May contain a path separator: the
    /// `GlanceARHR` app is named `AVG / R HR`.
    pub name: String,
    /// Declared length of the 60x60 icon field.
    pub normal_icon_len: usize,
    /// Declared length of the 30x30 icon field.
    pub small_icon_len: usize,
}

impl Header {
    /// Parse the header from at least [`HEADER_LEN`] bytes.
    ///
    /// # Errors
    /// [`Error::TooShort`] if truncated, [`Error::NameNotUtf8`] if the name
    /// field is not UTF-8.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let head: &[u8; HEADER_LEN] = bytes
            .get(..HEADER_LEN)
            .and_then(|s| s.try_into().ok())
            .ok_or(Error::TooShort { got: bytes.len() })?;

        let u32_at = |offset: usize| -> u32 {
            u32::from_le_bytes([
                head[offset],
                head[offset + 1],
                head[offset + 2],
                head[offset + 3],
            ])
        };

        let mut id = [0u8; 8];
        id.copy_from_slice(&head[..8]);

        let name_field = &head[24..40];
        let name_bytes = name_field
            .iter()
            .position(|&b| b == 0)
            .map_or(name_field, |nul| &name_field[..nul]);
        let name = std::str::from_utf8(name_bytes)
            .map_err(|_| Error::NameNotUtf8)?
            .trim()
            .to_owned();

        Ok(Self {
            app_id: AppId::new(u64::from_le_bytes(id)),
            version: Version::from_packed(u32_at(8)),
            libc_version: Version::from_packed(u32_at(12)),
            service_len: u32_at(16) as usize,
            flags: Flags::from_bits_retain(u32_at(20)),
            name,
            normal_icon_len: u32_at(40) as usize,
            small_icon_len: u32_at(44) as usize,
        })
    }

    /// What kind of app this is.
    #[must_use]
    pub const fn app_type(&self) -> AppType {
        AppType::from_flags(self.flags.bits())
    }

    /// Whether the app starts at boot.
    #[must_use]
    pub const fn autostart(&self) -> bool {
        self.flags.contains(Flags::AUTOSTART)
    }

    /// Bytes accounted for by everything except the GUI image.
    #[must_use]
    pub const fn fixed_len(&self) -> usize {
        HEADER_LEN + self.normal_icon_len + self.small_icon_len + self.service_len + CRC_LEN
    }

    /// Length of the GUI image in a file of `total` bytes. Zero for a Glance app
    /// built without one.
    ///
    /// # Errors
    /// [`Error::Truncated`] if the header accounts for more bytes than exist,
    /// which is the cheapest way to tell a `.uapp` from an unrelated file.
    pub fn gui_len(&self, total: usize) -> Result<usize, Error> {
        total.checked_sub(self.fixed_len()).ok_or(Error::Truncated {
            declared: self.fixed_len(),
            actual: total,
        })
    }
}

/// Result of checking the CRC-32 footer.
///
/// A file failing this check is dropped *silently* by the watch kernel — the app
/// simply never appears in the launcher — so always check before writing one to
/// a device. Reported rather than raised, because a caller inspecting a file is
/// not necessarily about to install it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrcCheck {
    /// Value stored in the footer.
    pub stored: u32,
    /// Value computed over the preceding bytes.
    pub computed: u32,
}

impl CrcCheck {
    /// Whether the footer matches the content.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.stored == self.computed
    }
}

/// A parsed `.uapp`, borrowing the file's bytes.
#[derive(Debug, Clone)]
pub struct Uapp<'a> {
    bytes: &'a [u8],
    header: Header,
    gui_len: usize,
}

impl<'a> Uapp<'a> {
    /// Parse a complete `.uapp`.
    ///
    /// # Errors
    /// See [`Header::parse`] and [`Header::gui_len`].
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let header = Header::parse(bytes)?;
        let gui_len = header.gui_len(bytes.len())?;
        Ok(Self {
            bytes,
            header,
            gui_len,
        })
    }

    /// The parsed header.
    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    /// Total file length.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether the file is empty. Never true for a parsed `.uapp`.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Length of the GUI image; zero when absent.
    #[must_use]
    pub const fn gui_len(&self) -> usize {
        self.gui_len
    }

    /// The 60x60 icon field. A non-zero length does not mean there are pixels —
    /// see [`crate::icon::is_blank`].
    #[must_use]
    pub fn normal_icon(&self) -> &'a [u8] {
        let start = HEADER_LEN;
        &self.bytes[start..start + self.header.normal_icon_len]
    }

    /// The 30x30 icon field.
    #[must_use]
    pub fn small_icon(&self) -> &'a [u8] {
        let start = HEADER_LEN + self.header.normal_icon_len;
        &self.bytes[start..start + self.header.small_icon_len]
    }

    /// The service image.
    #[must_use]
    pub fn service(&self) -> &'a [u8] {
        let start = HEADER_LEN + self.header.normal_icon_len + self.header.small_icon_len;
        &self.bytes[start..start + self.header.service_len]
    }

    /// The GUI image; empty for a Glance app without one.
    #[must_use]
    pub fn gui(&self) -> &'a [u8] {
        let start = self.bytes.len() - CRC_LEN - self.gui_len;
        &self.bytes[start..start + self.gui_len]
    }

    /// Everything between the header and the CRC footer: icons, service and GUI.
    ///
    /// This is the app itself, with no version stamp in it. Hashing this rather
    /// than the whole file distinguishes "the code changed" from "the release tag
    /// moved" — in `apps-v1.3.0`, six of the thirteen apps were byte-identical to
    /// their `apps-v1.2.0` builds.
    #[must_use]
    pub fn payload(&self) -> &'a [u8] {
        &self.bytes[HEADER_LEN..self.bytes.len() - CRC_LEN]
    }

    /// Check the CRC-32 footer.
    #[must_use]
    pub fn verify_crc(&self) -> CrcCheck {
        let split = self.bytes.len() - CRC_LEN;
        let (body, footer) = self.bytes.split_at(split);
        let stored = u32::from_le_bytes([footer[0], footer[1], footer[2], footer[3]]);
        CrcCheck {
            stored,
            computed: crc32fast::hash(body),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a valid `.uapp` so tests need no vendor binaries.
    fn make(name: &str, flags: u32, service: usize, gui: usize) -> Vec<u8> {
        let total = HEADER_LEN + NORMAL_ICON_LEN + SMALL_ICON_LEN + service + gui + CRC_LEN;
        let mut bytes = vec![0u8; total];
        bytes[..8].copy_from_slice(&0xA19C_2A7E_4F8B_6D31u64.to_le_bytes());
        bytes[8..12].copy_from_slice(&Version::new(1, 3, 0).packed().to_le_bytes());
        bytes[12..16].copy_from_slice(&Version::new(0, 0, 3).packed().to_le_bytes());
        bytes[16..20].copy_from_slice(&(service as u32).to_le_bytes());
        bytes[20..24].copy_from_slice(&flags.to_le_bytes());
        let name = name.as_bytes();
        bytes[24..24 + name.len().min(15)].copy_from_slice(&name[..name.len().min(15)]);
        bytes[40..44].copy_from_slice(&(NORMAL_ICON_LEN as u32).to_le_bytes());
        bytes[44..48].copy_from_slice(&(SMALL_ICON_LEN as u32).to_le_bytes());
        for (i, byte) in bytes
            .iter_mut()
            .enumerate()
            .take(total - CRC_LEN)
            .skip(HEADER_LEN)
        {
            *byte = i as u8;
        }
        let crc = crc32fast::hash(&bytes[..total - CRC_LEN]);
        bytes[total - CRC_LEN..].copy_from_slice(&crc.to_le_bytes());
        bytes
    }

    #[test]
    fn parses_every_header_field() {
        let bytes = make("Alarm", 0x29, 64, 32);
        let uapp = Uapp::parse(&bytes).unwrap();
        let header = uapp.header();
        assert_eq!(header.app_id.to_string(), "A19C2A7E4F8B6D31");
        assert_eq!(header.version, Version::new(1, 3, 0));
        assert_eq!(header.libc_version, Version::new(0, 0, 3));
        assert_eq!(header.name, "Alarm");
        assert_eq!(header.app_type(), AppType::Utility);
        assert!(header.autostart());
        assert!(header.flags.contains(Flags::GLANCE_CAPABLE));
        assert_eq!(header.service_len, 64);
        assert_eq!(uapp.gui_len(), 32);
        assert!(uapp.verify_crc().is_valid());
    }

    #[test]
    fn slices_tile_the_file_exactly() {
        let bytes = make("Alarm", 0x29, 128, 256);
        let uapp = Uapp::parse(&bytes).unwrap();
        let tiled = HEADER_LEN
            + uapp.normal_icon().len()
            + uapp.small_icon().len()
            + uapp.service().len()
            + uapp.gui().len()
            + CRC_LEN;
        assert_eq!(tiled, bytes.len());
        assert_eq!(uapp.payload().len(), bytes.len() - HEADER_LEN - CRC_LEN);
    }

    #[test]
    fn glance_without_gui_has_zero_gui_len() {
        let bytes = make("Live HR", 0x22, 64, 0);
        let uapp = Uapp::parse(&bytes).unwrap();
        assert_eq!(uapp.header().app_type(), AppType::Glance);
        assert_eq!(uapp.gui_len(), 0);
        assert!(uapp.gui().is_empty());
    }

    #[test]
    fn every_app_type_decodes() {
        for (bits, expected) in [
            (0, AppType::Activity),
            (1, AppType::Utility),
            (2, AppType::Glance),
            (3, AppType::Clockface),
        ] {
            let bytes = make("x", bits, 16, 0);
            assert_eq!(Uapp::parse(&bytes).unwrap().header().app_type(), expected);
        }
    }

    #[test]
    fn name_uses_the_whole_field_when_unterminated() {
        let bytes = make("ABCDEFGHIJKLMNO", 0, 16, 0);
        assert_eq!(
            Uapp::parse(&bytes).unwrap().header().name,
            "ABCDEFGHIJKLMNO"
        );
    }

    #[test]
    fn display_name_may_contain_a_path_separator() {
        // GlanceARHR really ships as "AVG / R HR", which is why on-device folder
        // names must come from the release layout and never from the header.
        let bytes = make("AVG / R HR", 0x22, 16, 0);
        assert_eq!(Uapp::parse(&bytes).unwrap().header().name, "AVG / R HR");
    }

    #[test]
    fn corrupt_footer_is_reported_not_raised() {
        let mut bytes = make("Alarm", 0x29, 64, 32);
        let len = bytes.len();
        bytes[len - 1] ^= 0xFF;
        let check = Uapp::parse(&bytes).unwrap().verify_crc();
        assert!(!check.is_valid());
        assert_ne!(check.stored, check.computed);
    }

    #[test]
    fn header_only_parse_leaves_gui_len_to_the_caller() {
        let bytes = make("Alarm", 0x29, 64, 32);
        let header = Header::parse(&bytes[..HEADER_LEN]).unwrap();
        assert_eq!(header.name, "Alarm");
        assert_eq!(header.gui_len(bytes.len()).unwrap(), 32);
    }

    #[test]
    fn rejects_input_shorter_than_the_header() {
        assert_eq!(
            Header::parse(&[0u8; HEADER_LEN - 1]),
            Err(Error::TooShort {
                got: HEADER_LEN - 1
            })
        );
    }

    #[test]
    fn rejects_a_file_smaller_than_its_header_claims() {
        let bytes = make("Alarm", 0x29, 4096, 0);
        let header = Header::parse(&bytes).unwrap();
        assert!(matches!(header.gui_len(512), Err(Error::Truncated { .. })));
    }

    #[test]
    fn app_id_round_trips() {
        let id: AppId = "A19C2A7E4F8B6D31".parse().unwrap();
        assert_eq!(id.to_string(), "A19C2A7E4F8B6D31");
        assert_eq!(
            "8899AABBCCDDEEFF".parse::<AppId>().unwrap().get(),
            0x8899_AABB_CCDD_EEFF
        );
        assert!("A19C".parse::<AppId>().is_err());
        assert!("A19C2A7E4F8B6D3Z".parse::<AppId>().is_err());
    }

    #[test]
    fn version_round_trips_and_orders() {
        assert_eq!(Version::from_packed(0x0001_0300).to_string(), "1.3.0");
        assert_eq!("1.3.0".parse::<Version>().unwrap(), Version::new(1, 3, 0));
        // Upstream publishes apps-v0.1.9-rc3 as a full release.
        assert_eq!(
            "v0.1.9-rc3".parse::<Version>().unwrap(),
            Version::new(0, 1, 9)
        );
        assert!("nonsense".parse::<Version>().is_err());
        assert!("1.3.256".parse::<Version>().is_err());
        assert!(Version::new(1, 3, 0) > Version::new(1, 2, 9));
        assert!(Version::new(0, 1, 9) < Version::new(1, 0, 0));
    }
}
