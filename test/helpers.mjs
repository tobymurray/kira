/** Synthesise a valid .uapp so tests need no vendor binaries. */

import { crc32, HEADER_SIZE, NORMAL_ICON_SIZE, SMALL_ICON_SIZE } from '../src/uapp.js';

export function makeUapp({
  appId = 'A19C2A7E4F8B6D31',
  version = 0x00010300, // 1.3.0
  libcVersion = 0x00000003, // 0.0.3
  flags = 0x00000029, // Utility + autostart + glance-capable
  name = 'Alarm',
  serviceSize = 64,
  guiSize = 32,
  normalIconSize = NORMAL_ICON_SIZE,
  smallIconSize = SMALL_ICON_SIZE,
  breakCrc = false,
} = {}) {
  const total = HEADER_SIZE + normalIconSize + smallIconSize + serviceSize + guiSize + 4;
  const bytes = new Uint8Array(total);
  const view = new DataView(bytes.buffer);

  view.setBigUint64(0, BigInt(`0x${appId}`), true);
  view.setUint32(8, version, true);
  view.setUint32(12, libcVersion, true);
  view.setUint32(16, serviceSize, true);
  view.setUint32(20, flags, true);
  bytes.set(new TextEncoder().encode(name).subarray(0, 15), 24);
  view.setUint32(40, normalIconSize, true);
  view.setUint32(44, smallIconSize, true);

  // Recognisable payload so offset bugs surface as wrong bytes, not zeros.
  for (let i = HEADER_SIZE; i < total - 4; i++) bytes[i] = i & 0xff;

  view.setUint32(total - 4, breakCrc ? 0xdeadbeef : crc32(bytes.subarray(0, total - 4)), true);
  return bytes;
}

/** A catalogue entry shaped like build-catalog.mjs emits. */
export function catalogEntry(over = {}) {
  return {
    appId: 'A19C2A7E4F8B6D31',
    name: 'Alarm',
    folder: 'Alarm',
    file: 'Alarm_1.3.0.uapp',
    version: '1.3.0',
    versionPacked: 0x00010300,
    libcVersion: '0.0.3',
    type: 'Utility',
    autostart: true,
    size: 210628,
    sha256: 'a'.repeat(64),
    icon: 'icons/A19C2A7E4F8B6D31.png',
    download: 'apps/Alarm/Alarm_1.3.0.uapp',
    ...over,
  };
}

export function installedEntry(over = {}) {
  return {
    appId: 'A19C2A7E4F8B6D31',
    folder: 'Alarm',
    file: 'Alarm_1.3.0.uapp',
    name: 'Alarm',
    version: '1.3.0',
    versionPacked: 0x00010300,
    size: 210628,
    extraUapps: [],
    ...over,
  };
}
