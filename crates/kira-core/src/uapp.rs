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
//! 48            normal icon, small icon, service image, trailing region
//! len-4   4     CRC32          u32  over everything preceding it
//! ```
//!
//! Nothing in the header gives the length of the trailing region, so it is
//! whatever remains between the service image and the CRC footer. For an
//! ordinary app that region is the GUI image, absent for a Glance app built
//! without one. For a **variant alias** it is something else entirely — see
//! [`VariantAlias`] — which is why nothing here calls it the GUI image.

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
/// Length of a variant alias's fixed descriptor, ahead of its config.
pub const VARIANT_PAYLOAD_LEN: usize = 32;
/// The only descriptor layout this reader understands.
///
/// The SDK's own `SDK::Variant::Config` gates on the same value and falls back to
/// classic defaults on anything else: a shipped reader must never guess at
/// rearranged fields. Kira does the same, and reports rather than guesses.
pub const VARIANT_PAYLOAD_VERSION: u32 = 1;
/// Largest embedded config the kernel will read, from `make_variant.py`.
pub const VARIANT_CONFIG_MAX: usize = 8192;

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
        /// `FLAG_APP_VARIANT_ALIAS`: this file carries no code at all.
        ///
        /// It is an alias that makes an existing app binary appear in the
        /// launcher as a separate activity — see [`VariantAlias`]. This bit is
        /// the *only* thing that says so, which is why a file carrying it is
        /// never read as an ordinary app even when its descriptor is unreadable.
        const VARIANT_ALIAS = 0x0000_0040;
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

    /// Whether this file is a variant alias rather than an app.
    ///
    /// A code-less `.uapp` that makes an existing binary appear in the launcher
    /// as a separate activity. See [`VariantAlias`].
    #[must_use]
    pub const fn is_variant_alias(&self) -> bool {
        self.flags.contains(Flags::VARIANT_ALIAS)
    }

    /// Bytes accounted for by everything except the trailing region.
    #[must_use]
    pub const fn fixed_len(&self) -> usize {
        HEADER_LEN + self.normal_icon_len + self.small_icon_len + self.service_len + CRC_LEN
    }

    /// Length of the trailing region in a file of `total` bytes.
    ///
    /// The GUI image for an ordinary app, zero for a Glance app built without
    /// one, and the alias descriptor plus its config for a variant.
    ///
    /// # Errors
    /// [`Error::Truncated`] if the header accounts for more bytes than exist,
    /// which is the cheapest way to tell a `.uapp` from an unrelated file.
    pub fn trailing_len(&self, total: usize) -> Result<usize, Error> {
        total.checked_sub(self.fixed_len()).ok_or(Error::Truncated {
            declared: self.fixed_len(),
            actual: total,
        })
    }
}

/// Who owns a variant's folder on the watch when an update lands.
///
/// The raw byte is not retained: nothing renders it, and an origin Kira does not
/// know the meaning of is [`Self::Unknown`] whatever number it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VariantOrigin {
    /// Packed by upstream's CI from a manifest in the SDK tree.
    Shipped,
    /// Created on the watch, by the kernel's `CreateVariant`.
    ///
    /// Such a variant exists in no release, so nothing in a catalogue can
    /// describe it — only the alias itself says what it is.
    User,
    /// A value this build does not know the meaning of.
    Unknown,
}

impl VariantOrigin {
    /// Decode the descriptor's `origin` byte.
    #[must_use]
    pub const fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Shipped,
            1 => Self::User,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for VariantOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Shipped => "shipped",
            Self::User => "user",
            Self::Unknown => "unknown",
        };
        f.write_str(name)
    }
}

