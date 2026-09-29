#!/usr/bin/env node
/**
 * Erzeugt src-tauri/web/legal.js aus der gemeinsamen Quelle src/legal.ts
 * (die Desktop-App importiert dieselbe Datei).
 *
 *   node scripts/gen-legal.mjs           # schreibt legal.js
 *   node scripts/gen-legal.mjs --check   # nur prüfen (für CI), exit 1 bei Drift
 *
 * Hintergrund: Das eingebettete Web-Frontend kann kein TypeScript, lädt seine
 * Skripte aber als plain JS aus src-tauri/web/. Ohne Generator gäbe es die
 * Rechtstexte doppelt - einmal in src/legal.ts, einmal hier.
 */
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const srcPath = join(root, "src", "legal.ts");
const outPath = join(root, "src-tauri", "web", "legal.js");

const src = readFileSync(srcPath, "utf8");
const start = src.indexOf("export const APP_LEGAL");
if (start < 0) {
  console.error("gen-legal: src/legal.ts has no `export const APP_LEGAL`");
  process.exit(1);
}

// The interface declarations and the typed const are TypeScript-only; strip
// them and keep the object literal, which is valid JS once the keys are quoted.
let doc = src.slice(start);
doc = doc.replace(/^export const APP_LEGAL: LegalDoc =/, "const APP_LEGAL =");
const body = doc.replace(/;?\s*$/, "").trim();
if (!body.startsWith("const APP_LEGAL =")) {
  console.error("gen-legal: could not read the APP_LEGAL object");
  process.exit(1);
}

// Evaluate the literal in a throwaway scope instead of hand-parsing it: the
// source stays the single place where the wording is edited.
let parsed;
try {
  const factory = new Function(
    "LegalDoc", "LegalSection",
    body + "\n return APP_LEGAL;"
  );
  parsed = factory({ id: "", title: "", body: "" });
} catch (e) {
  console.error("gen-legal: could not evaluate APP_LEGAL: " + e.message);
  process.exit(1);
}
if (!parsed || typeof parsed !== "object" || !Array.isArray(parsed.sections)) {
  console.error("gen-legal: APP_LEGAL has no sections array");
  process.exit(1);
}
const ids = new Set();
for (const s of parsed.sections) {
  if (!s || typeof s.id !== "string" || !s.id.trim() || typeof s.title !== "string" || typeof s.body !== "string" || !s.body.trim()) {
    console.error("gen-legal: every section needs a non-empty id, title and body");
    process.exit(1);
  }
  if (ids.has(s.id)) { console.error("gen-legal: duplicate section id " + s.id); process.exit(1); }
  ids.add(s.id);
}
if (!ids.size) { console.error("gen-legal: no sections"); process.exit(1); }

const out =
  "/* GENERATED FILE - do not edit.\n" +
  "   Source: src/legal.ts  (Regenerate: npm run legal)\n" +
  "   The desktop app imports that TypeScript module directly; this plain-JS\n" +
  "   copy exists because the embedded web frontend cannot import TypeScript. */\n" +
  "window.AppLegal = " + JSON.stringify(parsed, null, 2) + ";\n";

if (process.argv.includes("--check")) {
  let current = "";
  try { current = readFileSync(outPath, "utf8"); } catch { /* missing = drift */ }
  if (current !== out) {
    console.error("legal.js is out of date. Run: npm run legal");
    process.exit(1);
  }
  console.log("legal.js is current");
} else {
  writeFileSync(outPath, out);
  console.log(`wrote ${outPath} (${parsed.sections.length} sections)`);
}
