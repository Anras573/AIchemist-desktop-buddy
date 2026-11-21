// Keeps the app version the same everywhere it is declared (docs/releasing.md).
//
//   node tools/bump-version.mjs 0.2.0          # set the version in every file
//   node tools/bump-version.mjs --check        # fail unless all files agree
//   node tools/bump-version.mjs --check v0.2.0 # ...and match this tag
//
// Files: package.json, package-lock.json (root entry), src-tauri/Cargo.toml,
// src-tauri/Cargo.lock (this crate's entry) and src-tauri/tauri.conf.json.
// Edits are targeted text replacements, so formatting is preserved. Node
// built-ins only.

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const CRATE = "aichemist-desktop-buddy";
// Plain MAJOR.MINOR.PATCH. The Windows MSI bundler rejects most pre-release
// suffixes, so they are not allowed here either.
const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

/** Each file: how to read its version and how to replace it in the text. */
const FILES = [
  {
    path: "package.json",
    read: (text) => [JSON.parse(text).version],
    write: (text, v) => replaceFirst(text, /^(  "version": ")[^"]*(")/m, v, 1),
  },
  {
    path: "package-lock.json",
    read: (text) => {
      const lock = JSON.parse(text);
      return [lock.version, lock.packages?.[""]?.version];
    },
    // The top-level "version" and packages[""].version are the first two.
    write: (text, v) => replaceFirst(text, /^(\s*"version": ")[^"]*(")/gm, v, 2),
  },
  {
    path: "src-tauri/Cargo.toml",
    read: (text) => [packageSection(text).match(/^version = "([^"]*)"/m)?.[1]],
    write: (text, v) => {
      const section = packageSection(text);
      const updated = replaceFirst(section, /^(version = ")[^"]*(")/m, v, 1);
      return text.replace(section, updated);
    },
  },
  {
    path: "src-tauri/Cargo.lock",
    read: (text) => [text.match(lockEntry())?.[2]],
    write: (text, v) => {
      if (!lockEntry().test(text)) throw new Error(`no [[package]] entry for ${CRATE}`);
      return text.replace(lockEntry(), (_, head, _old, tail) => `${head}${v}${tail}`);
    },
  },
  {
    path: "src-tauri/tauri.conf.json",
    read: (text) => [JSON.parse(text).version],
    write: (text, v) => replaceFirst(text, /^(  "version": ")[^"]*(")/m, v, 1),
  },
];

function lockEntry() {
  return new RegExp(`(\\[\\[package\\]\\]\\r?\\nname = "${CRATE}"\\r?\\nversion = ")([^"]*)(")`);
}

/** The text of Cargo.toml's [package] table, up to the next table header. */
function packageSection(text) {
  const match = text.match(/^\[package\][\s\S]*?(?=^\[)/m);
  if (!match) throw new Error("src-tauri/Cargo.toml has no [package] table");
  return match[0];
}

/** Replaces the version in the first `count` matches of `re` (groups: prefix, suffix). */
function replaceFirst(text, re, version, count) {
  let n = 0;
  const out = text.replace(re, (whole, head, tail) => (n++ < count ? `${head}${version}${tail}` : whole));
  if (n < count) throw new Error(`expected ${count} version field(s), found ${n}`);
  return out;
}

function readVersions() {
  return FILES.map((f) => {
    const text = readFileSync(join(root, f.path), "utf8");
    return { path: f.path, versions: f.read(text) };
  });
}

function check(tag) {
  const found = readVersions();
  const all = new Set(found.flatMap((f) => f.versions));
  for (const f of found) console.log(`${f.path}: ${f.versions.join(", ")}`);
  if (all.size !== 1 || all.has(undefined)) {
    console.error("error: the version fields disagree. Run: node tools/bump-version.mjs X.Y.Z");
    process.exit(1);
  }
  const [version] = all;
  if (!SEMVER.test(version)) {
    console.error(`error: version ${version} is not plain MAJOR.MINOR.PATCH`);
    process.exit(1);
  }
  if (tag !== undefined && tag !== `v${version}`) {
    console.error(`error: tag ${tag} does not match the app version ${version} (expected v${version})`);
    process.exit(1);
  }
  console.log(`ok: version ${version}${tag ? `, tag ${tag}` : ""}`);
}

function bump(version) {
  if (!SEMVER.test(version)) {
    console.error(`error: ${version} is not plain MAJOR.MINOR.PATCH (e.g. 0.2.0)`);
    process.exit(1);
  }
  // Compute every edit before writing any file, so a failure changes nothing.
  const edits = FILES.map((f) => {
    const full = join(root, f.path);
    const text = readFileSync(full, "utf8");
    const next = f.write(text, version);
    if (f.read(next).some((v) => v !== version)) throw new Error(`${f.path}: edit did not take`);
    return { full, path: f.path, next, changed: next !== text };
  });
  for (const e of edits) {
    if (e.changed) writeFileSync(e.full, e.next);
    console.log(`${e.changed ? "updated" : "unchanged"} ${e.path}`);
  }
  console.log(`\nNext: commit, merge to main, then tag v${version} on that commit (docs/releasing.md).`);
}

const args = process.argv.slice(2);
if (args[0] === "--check" && args.length <= 2) {
  check(args[1]);
} else if (args.length === 1 && !args[0].startsWith("-")) {
  bump(args[0].replace(/^v/, ""));
} else {
  console.error("usage: node tools/bump-version.mjs X.Y.Z | --check [vX.Y.Z]");
  process.exit(2);
}