/// Why an alias descriptor could not be read.
///
/// Every one of these describes a file the watch's own validator would refuse or
/// read differently, so none of them is a case to guess through. Reported rather
/// than raised: the alias flag has already settled *what the file is*, and this
/// only says that its contents do not follow.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VariantError {
    /// The header claims code, which an alias never has.
    #[error(
        "an alias carries no code, but this declares serviceLen {service_len} and LibC {libc_version}"
    )]
    NotCodeless {
        /// Declared length of the service image.
        service_len: usize,
        /// Declared `LibC` ABI version.
        libc_version: Version,
    },
    /// The icon fields are not the fixed sizes the descriptor's offset assumes.
    ///
    /// The kernel and the SDK's reader both seek to a *constant* 48 + 3600 + 900,
    /// so a header declaring anything else points them at different bytes from
    /// the ones [`Header::fixed_len`] would compute.
    #[error(
        "an alias must declare {NORMAL_ICON_LEN}/{SMALL_ICON_LEN} icon bytes, not {normal}/{small}"
    )]
    IconSizes {
        /// Declared length of the 60x60 icon field.
        normal: usize,
        /// Declared length of the 30x30 icon field.
        small: usize,
    },
    /// Fewer bytes after the icons than the fixed descriptor needs.
    #[error("too short for an alias descriptor: {got} < {VARIANT_PAYLOAD_LEN}")]
    TooShort {
        /// Bytes available after the icons.
        got: usize,
    },
    /// A descriptor layout this build was not written for.
    #[error("alias payloadVersion {version}, and this reads only {VARIANT_PAYLOAD_VERSION}")]
    UnsupportedPayload {
        /// Version the descriptor declares.
        version: u32,
    },
    /// The declared config length does not account for the file.
    #[error("alias declares a {declared}-byte config but {available} bytes follow the descriptor")]
    ConfigSize {
        /// Length the descriptor declares.
        declared: usize,
        /// Bytes actually between the descriptor and the CRC footer.
        available: usize,
    },
    /// A config larger than the kernel will read.
    #[error("alias config is {declared} bytes, over the kernel's {VARIANT_CONFIG_MAX}")]
    ConfigTooLarge {
        /// Length the descriptor declares.
        declared: usize,
    },
    /// No config at all, which the app-side reader treats as no variant.
    #[error("alias carries no config, so the target would run as its classic self")]
    NoConfig,
    /// The config is not text.
    #[error("alias config is not valid UTF-8")]
    ConfigNotUtf8,
    /// The alias claims its own identity as its target.
    #[error("alias targets its own AppID {target}, which the kernel rejects at boot scan")]
    TargetsItself {
        /// The contested identity.
        target: AppId,
    },
}

/// A code-less `.uapp` that presents an existing app binary as a second activity.
///
/// `Walk` is the first: the `Hike` binary, its own [`AppId`] and icons, and a JSON
/// config that tells the shared binary which FIT sport to record. It is
/// discriminated by [`Flags::VARIANT_ALIAS`] alone, and everything below it comes
/// from the fixed 32-byte descriptor that replaces the GUI image:
///
/// ```text
/// offset  size  field
/// 0       4     payloadVersion    u32  gated on == 1
/// 4       8     targetAppID       u64
/// 12      4     minTargetVersion  u32  packed as AppVersion; 0 = any
/// 16      1     origin            u8   0 shipped, 1 user
/// 17      4     configSize        u32  max 8192, unaligned
/// 21      11    reserved               zeroed
/// 32            config JSON, configSize bytes
/// ```
///
/// The config is carried verbatim and deliberately not parsed. The kernel never
/// parses it either: only the app does, against a `features` vocabulary its own
/// family defines, so anything Kira read out of it beyond `schema` would be a
/// guess about somebody else's subtree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantAlias<'a> {
    /// The app whose binary actually runs.
    pub target: AppId,
    /// Oldest target build this may resolve against; `None` for any.
    ///
    /// The field is the packed `A.B.C`, which cannot carry a pre-release suffix —
    /// so a `1.4.0-rc2` target satisfies a `1.4.0` minimum, as upstream intends.
    pub min_target_version: Option<Version>,
    /// Who owns the folder on update.
    pub origin: VariantOrigin,
    /// The embedded config, verbatim.
    ///
    /// Third-party JSON out of a binary: render as text, never as HTML.
    pub config: &'a str,
}

