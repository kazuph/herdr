import { deflateSync } from "zlib";
import { writeFileSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";

// Hand-rolled PNG writer so the demo has a real image to render without
// shipping binary assets or an image dependency.

const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});

function crc32(buf: Buffer): number {
  let c = 0xffffffff;
  for (const byte of buf) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type: string, data: Buffer): Buffer {
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const out = Buffer.alloc(body.length + 8);
  out.writeUInt32BE(data.length, 0);
  body.copy(out, 4);
  out.writeUInt32BE(crc32(body), body.length + 4);
  return out;
}

export function writeDemoImage(): string {
  const w = 320;
  const h = 128;
  const raw = Buffer.alloc(h * (1 + w * 4));
  for (let y = 0; y < h; y++) {
    const row = y * (1 + w * 4) + 1;
    for (let x = 0; x < w; x++) {
      const u = x / w;
      const v = y / h;
      const wave = Math.sin(u * 9 + v * 3) * 0.5 + 0.5;
      const glow = Math.exp(-((u - 0.72) ** 2 + (v - 0.35) ** 2) * 6);
      const i = row + x * 4;
      raw[i] = Math.round(40 + 130 * wave * (1 - v) + 80 * glow);
      raw[i + 1] = Math.round(30 + 60 * v + 90 * glow);
      raw[i + 2] = Math.round(90 + 120 * (1 - wave) + 60 * glow);
      raw[i + 3] = 255;
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // rgba
  const png = Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
  const path = join(tmpdir(), "pixel-markdown-demo.png");
  writeFileSync(path, png);
  return path;
}
