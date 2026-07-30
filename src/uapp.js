/**
 * Parser for the UNA Watch `.uapp` container.
 *
 * Layout (little-endian), per app_merging.py in the UNA SDK:
 *
 *   offset  size  field
 *   0       8     AppID          u64
 *   8       4     AppVersion     u32  0x00AABBCC = A.B.C
 *   12      4     LibCVersion    u32  ABI the app was linked against
 *   16      4     serviceSize    u32  bytes of the service image
 *   20      4     flags          u32  see FLAGS
 *   24      16    AppName        char[16], NUL-padded, max 15 chars
 *   40      4     normalIconSize u32  60x60 ABGR2222 = 3600
 *   44      4     smallIconSize  u32  30x30 ABGR2222 = 900
 *   48            normal icon, small icon, service image, GUI image
 *   len-4   4     CRC32          u32  over everything preceding it
 *
 * The GUI image is absent for Glance apps, so its size is whatever is left
 * between the service image and the CRC footer.
 *
 * This module is dependency-free ESM so the identical parser runs in the
 * catalogue build (Node) and in the browser when reading a connected watch.
 */

export const HEADER_SIZE = 48;
export const CRC_SIZE = 4;
export const NORMAL_ICON_SIZE = 60 * 60;
export const SMALL_ICON_SIZE = 30 * 30;

/** Low 2 bits of `flags`. Index matches APP_TYPES in app_merging.py. */
export const APP_TYPES = ['Activity', 'Utility', 'Glance', 'Clockface'];

export const FLAGS = {
  TYPE_MASK: 0x3,
  AUTOSTART: 0x08,
  /**
   * Bit 5. Note: `una_app_build_app()` in cmake/una-app.cmake passes
   * -glance_capable unconditionally, so every officially built app has this
   * set. It carries no information in practice — do not surface it as a
   * feature.
   */
  GLANCE_CAPABLE: 0x20,
};

let crcTable;

/** CRC-32/ISO-HDLC — the same polynomial Python's zlib.crc32 uses. */
export function crc32(bytes, seed = 0) {
  if (!crcTable) {
    crcTable = new Uint32Array(256);
    for (let n = 0; n < 256; n++) {
      let c = n;
      for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
      crcTable[n] = c >>> 0;
    }
  }
  let c = (seed ^ 0xffffffff) >>> 0;
  for (let i = 0; i < bytes.length; i++) {
    c = (crcTable[(c ^ bytes[i]) & 0xff] ^ (c >>> 8)) >>> 0;
  }
  return (c ^ 0xffffffff) >>> 0;
}

/** 0x00010300 -> "1.3.0" */
export function formatVersion(packed) {
  return `${(packed >>> 16) & 0xff}.${(packed >>> 8) & 0xff}.${packed & 0xff}`;
}

/** "1.3.0" -> 0x00010300. Returns null if unparseable. */
export function parseVersion(str) {
  const m = /^v?(\d+)\.(\d+)\.(\d+)/.exec(String(str).trim());
  if (!m) return null;
  const [a, b, c] = m.slice(1).map(Number);
  if ([a, b, c].some((v) => v > 255)) return null;
  return ((a << 16) | (b << 8) | c) >>> 0;
}

/** Compare two packed versions. Negative if a is older. */
export function compareVersions(a, b) {
  return a - b;
}

function toU8(input) {
  if (input instanceof Uint8Array) return input;
  if (input instanceof ArrayBuffer) return new Uint8Array(input);
  if (ArrayBuffer.isView(input)) {
    return new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
  }
  throw new TypeError('expected Uint8Array, ArrayBuffer or typed array');
}

/**
 * Parse the fixed 48-byte header.
 *
 * Only the first HEADER_SIZE bytes are read, so callers holding a huge file can
 * pass a 48-byte slice and skip reading the payload. Pass `totalSize` (the full
 * file length) to additionally derive `guiSize` and check internal consistency.
 */