impl<'a> VariantAlias<'a> {
    /// Read the descriptor of a file whose alias flag is set.
    ///
    /// `bytes` is the whole file and `header` its parsed head; the caller has
    /// already established that the file is at least [`Header::fixed_len`] long.
    fn parse(bytes: &'a [u8], header: &Header) -> Result<Self, VariantError> {
        // Everything the kernel's validator checks about *where the bytes are*.
        // Its reader seeks to a constant offset rather than to anything the
        // header declares, so a header disagreeing with that constant describes
        // a file Kira and the watch would read differently -- which is the one
        // kind of disagreement that must never be papered over.
        if header.service_len != 0 || header.libc_version != Version::default() {
            return Err(VariantError::NotCodeless {
                service_len: header.service_len,
                libc_version: header.libc_version,
            });
        }
        if header.normal_icon_len != NORMAL_ICON_LEN || header.small_icon_len != SMALL_ICON_LEN {
            return Err(VariantError::IconSizes {
                normal: header.normal_icon_len,
                small: header.small_icon_len,
            });
        }

        // Equal to the kernel's constant 48 + 3600 + 900, given the two checks
        // above; computed rather than written out so the two cannot drift.
        let start = header.fixed_len() - CRC_LEN;
        let region = &bytes[start..bytes.len() - CRC_LEN];
        let fixed: &[u8; VARIANT_PAYLOAD_LEN] = region
            .get(..VARIANT_PAYLOAD_LEN)
            .and_then(|s| s.try_into().ok())
            .ok_or(VariantError::TooShort { got: region.len() })?;

        let u32_at = |offset: usize| -> u32 {
            u32::from_le_bytes([
                fixed[offset],
                fixed[offset + 1],
                fixed[offset + 2],
                fixed[offset + 3],
            ])
        };

        let version = u32_at(0);
        if version != VARIANT_PAYLOAD_VERSION {
            return Err(VariantError::UnsupportedPayload { version });
        }

        let mut id = [0u8; 8];
        id.copy_from_slice(&fixed[4..12]);
        let target = AppId::new(u64::from_le_bytes(id));
        if target == header.app_id {
            return Err(VariantError::TargetsItself { target });
        }

        let declared = u32_at(17) as usize;
        let available = region.len() - VARIANT_PAYLOAD_LEN;
        if declared == 0 {
            return Err(VariantError::NoConfig);
        }
        if declared > VARIANT_CONFIG_MAX {
            return Err(VariantError::ConfigTooLarge { declared });
        }
        if declared != available {
            return Err(VariantError::ConfigSize {
                declared,
                available,
            });
        }

        let min_target = u32_at(12);
        Ok(Self {
            target,
            // Zero is the packer's "any", not version 0.0.0.
            min_target_version: (min_target != 0).then(|| Version::from_packed(min_target)),
            origin: VariantOrigin::from_raw(fixed[16]),
            config: std::str::from_utf8(&region[VARIANT_PAYLOAD_LEN..])
                .map_err(|_| VariantError::ConfigNotUtf8)?,
        })
    }
}

