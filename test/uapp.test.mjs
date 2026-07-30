import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  APP_TYPES,
  CRC_SIZE,
  HEADER_SIZE,
  compareVersions,
  crc32,
  decodeIcon,
  formatVersion,
  isBlankIcon,
  NORMAL_ICON_SIZE,
  parseHeader,
  parseUapp,
  parseVersion,
  SMALL_ICON_SIZE,
  verifyCrc,
} from '../src/uapp.js';
import { makeUapp } from './helpers.mjs';

test('crc32 matches the zlib reference vector', () => {
  // zlib.crc32(b"123456789") == 0xCBF43926
  assert.equal(crc32(new TextEncoder().encode('123456789')), 0xcbf43926);
});

test('parses every header field', () => {
  const app = parseUapp(makeUapp());
  assert.equal(app.appId, 'A19C2A7E4F8B6D31');
  assert.equal(app.version, '1.3.0');
  assert.equal(app.libcVersionStr, '0.0.3');
  assert.equal(app.name, 'Alarm');
  assert.equal(app.type, 'Utility');
  assert.equal(app.autostart, true);
  assert.equal(app.glanceCapable, true);
  assert.equal(app.serviceSize, 64);
  assert.equal(app.guiSize, 32);
  assert.equal(app.crc.ok, true);
});

test('derives payload offsets that tile the file exactly', () => {
  const bytes = makeUapp({ serviceSize: 128, guiSize: 256 });
  const app = parseUapp(bytes);
  assert.equal(app.normalIconOffset, HEADER_SIZE);
  assert.equal(app.smallIconOffset, HEADER_SIZE + app.normalIconSize);
  assert.equal(app.serviceOffset, HEADER_SIZE + app.normalIconSize + app.smallIconSize);
  assert.equal(app.guiOffset, app.serviceOffset + app.serviceSize);
  assert.equal(app.guiOffset + app.guiSize + CRC_SIZE, bytes.length);
});

test('a Glance app with no GUI image gives guiSize 0', () => {
  const app = parseUapp(makeUapp({ flags: 0x22, guiSize: 0 }));
  assert.equal(app.type, 'Glance');
  assert.equal(app.guiSize, 0);
});

test('all four app types decode from the low flag bits', () => {
  for (const [index, name] of APP_TYPES.entries()) {
    assert.equal(parseUapp(makeUapp({ flags: index })).type, name);
  }
});

test('a 15-character name with no NUL terminator still parses', () => {
  const app = parseUapp(makeUapp({ name: 'ABCDEFGHIJKLMNO' }));
  assert.equal(app.name, 'ABCDEFGHIJKLMNO');
});

test('display names may contain a path separator', () => {
  // GlanceARHR really ships as "AVG / R HR" — the reason on-device folder names
  // must come from the release layout, never from the header.
  const app = parseUapp(makeUapp({ name: 'AVG / R HR' }));
  assert.equal(app.name, 'AVG / R HR');
  assert.match(app.name, /\//);
});

test('a corrupted footer is reported, not thrown', () => {
  const app = parseUapp(makeUapp({ breakCrc: true }));
  assert.equal(app.crc.ok, false);
  assert.equal(app.crc.stored, 0xdeadbeef);
  assert.notEqual(app.crc.computed, 0xdeadbeef);
});

test('a header-only slice parses when the total size is supplied', () => {
  const bytes = makeUapp();
  const head = parseHeader(bytes.subarray(0, HEADER_SIZE), bytes.length);
  assert.equal(head.name, 'Alarm');
  assert.equal(head.guiSize, 32);
});

test('a header-only slice leaves guiSize unknown without a total size', () => {
  const head = parseHeader(makeUapp().subarray(0, HEADER_SIZE));
  assert.equal(head.guiSize, null);
});

test('rejects input too short to hold a header', () => {
  assert.throws(() => parseHeader(new Uint8Array(HEADER_SIZE - 1)), /too short/);
});

test('rejects a file smaller than its header claims', () => {
  const bytes = makeUapp({ serviceSize: 4096 });
  assert.throws(() => parseHeader(bytes.subarray(0, HEADER_SIZE), 512), /truncated/);
});

test('verifyCrc refuses a file with no room for a footer', () => {
  const result = verifyCrc(new Uint8Array(8));
  assert.equal(result.ok, false);
  assert.match(result.reason, /too short/);
});

test('accepts ArrayBuffer and DataView as well as Uint8Array', () => {
  const bytes = makeUapp();
  assert.equal(parseUapp(bytes.buffer).name, 'Alarm');
  assert.equal(parseHeader(new DataView(bytes.buffer)).name, 'Alarm');
});

test('parses a header at a non-zero offset in a larger buffer', () => {
  const bytes = makeUapp();
  const padded = new Uint8Array(bytes.length + 7);
  padded.set(bytes, 7);
  const slice = padded.subarray(7);
  assert.equal(parseHeader(slice, bytes.length).appId, 'A19C2A7E4F8B6D31');
  assert.equal(verifyCrc(slice).ok, true);
});

test('version packing round-trips', () => {
  assert.equal(formatVersion(0x00010300), '1.3.0');
  assert.equal(formatVersion(0x00000003), '0.0.3');
  assert.equal(parseVersion('1.3.0'), 0x00010300);
  assert.equal(parseVersion('v1.3.0-rc4'), 0x00010300);
  assert.equal(parseVersion('nonsense'), null);
  assert.equal(parseVersion('1.3.256'), null);
  assert.ok(compareVersions(parseVersion('1.3.0'), parseVersion('1.2.0')) > 0);
  assert.ok(compareVersions(parseVersion('1.2.0'), parseVersion('1.3.0')) < 0);
  assert.equal(compareVersions(parseVersion('1.3.0'), parseVersion('1.3.0')), 0);
});

test('decodes ABGR2222 pixels to RGBA', () => {
  // 0xFF = all channels 3 -> opaque white; 0xC0 = alpha 3, colours 0 -> black.
  const { width, height, rgba } = decodeIcon(Uint8Array.from([0xff, 0xc0, 0xc0, 0xff]));
  assert.equal(width, 2);
  assert.equal(height, 2);
  assert.deepEqual([...rgba.subarray(0, 4)], [255, 255, 255, 255]);
  assert.deepEqual([...rgba.subarray(4, 8)], [0, 0, 0, 255]);
});

test('rejects a non-square icon', () => {
  assert.throws(() => decodeIcon(new Uint8Array(5)), /not square/);
});

test('a zero-filled icon field is recognised as no icon at all', () => {
  // Every Glance app in apps-v1.3.0 declares 3600/900-byte icons that are
  // entirely zeros, so size alone must never be read as "has an icon".
  assert.equal(isBlankIcon(new Uint8Array(NORMAL_ICON_SIZE)), true);
  assert.equal(isBlankIcon(new Uint8Array(SMALL_ICON_SIZE)), true);
});

test('an icon with any opaque pixel is not blank', () => {
  const icon = new Uint8Array(NORMAL_ICON_SIZE);
  icon[1234] = 0x40; // alpha 1, colours 0
  assert.equal(isBlankIcon(icon), false);
});

test('colour bits alone do not make an icon visible', () => {
  // Fully transparent pixels with colour set still render as nothing.
  const icon = new Uint8Array(NORMAL_ICON_SIZE).fill(0x3f); // alpha 0, RGB max
  assert.equal(isBlankIcon(icon), true);
});
