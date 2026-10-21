// Generate the icon assets Tauri needs (pure Node, no third-party packages).
//   usage:  node tools/gen-icons.mjs
//   output: src-tauri/icons/{icon.ico,32x32.png,128x128.png,128x128@2x.png,icon.png}
import { deflateSync } from 'node:zlib';
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const outDir = join(root, 'src-tauri', 'icons');

// ---------------------------------------------------------------- geometry & colors
const SS = 4; // 4x4 supersampling per pixel, for anti-aliased edges

function insideRoundRect(x, y, size, r) {
  const qx = Math.abs(x - size / 2) - (size / 2 - r);
  const qy = Math.abs(y - size / 2) - (size / 2 - r);
  if (qx <= 0 && qy <= 0) return true;
  return Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) <= r;
}

function distToSegment(px, py, ax, ay, bx, by) {
  const vx = bx - ax;
  const vy = by - ay;
  const wx = px - ax;
  const wy = py - ay;
  const l2 = vx * vx + vy * vy;
  const u = l2 > 0 ? Math.min(1, Math.max(0, (wx * vx + wy * vy) / l2)) : 0;
  return Math.hypot(px - (ax + u * vx), py - (ay + u * vy));
}

/** Produce a size x size RGBA buffer (rounded gradient background + a white ">" arrow). */
function renderRGBA(size) {
  const px = Buffer.alloc(size * size * 4);
  const radius = size * 0.23;
  const half = size * 0.065; // half the arrow stroke width
  const a = [size * 0.33, size * 0.27];
  const b = [size * 0.65, size * 0.5];
  const c = [size * 0.33, size * 0.73];

  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      let bgCov = 0;
      let fgCov = 0;
      for (let sy = 0; sy < SS; sy++) {
        for (let sx = 0; sx < SS; sx++) {
          const fx = x + (sx + 0.5) / SS;
          const fy = y + (sy + 0.5) / SS;
          const inBg = insideRoundRect(fx, fy, size, radius);
          if (!inBg) continue;
          bgCov++;
          const d = Math.min(
            distToSegment(fx, fy, a[0], a[1], b[0], b[1]),
            distToSegment(fx, fy, b[0], b[1], c[0], c[1]),
          );
          if (d <= half) fgCov++;
        }
      }
      const total = SS * SS;
      const bg = bgCov / total;
      const fg = bgCov > 0 ? fgCov / bgCov : 0;

      // diagonal gradient: #4c8dff -> #123a8f
      const g = (x + y) / (2 * size);
      const br = Math.round(76 + (18 - 76) * g);
      const bgc = Math.round(141 + (58 - 141) * g);
      const bb = Math.round(255 + (143 - 255) * g);

      const r = Math.round(br * (1 - fg) + 255 * fg);
      const gg = Math.round(bgc * (1 - fg) + 255 * fg);
      const bl = Math.round(bb * (1 - fg) + 255 * fg);

      const i = (y * size + x) * 4;
      px[i] = r;
      px[i + 1] = gg;
      px[i + 2] = bl;
      px[i + 3] = Math.round(bg * 255);
    }
  }
  return px;
}

// ------------------------------------------------------------------- PNG
const CRC_TABLE = (() => {
  const t = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c;
  }
  return t;
})();

function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function pngChunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body), 0);
  return Buffer.concat([len, body, crc]);
}

function encodePNG(rgba, size) {
  const raw = Buffer.alloc((size * 4 + 1) * size);
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0; // filter: none
    rgba.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // color type: RGBA
  ihdr[10] = 0;
  ihdr[11] = 0;
  ihdr[12] = 0;
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    pngChunk('IHDR', ihdr),
    pngChunk('IDAT', deflateSync(raw, { level: 9 })),
    pngChunk('IEND', Buffer.alloc(0)),
  ]);
}

