// Validates sprite packs against docs/sprite-pack.md and the bundled chime.
//
//   npm run check:sprites                 # checks assets/sprites/default
//   node tools/check-sprites.mjs <dir>... # checks other pack folders
//
// pack.json is validated against assets/sprites/pack.schema.json with a tiny
// built-in validator (only the keywords that schema uses), then the PNG strips
// are checked against the declared frame size and frame count.
// Pure Node, no npm dependencies. Exits 1 on any error.

import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { readPngHeader } from "./lib/png.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SCHEMA_PATH = join(ROOT, "assets", "sprites", "pack.schema.json");
const CHIME_PATH = join(ROOT, "assets", "sounds", "chime.wav");
const STATES = ["idle", "sleeping", "thinking", "alert"];

// --- Minimal JSON Schema validator ------------------------------------------

const SUPPORTED = new Set([
  "$schema", "$id", "$defs", "$ref", "title", "description",
  "type", "required", "properties", "additionalProperties",
  "const", "enum", "minLength", "maxLength", "minimum", "maximum", "pattern",
  "items", "minItems", "maxItems",
]);

function typeOf(v) {
  if (v === null) return "null";
  if (Array.isArray(v)) return "array";
  if (typeof v === "number") return Number.isInteger(v) ? "integer" : "number";
  return typeof v;
}

function validate(schema, value, path, root, errors) {
  for (const key of Object.keys(schema)) {
    if (!SUPPORTED.has(key)) throw new Error(`check-sprites: schema keyword "${key}" not supported by the built-in validator`);
  }
  if (schema.$ref) {
    const m = /^#\/\$defs\/(.+)$/.exec(schema.$ref);
    if (!m || !root.$defs?.[m[1]]) throw new Error(`check-sprites: unresolvable $ref ${schema.$ref}`);
    return validate(root.$defs[m[1]], value, path, root, errors);
  }
  const t = typeOf(value);
  if (schema.type) {
    const ok = schema.type === t || (schema.type === "number" && t === "integer");
    if (!ok) return errors.push(`${path}: expected ${schema.type}, got ${t}`);
  }
  if ("const" in schema && value !== schema.const) errors.push(`${path}: must be ${JSON.stringify(schema.const)}`);
  if (schema.enum && !schema.enum.includes(value)) errors.push(`${path}: must be one of ${JSON.stringify(schema.enum)}`);
  if (t === "array") {
    if (schema.minItems !== undefined && value.length < schema.minItems) errors.push(`${path}: fewer than ${schema.minItems} items`);
    if (schema.maxItems !== undefined && value.length > schema.maxItems) errors.push(`${path}: more than ${schema.maxItems} items`);
    if (schema.items) value.forEach((v, i) => validate(schema.items, v, `${path}[${i}]`, root, errors));
  }
  if (t === "string") {
    if (schema.minLength !== undefined && value.length < schema.minLength) errors.push(`${path}: shorter than ${schema.minLength}`);
    if (schema.maxLength !== undefined && value.length > schema.maxLength) errors.push(`${path}: longer than ${schema.maxLength}`);
    if (schema.pattern && !new RegExp(schema.pattern, "u").test(value)) errors.push(`${path}: does not match ${schema.pattern}`);
  }
  if (t === "integer" || t === "number") {
    if (schema.minimum !== undefined && value < schema.minimum) errors.push(`${path}: below minimum ${schema.minimum}`);
    if (schema.maximum !== undefined && value > schema.maximum) errors.push(`${path}: above maximum ${schema.maximum}`);
  }
  if (t === "object") {
    for (const req of schema.required ?? []) if (!(req in value)) errors.push(`${path}: missing "${req}"`);
    const props = schema.properties ?? {};
    for (const [k, v] of Object.entries(value)) {
      if (props[k]) validate(props[k], v, `${path}.${k}`, root, errors);
      else if (schema.additionalProperties === false) errors.push(`${path}: unknown property "${k}"`);
    }
  }
}

// --- Pack checks ---------------------------------------------------------------

function checkPack(dir, schema) {
  const errors = [];
  const packPath = join(dir, "pack.json");
  if (!existsSync(packPath)) return [`${packPath}: not found`];
  let pack;
  try {
    pack = JSON.parse(readFileSync(packPath, "utf8"));
  } catch (e) {
    return [`${packPath}: invalid JSON (${e.message})`];
  }
  validate(schema, pack, "pack", schema, errors);
  if (errors.length) return errors;

  const { width: fw, height: fh } = pack.frameSize;
  for (const name of STATES) {
    const st = pack.states[name];
    if (st.durations !== undefined) {
      if (pack.format < 2) errors.push(`states.${name}: durations needs "format": 2`);
      if (st.durations.length !== st.frames) {
        errors.push(`states.${name}: durations has ${st.durations.length} entries, expected one per frame (${st.frames})`);
      }
    }
    const file = join(dir, st.file);
    if (!existsSync(file)) {
      errors.push(`states.${name}: file ${st.file} not found`);
      continue;
    }
    let hdr;
    try {
      hdr = readPngHeader(readFileSync(file));
    } catch (e) {
      errors.push(`states.${name}: ${st.file}: ${e.message}`);
      continue;
    }
    if (hdr.width !== fw * st.frames) {
      errors.push(`states.${name}: ${st.file} is ${hdr.width}px wide, expected frameSize.width x frames = ${fw} x ${st.frames} = ${fw * st.frames}`);
    }
    if (hdr.height !== fh) errors.push(`states.${name}: ${st.file} is ${hdr.height}px high, expected frameSize.height = ${fh}`);
  }
  return errors;
}

function checkChime() {
  if (!existsSync(CHIME_PATH)) return [`${CHIME_PATH}: not found`];
  const b = readFileSync(CHIME_PATH);
  if (b.length < 44 || b.toString("ascii", 0, 4) !== "RIFF" || b.toString("ascii", 8, 12) !== "WAVE") return ["chime.wav: not a RIFF/WAVE file"];
  const channels = b.readUInt16LE(22);
  const rate = b.readUInt32LE(24);
  const bits = b.readUInt16LE(34);
  const seconds = b.readUInt32LE(40) / (rate * channels * (bits / 8));
  const errors = [];
  if (b.readUInt16LE(20) !== 1 || channels !== 1 || rate !== 22050 || bits !== 16) errors.push("chime.wav: expected mono 22.05 kHz 16-bit PCM");
  if (seconds > 1) errors.push(`chime.wav: ${seconds.toFixed(2)} s, expected at most 1 s`);
  return errors;
}

function main() {
  const schema = JSON.parse(readFileSync(SCHEMA_PATH, "utf8"));
  const args = process.argv.slice(2);
  const dirs = args.length ? args.map((a) => resolve(a)) : [join(ROOT, "assets", "sprites", "default")];
  let failed = false;
  for (const dir of dirs) {
    const errors = checkPack(dir, schema);
    if (errors.length) {
      failed = true;
      console.error(`FAIL ${dir}`);
      for (const e of errors) console.error(`  - ${e}`);
    } else {
      console.log(`ok   ${dir}`);
    }
  }
  if (!args.length) {
    const errors = checkChime();
    if (errors.length) {
      failed = true;
      for (const e of errors) console.error(`FAIL ${e}`);
    } else {
      console.log(`ok   ${CHIME_PATH}`);
    }
  }
  process.exit(failed ? 1 : 0);
}

main();