export function parseHeader(input, totalSize = null) {
  const u8 = toU8(input);
  if (u8.length < HEADER_SIZE) {
    throw new Error(`too short for a .uapp header: ${u8.length} < ${HEADER_SIZE}`);
  }
  const view = new DataView(u8.buffer, u8.byteOffset, u8.byteLength);

  const appId = view.getBigUint64(0, true).toString(16).toUpperCase().padStart(16, '0');
  const appVersion = view.getUint32(8, true);
  const libcVersion = view.getUint32(12, true);
  const serviceSize = view.getUint32(16, true);
  const flags = view.getUint32(20, true);

  const nameBytes = u8.subarray(24, 40);
  const nul = nameBytes.indexOf(0);
  const name = new TextDecoder('utf-8')
    .decode(nul === -1 ? nameBytes : nameBytes.subarray(0, nul))
    .trim();

  const normalIconSize = view.getUint32(40, true);
  const smallIconSize = view.getUint32(44, true);

  const header = {
    appId,
    appVersion,
    version: formatVersion(appVersion),
    libcVersion,
    libcVersionStr: formatVersion(libcVersion),
    serviceSize,
    flags,
    type: APP_TYPES[flags & FLAGS.TYPE_MASK],
    autostart: (flags & FLAGS.AUTOSTART) !== 0,
    glanceCapable: (flags & FLAGS.GLANCE_CAPABLE) !== 0,
    name,
    normalIconSize,
    smallIconSize,
    // Payload offsets, for slicing icons out without a full parse.
    normalIconOffset: HEADER_SIZE,
    smallIconOffset: HEADER_SIZE + normalIconSize,
    serviceOffset: HEADER_SIZE + normalIconSize + smallIconSize,
    guiSize: null,
  };

  if (totalSize !== null) {
    const fixed = HEADER_SIZE + normalIconSize + smallIconSize + serviceSize + CRC_SIZE;
    header.guiSize = totalSize - fixed;
    if (header.guiSize < 0) {
      throw new Error(
        `header describes ${fixed} bytes but the file is ${totalSize} — not a .uapp, or truncated`,
      );
    }
    header.guiOffset = header.serviceOffset + serviceSize;
  }

  if (!header.type) throw new Error(`unknown app type in flags 0x${flags.toString(16)}`);
  return header;
}

/**
 * Verify the CRC32 footer. The watch kernel drops a `.uapp` that fails this
 * *silently* — the app simply never appears in the launcher — so always check
 * before writing one to a device.
 */
export function verifyCrc(input) {
  const u8 = toU8(input);
  if (u8.length < HEADER_SIZE + CRC_SIZE) {
    return { ok: false, stored: null, computed: null, reason: 'file too short' };
  }
  const body = u8.subarray(0, u8.length - CRC_SIZE);
  const view = new DataView(u8.buffer, u8.byteOffset, u8.byteLength);
  const stored = view.getUint32(u8.length - CRC_SIZE, true);
  const computed = crc32(body);
  return { ok: stored === computed, stored, computed };
}

/**
 * The bytes between the header and the CRC footer: icons, service image and GUI
 * image. This is the app itself, with no version stamp in it.
 *
 * Hashing this rather than the whole file distinguishes "the code changed" from
 * "the release tag moved". App versions are stamped from the `apps-v*` tag and
 * applied to every app in the release, so a version bump alone says nothing
 * about whether a given app changed — in apps-v1.3.0, six of the thirteen were
 * byte-identical to their apps-v1.2.0 builds.
 */
export function payloadOf(input) {
  const u8 = toU8(input);
  if (u8.length < HEADER_SIZE + CRC_SIZE) {
    throw new Error(`too short to hold a payload: ${u8.length}`);
  }
  return u8.subarray(HEADER_SIZE, u8.length - CRC_SIZE);
}

/** Full parse: header, derived sizes, and CRC verification. */
export function parseUapp(input) {
  const u8 = toU8(input);
  const header = parseHeader(u8, u8.length);
  const crc = verifyCrc(u8);
  return { ...header, size: u8.length, crc };
}

/**
 * True if every pixel is fully transparent, i.e. there is no image here.
 *
 * Glance apps built with icons off still carry correctly *sized* icon fields:
 * app_merging.py's convert_icon_or_zeros() zero-fills them rather than writing
 * a zero length. All six Glance apps in apps-v1.3.0 are like this, so a non-zero
 * icon size is not evidence of an icon.
 */
export function isBlankIcon(input) {
  const u8 = toU8(input);
  for (let i = 0; i < u8.length; i++) {
    if ((u8[i] >> 6) & 0x03) return false; // any non-zero alpha
  }
  return true;
}

/**
 * Unpack an ABGR2222 icon (1 byte per pixel) to RGBA8. Square images only,
 * which is what app_merging.py enforces.
 */
export function decodeIcon(input) {
  const u8 = toU8(input);
  const side = Math.sqrt(u8.length);
  if (!Number.isInteger(side)) {
    throw new Error(`icon of ${u8.length} bytes is not square`);
  }
  const rgba = new Uint8Array(u8.length * 4);
  for (let i = 0; i < u8.length; i++) {
    const px = u8[i];
    // 2 bits per channel, packed (a<<6)|(b<<4)|(g<<2)|r. Scale 0..3 -> 0..255.
    rgba[i * 4 + 0] = (px & 0x03) * 85;
    rgba[i * 4 + 1] = ((px >> 2) & 0x03) * 85;
    rgba[i * 4 + 2] = ((px >> 4) & 0x03) * 85;
    rgba[i * 4 + 3] = ((px >> 6) & 0x03) * 85;
  }
  return { width: side, height: side, rgba };
}