// ------------------------------------------------------------------- ICO
/** One ICO entry: a 32bpp BMP (XOR bitmap + AND mask), matching the traditional Windows format. */
function icoBmpEntry(rgba, size) {
  const header = Buffer.alloc(40);
  header.writeUInt32LE(40, 0);
  header.writeInt32LE(size, 4);
  header.writeInt32LE(size * 2, 8); // XOR + AND
  header.writeUInt16LE(1, 12);
  header.writeUInt16LE(32, 14);
  header.writeUInt32LE(0, 16); // BI_RGB
  header.writeUInt32LE(size * size * 4, 20);

  const xor = Buffer.alloc(size * size * 4);
  for (let y = 0; y < size; y++) {
    const srcRow = size - 1 - y; // BMP is stored bottom-up
    for (let x = 0; x < size; x++) {
      const s = (srcRow * size + x) * 4;
      const d = (y * size + x) * 4;
      xor[d] = rgba[s + 2]; // B
      xor[d + 1] = rgba[s + 1]; // G
      xor[d + 2] = rgba[s]; // R
      xor[d + 3] = rgba[s + 3]; // A
    }
  }

  const maskRow = Math.ceil(size / 32) * 4;
  const mask = Buffer.alloc(maskRow * size);
  for (let y = 0; y < size; y++) {
    const srcRow = size - 1 - y;
    for (let x = 0; x < size; x++) {
      const alpha = rgba[(srcRow * size + x) * 4 + 3];
      if (alpha < 128) mask[y * maskRow + (x >> 3)] |= 0x80 >> (x & 7);
    }
  }
  return Buffer.concat([header, xor, mask]);
}

function encodeICO(entries) {
  const dir = Buffer.alloc(6);
  dir.writeUInt16LE(0, 0);
  dir.writeUInt16LE(1, 2);
  dir.writeUInt16LE(entries.length, 4);

  const table = Buffer.alloc(16 * entries.length);
  let offset = 6 + 16 * entries.length;
  entries.forEach((e, i) => {
    const o = i * 16;
    table[o] = e.size >= 256 ? 0 : e.size;
    table[o + 1] = e.size >= 256 ? 0 : e.size;
    table[o + 2] = 0;
    table[o + 3] = 0;
    table.writeUInt16LE(1, o + 4);
    table.writeUInt16LE(32, o + 6);
    table.writeUInt32LE(e.data.length, o + 8);
    table.writeUInt32LE(offset, o + 12);
    offset += e.data.length;
  });

  return Buffer.concat([dir, table, ...entries.map((e) => e.data)]);
}

// ------------------------------------------------------------------- ICNS (macOS)
/** ICNS container: each chunk = type (4B) + length (4B, including the header) + PNG data. */
function encodeICNS(entries) {
  const chunks = entries.map(({ type, png }) => {
    const head = Buffer.alloc(8);
    head.write(type, 0, 4, 'ascii');
    head.writeUInt32BE(png.length + 8, 4);
    return Buffer.concat([head, png]);
  });
  const body = Buffer.concat(chunks);
  const head = Buffer.alloc(8);
  head.write('icns', 0, 4, 'ascii');
  head.writeUInt32BE(body.length + 8, 4);
  return Buffer.concat([head, body]);
}

// ------------------------------------------------------------------- output
mkdirSync(outDir, { recursive: true });

const cache = new Map();
const rgbaFor = (size) => {
  if (!cache.has(size)) cache.set(size, renderRGBA(size));
  return cache.get(size);
};
const pngFor = (size) => encodePNG(rgbaFor(size), size);

// Linux / generic PNG
for (const size of [32, 128, 512]) {
  writeFileSync(join(outDir, `${size}x${size}.png`), pngFor(size));
}
writeFileSync(join(outDir, '128x128@2x.png'), pngFor(256));
writeFileSync(join(outDir, 'icon.png'), pngFor(512));

// Windows ICO (32bpp BMP entries, best compatibility)
const icoSizes = [16, 32, 48, 64, 128, 256];
writeFileSync(
  join(outDir, 'icon.ico'),
  encodeICO(icoSizes.map((size) => ({ size, data: icoBmpEntry(rgbaFor(size), size) }))),
);

// macOS ICNS (all PNG entries)
writeFileSync(
  join(outDir, 'icon.icns'),
  encodeICNS([
    { type: 'ic11', png: pngFor(32) },
    { type: 'ic12', png: pngFor(64) },
    { type: 'ic07', png: pngFor(128) },
    { type: 'ic08', png: pngFor(256) },
    { type: 'ic09', png: pngFor(512) },
    { type: 'ic10', png: pngFor(1024) },
  ]),
);

console.log(`icons written to ${outDir}`);
