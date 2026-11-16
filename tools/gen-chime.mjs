// Generates the placeholder reminder chime: assets/sounds/chime.wav.
//
//   npm run gen:chime
//
// Two soft sine notes (A5 then E6) with a short attack and exponential decay,
// mono, 22.05 kHz, 16-bit PCM, 0.8 s. Pure Node, deterministic output.

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const OUT = join(ROOT, "assets", "sounds", "chime.wav");

const RATE = 22050;
const DURATION = 0.8; // seconds
const PEAK = 0.3; // fraction of full scale: soft, never clipping
const NOTES = [
  { freq: 880.0, start: 0.0 }, // A5
  { freq: 1318.51, start: 0.16 }, // E6
];
const ATTACK = 0.008; // seconds; avoids a click at note onset
const DECAY = 0.16; // exponential decay time constant (seconds)
const TAIL_FADE = 0.05; // linear fade to exact silence at the end

function sample(t) {
  let v = 0;
  for (const { freq, start } of NOTES) {
    const nt = t - start;
    if (nt < 0) continue;
    const env = Math.min(1, nt / ATTACK) * Math.exp(-nt / DECAY);
    v += env * Math.sin(2 * Math.PI * freq * nt);
  }
  const fadeStart = DURATION - TAIL_FADE;
  if (t > fadeStart) v *= Math.max(0, (DURATION - t) / TAIL_FADE);
  return v * PEAK;
}

function main() {
  const n = Math.round(RATE * DURATION);
  const data = Buffer.alloc(n * 2);
  for (let i = 0; i < n; i++) {
    const s = Math.max(-1, Math.min(1, sample(i / RATE)));
    data.writeInt16LE(Math.round(s * 32767), i * 2);
  }

  const header = Buffer.alloc(44);
  header.write("RIFF", 0, "ascii");
  header.writeUInt32LE(36 + data.length, 4);
  header.write("WAVE", 8, "ascii");
  header.write("fmt ", 12, "ascii");
  header.writeUInt32LE(16, 16); // fmt chunk size
  header.writeUInt16LE(1, 20); // PCM
  header.writeUInt16LE(1, 22); // mono
  header.writeUInt32LE(RATE, 24);
  header.writeUInt32LE(RATE * 2, 28); // byte rate
  header.writeUInt16LE(2, 32); // block align
  header.writeUInt16LE(16, 34); // bits per sample
  header.write("data", 36, "ascii");
  header.writeUInt32LE(data.length, 40);

  mkdirSync(dirname(OUT), { recursive: true });
  writeFileSync(OUT, Buffer.concat([header, data]));
  console.log(`Wrote ${OUT} (${n} samples, ${DURATION} s)`);
}

main();
