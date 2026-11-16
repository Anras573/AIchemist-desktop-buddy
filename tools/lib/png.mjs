// Minimal PNG encoder and header reader. Pure Node (zlib only), no npm deps.
//
// Output is deterministic: no timestamps or text chunks are written, and the
// zlib level and strategy are fixed.

import { deflateSync } from "node:zlib";

const SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(buf) {
  let c = 0xffffffff;
  for (const byte of buf) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
}

function ihdr(width, height, colorType) {
  const data = Buffer.alloc(13);
  data.writeUInt32BE(width, 0);
  data.writeUInt32BE(height, 4);
  data[8] = 8; // bit depth
  data[9] = colorType;
  data[10] = 0; // compression
  data[11] = 0; // filter
  data[12] = 0; // interlace
  return chunk("IHDR", data);
}

function idat(rows) {
  return chunk("IDAT", deflateSync(rows, { level: 9, strategy: 0 }));
}

/**
 * Encode an 8-bit indexed PNG.
 * @param {number} width
 * @param {number} height
 * @param {Uint8Array} indices width*height palette indices
 * @param {Array<[number, number, number, number]>} palette RGBA entries; alpha 0 = transparent
 */
export function encodeIndexedPng(width, height, indices, palette) {
  const plte = Buffer.alloc(palette.length * 3);
  const trns = Buffer.alloc(palette.length);
  palette.forEach(([r, g, b, a], i) => {
    plte[i * 3] = r;
    plte[i * 3 + 1] = g;
    plte[i * 3 + 2] = b;
    trns[i] = a;
  });
  const rows = Buffer.alloc((width + 1) * height);
  for (let y = 0; y < height; y++) {
    rows[y * (width + 1)] = 0; // filter: none
    for (let x = 0; x < width; x++) rows[y * (width + 1) + 1 + x] = indices[y * width + x];
  }
  return Buffer.concat([
    SIGNATURE,
    ihdr(width, height, 3),
    chunk("PLTE", plte),
    chunk("tRNS", trns),
    idat(rows),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

/** Read width, height, bit depth and colour type from a PNG's IHDR. Throws if not a PNG. */
export function readPngHeader(buf) {
  if (buf.length < 33 || !buf.subarray(0, 8).equals(SIGNATURE)) throw new Error("not a PNG file");
  if (buf.toString("ascii", 12, 16) !== "IHDR") throw new Error("PNG is missing IHDR");
  return {
    width: buf.readUInt32BE(16),
    height: buf.readUInt32BE(20),
    bitDepth: buf[24],
    colorType: buf[25],
  };
}