/// CRC-32/ISO-HDLC, the polynomial the packer uses.
///
/// Hand-rolled rather than pulled from a crate: this runs over a few hundred
/// kilobytes at a time, so a table-driven byte-at-a-time loop is ample, and the
/// browser build has no room for a dependency that ships SIMD dispatch it will
/// never use.
fn crc32(bytes: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut n = 0;
        while n < 256 {
            let mut c = n as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                k += 1;
            }
            table[n] = c;
            n += 1;
        }
        table
    };

    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc = TABLE[((crc ^ u32::from(byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
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
    trailing_len: usize,
    variant: Option<Result<VariantAlias<'a>, VariantError>>,
}

impl<'a> Uapp<'a> {
    /// Parse a complete `.uapp`.
    ///
    /// A variant alias whose descriptor is malformed still parses: the flag has
    /// already settled what the file is, and refusing here would take a whole
    /// watch scan or catalogue build down over one file. [`Self::variant`]
    /// carries the failure instead.
    ///
    /// # Errors
    /// See [`Header::parse`] and [`Header::trailing_len`].
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let header = Header::parse(bytes)?;
        let trailing_len = header.trailing_len(bytes.len())?;
        let variant = header
            .is_variant_alias()
            .then(|| VariantAlias::parse(bytes, &header));
        Ok(Self {
            bytes,
            header,
            trailing_len,
            variant,
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

    /// Length of the trailing region; zero when absent.
    #[must_use]
    pub const fn trailing_len(&self) -> usize {
        self.trailing_len
    }

    /// The variant descriptor, when [`Flags::VARIANT_ALIAS`] is set.
    ///
    /// `None` for an ordinary app. `Some(Err(_))` for an alias whose descriptor
    /// does not follow the contract — never `None`, because the flag is the only
    /// thing that decides what the file is and an unreadable alias is still not
    /// an app.
    #[must_use]
    pub fn variant(&self) -> Option<Result<&VariantAlias<'a>, &VariantError>> {
        self.variant.as_ref().map(Result::as_ref)
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

    /// Everything after the service image and before the CRC footer.
    ///
    /// The GUI image for an ordinary app, empty for a Glance app without one, and
    /// the descriptor plus config for a variant alias — which is why it is not
    /// called the GUI image. Use [`Self::variant`] to read the latter.
    #[must_use]
    pub fn trailing(&self) -> &'a [u8] {
        let start = self.bytes.len() - CRC_LEN - self.trailing_len;
        &self.bytes[start..start + self.trailing_len]
    }

    /// Everything between the header and the CRC footer: icons, service and the
    /// trailing region.
    ///
    /// This is the app itself. Hashing it rather than the whole file
    /// distinguishes "the code changed" from "the release tag moved" — in
    /// `apps-v1.3.0`, six of the thirteen apps were byte-identical to their
    /// `apps-v1.2.0` builds.
    ///
    /// The version stamp is the reason, but the *whole* header is excluded, so
    /// the [`AppId`], the `LibC` ABI version, the type and autostart flags, the
    /// display name and the icon lengths are out of it too. Two builds differing
    /// only in one of those hash the same and are reported as unchanged code —
    /// true of the code, and silent about the flag. Narrowing the exclusion to
    /// the version field alone would redefine a field the catalogue has already
    /// published, so this is a boundary to be aware of rather than a bug to fix
    /// quietly. `the_payload_hash_ignores_every_header_field` pins it.
    ///
    /// **For a variant alias this is not code and cannot stand in for one.** An
    /// alias has none: this covers its icons, its descriptor and its config, so
    /// two builds hashing the same mean only that the *alias* is unchanged. What
    /// it does is the target binary's, and the alias's bytes say nothing at all
    /// about whether that moved. Nothing may report "code unchanged" from this
    /// hash for an alias -- see `catalog::contents_noun`.
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
            computed: crc32(body),
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
        let crc = crc32(&bytes[..total - CRC_LEN]);
        bytes[total - CRC_LEN..].copy_from_slice(&crc.to_le_bytes());
        bytes
    }

    /// `Hike`, and the `Walk` alias that runs on it. Real ids from apps-v1.4.0.
    const TARGET_ID: u64 = 0xA1F3_C92B_7E4D_8A10;
    const ALIAS_ID: u64 = 0xA1E5_D3B7_C9F0_4A82;
    /// The config `Walk 1.4.0` embeds, byte for byte.
    const WALK_CONFIG: &str = r#"{"schema":1,"name":"Walk","fit":{"sport":11,"subSport":0}}"#;

    fn restamp(bytes: &mut [u8]) {
        let end = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..end]);
        bytes[end..].copy_from_slice(&crc.to_le_bytes());
    }

    /// Build a variant alias the way `make_variant.py` packs one.
    ///
    /// Field by field rather than through a helper, so these tests check the
    /// layout against the packer rather than agreeing with the reader.
    fn make_alias(config: &[u8]) -> Vec<u8> {
        let total = HEADER_LEN
            + NORMAL_ICON_LEN
            + SMALL_ICON_LEN
            + VARIANT_PAYLOAD_LEN
            + config.len()
            + CRC_LEN;
        let mut bytes = vec![0u8; total];
        bytes[..8].copy_from_slice(&ALIAS_ID.to_le_bytes());
        bytes[8..12].copy_from_slice(&Version::new(1, 4, 0).packed().to_le_bytes());
        // LibCVersion and serviceLen stay zero: an alias carries no code.
        bytes[20..24].copy_from_slice(&Flags::VARIANT_ALIAS.bits().to_le_bytes());
        bytes[24..28].copy_from_slice(b"Walk");
        bytes[40..44].copy_from_slice(&(NORMAL_ICON_LEN as u32).to_le_bytes());
        bytes[44..48].copy_from_slice(&(SMALL_ICON_LEN as u32).to_le_bytes());

        let at = HEADER_LEN + NORMAL_ICON_LEN + SMALL_ICON_LEN;
        bytes[at..at + 4].copy_from_slice(&VARIANT_PAYLOAD_VERSION.to_le_bytes());
        bytes[at + 4..at + 12].copy_from_slice(&TARGET_ID.to_le_bytes());
        bytes[at + 12..at + 16].copy_from_slice(&Version::new(1, 4, 0).packed().to_le_bytes());
        bytes[at + 16] = 0; // shipped
        bytes[at + 17..at + 21].copy_from_slice(&(config.len() as u32).to_le_bytes());
        bytes[at + VARIANT_PAYLOAD_LEN..at + VARIANT_PAYLOAD_LEN + config.len()]
            .copy_from_slice(config);
        restamp(&mut bytes);
        bytes
    }

    /// The descriptor's offset within a well-formed alias.
    const fn descriptor_at() -> usize {
        HEADER_LEN + NORMAL_ICON_LEN + SMALL_ICON_LEN
    }

    #[test]
    fn crc32_matches_the_reference_vector() {
        // The check value for CRC-32/ISO-HDLC, which is what zlib.crc32 in the
        // packer computes. A hand-rolled table is only trustworthy against this.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
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
        assert_eq!(uapp.trailing_len(), 32);
        assert!(uapp.verify_crc().is_valid());
    }

    #[test]
    fn the_payload_hash_ignores_every_header_field() {
        // Pins the boundary the docs describe. The payload is what "same code"
        // is decided from, and it excludes the whole header -- so a build that
        // changed only its autostart flag, its name or its declared type reads
        // as unchanged code. That is accurate about the code and says nothing
        // about the flag, and it is worth failing loudly if it ever shifts,
        // because the catalogue has published these hashes.
        let base = make("Alarm", 0x29, 64, 32);
        let payload_of = |bytes: &[u8]| Uapp::parse(bytes).unwrap().payload().to_vec();
        let expected = payload_of(&base);

        // Every header field, one at a time. Offsets are the layout at the top of
        // this file; replacements are valid values rather than flipped bits, so
        // each fixture stays a parseable .uapp, and the CRC is restamped.
        let name: &[u8] = b"Renamed\0\0\0\0\0\0\0\0\0";
        for (offset, replacement, what) in [
            (0usize, &0xDEAD_BEEF_DEAD_BEEFu64.to_le_bytes()[..], "AppID"),
            (
                8,
                &Version::new(9, 9, 9).packed().to_le_bytes()[..],
                "version",
            ),
            (
                12,
                &Version::new(0, 0, 9).packed().to_le_bytes()[..],
                "LibC ABI",
            ),
            (20, &0x21u32.to_le_bytes()[..], "flags, including autostart"),
            (24, name, "display name"),
        ] {
            let mut altered = base.clone();
            altered[offset..offset + replacement.len()].copy_from_slice(replacement);
            let end = altered.len() - CRC_LEN;
            let crc = crc32(&altered[..end]);
            altered[end..].copy_from_slice(&crc.to_le_bytes());

            assert!(
                Uapp::parse(&altered).unwrap().verify_crc().is_valid(),
                "{what}: the fixture should still be a valid .uapp"
            );
            assert_eq!(
                payload_of(&altered),
                expected,
                "{what} reached the payload hash; \"same code\" now means something else"
            );
        }
    }

    #[test]
    fn slices_tile_the_file_exactly() {
        let bytes = make("Alarm", 0x29, 128, 256);
        let uapp = Uapp::parse(&bytes).unwrap();
        let tiled = HEADER_LEN
            + uapp.normal_icon().len()
            + uapp.small_icon().len()
            + uapp.service().len()
            + uapp.trailing().len()
            + CRC_LEN;
        assert_eq!(tiled, bytes.len());
        assert_eq!(uapp.payload().len(), bytes.len() - HEADER_LEN - CRC_LEN);
    }

    #[test]
    fn glance_without_gui_has_no_trailing_region() {
        let bytes = make("Live HR", 0x22, 64, 0);
        let uapp = Uapp::parse(&bytes).unwrap();
        assert_eq!(uapp.header().app_type(), AppType::Glance);
        assert_eq!(uapp.trailing_len(), 0);
        assert!(uapp.trailing().is_empty());
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
    fn header_only_parse_leaves_the_trailing_length_to_the_caller() {
        let bytes = make("Alarm", 0x29, 64, 32);
        let header = Header::parse(&bytes[..HEADER_LEN]).unwrap();
        assert_eq!(header.name, "Alarm");
        assert_eq!(header.trailing_len(bytes.len()).unwrap(), 32);
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
        assert!(matches!(
            header.trailing_len(512),
            Err(Error::Truncated { .. })
        ));
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

    #[test]
    fn a_variant_alias_decodes_its_descriptor() {
        let bytes = make_alias(WALK_CONFIG.as_bytes());
        // The shipped Walk 1.4.0 is exactly this size, which is the cheapest
        // check that this fixture is packed the way upstream packs one.
        assert_eq!(bytes.len(), 4642);

        let uapp = Uapp::parse(&bytes).unwrap();
        let header = uapp.header();
        assert!(header.is_variant_alias());
        // An alias declares its target's type and nothing else. Walk is the only
        // app in apps-v1.4.0 without GLANCE_CAPABLE, because the packer builds the
        // flags word from the type and the alias bit alone.
        assert_eq!(header.app_type(), AppType::Activity);
        assert!(!header.flags.contains(Flags::GLANCE_CAPABLE));
        assert!(!header.autostart());
        assert_eq!(header.service_len, 0);
        assert_eq!(header.libc_version, Version::new(0, 0, 0));

        let alias = uapp
            .variant()
            .expect("the flag is set")
            .expect("well formed");
        assert_eq!(alias.target, AppId::new(TARGET_ID));
        assert_eq!(alias.min_target_version, Some(Version::new(1, 4, 0)));
        assert_eq!(alias.origin, VariantOrigin::Shipped);
        assert_eq!(alias.config, WALK_CONFIG);
        assert_eq!(uapp.trailing_len(), VARIANT_PAYLOAD_LEN + WALK_CONFIG.len());
        assert!(uapp.verify_crc().is_valid());
    }

    #[test]
    fn an_ordinary_app_has_no_descriptor_to_read() {
        let bytes = make("Alarm", 0x29, 64, 32);
        assert!(!Uapp::parse(&bytes).unwrap().header().is_variant_alias());
        assert!(Uapp::parse(&bytes).unwrap().variant().is_none());
    }

    #[test]
    fn min_target_version_zero_means_any_rather_than_0_0_0() {
        // The packer's default, and what every variant carried before una-sdk#281
        // gave Walking a real floor.
        let mut bytes = make_alias(WALK_CONFIG.as_bytes());
        let at = descriptor_at();
        bytes[at + 12..at + 16].copy_from_slice(&0u32.to_le_bytes());
        restamp(&mut bytes);

        let uapp = Uapp::parse(&bytes).unwrap();
        let alias = uapp.variant().unwrap().unwrap();
        assert_eq!(alias.min_target_version, None);
    }

    #[test]
    fn a_user_created_variant_says_so() {
        // Nothing upstream publishes carries origin 1; the kernel's CreateVariant
        // stamps it on a variant made on the watch, which exists in no release.
        let mut bytes = make_alias(WALK_CONFIG.as_bytes());
        bytes[descriptor_at() + 16] = 1;
        restamp(&mut bytes);
        let uapp = Uapp::parse(&bytes).unwrap();
        assert_eq!(uapp.variant().unwrap().unwrap().origin, VariantOrigin::User);

        // And an origin from some later firmware is reported as unknown rather
        // than guessed into one of the two this build knows.
        bytes[descriptor_at() + 16] = 7;
        restamp(&mut bytes);
        let uapp = Uapp::parse(&bytes).unwrap();
        assert_eq!(
            uapp.variant().unwrap().unwrap().origin,
            VariantOrigin::Unknown
        );
    }

    #[test]
    fn a_malformed_alias_is_never_read_as_an_ordinary_app() {
        // The flag is the only thing that decides what the file is. Every one of
        // these is a file the watch would refuse or read differently, and each
        // must come back as an alias that failed rather than as an app.
        type Damage = fn(&mut [u8]);
        const AT: usize = descriptor_at();
        let cases: [(&str, Damage); 7] = [
            ("payload version", |b| {
                b[AT..AT + 4].copy_from_slice(&2u32.to_le_bytes());
            }),
            ("config size", |b| {
                b[AT + 17..AT + 21].copy_from_slice(&99u32.to_le_bytes());
            }),
            ("config size over the kernel's bound", |b| {
                b[AT + 17..AT + 21].copy_from_slice(&99_999u32.to_le_bytes());
            }),
            ("declares a service image", |b| {
                b[16..20].copy_from_slice(&16u32.to_le_bytes());
            }),
            ("declares a LibC ABI", |b| {
                b[12..16].copy_from_slice(&Version::new(0, 0, 3).packed().to_le_bytes());
            }),
            (
                "icon fields the kernel's fixed offset does not assume",
                |b| {
                    b[40..44].copy_from_slice(&16u32.to_le_bytes());
                },
            ),
            ("targets itself", |b| {
                b[AT + 4..AT + 12].copy_from_slice(&ALIAS_ID.to_le_bytes());
            }),
        ];

        for (what, damage) in cases {
            let mut bytes = make_alias(WALK_CONFIG.as_bytes());
            damage(&mut bytes);
            restamp(&mut bytes);
            let uapp = Uapp::parse(&bytes).expect("{what}: still parses as a container");
            assert!(
                uapp.header().is_variant_alias(),
                "{what}: stopped being an alias"
            );
            assert!(
                uapp.variant().expect("still flagged").is_err(),
                "{what}: read as a well-formed alias"
            );
        }
    }

    #[test]
    fn an_alias_with_no_config_activates_nothing_and_says_so() {
        // The app-side reader treats configSize 0 as "run as the classic self",
        // so an alias like this is inert rather than a variant.
        let bytes = make_alias(b"");
        let uapp = Uapp::parse(&bytes).unwrap();
        assert_eq!(uapp.variant().unwrap(), Err(&VariantError::NoConfig));
    }

    #[test]
    fn a_config_that_is_not_text_is_refused() {
        let bytes = make_alias(&[0xFF, 0xFE, 0xFD]);
        let uapp = Uapp::parse(&bytes).unwrap();
        assert_eq!(uapp.variant().unwrap(), Err(&VariantError::ConfigNotUtf8));
    }

    #[test]
    fn a_damaged_descriptor_never_panics() {
        // A .uapp comes off a USB volume or an upstream zip, so every field here
        // is somebody else's bytes. The reader may report anything it likes about
        // them; what it may not do is come apart.
        let good = make_alias(WALK_CONFIG.as_bytes());

        for cut in 0..good.len() {
            let bytes = &good[..cut];
            if let Ok(uapp) = Uapp::parse(bytes) {
                let _ = uapp.variant();
                let _ = uapp.verify_crc();
            }
        }

        // Every byte of the header and the descriptor, saturated one at a time.
        let region = descriptor_at() + VARIANT_PAYLOAD_LEN;
        for offset in (0..HEADER_LEN).chain(descriptor_at()..region) {
            for value in [0x00u8, 0xFF] {
                let mut bytes = good.clone();
                bytes[offset] = value;
                restamp(&mut bytes);
                if let Ok(uapp) = Uapp::parse(&bytes) {
                    let _ = uapp.variant();
                }
            }
        }
    }
}
