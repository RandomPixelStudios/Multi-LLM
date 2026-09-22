#!/usr/bin/env node
/**
 * Erzeugt src-tauri/web/presets.js aus der gemeinsamen Quelle
 * src/provider-presets.json (Desktop-Frontend importiert dieselbe Datei).
 *
 *   node scripts/gen-presets.mjs           # schreibt presets.js
 *   node scripts/gen-presets.mjs --check   # nur prüfen (für CI), exit 1 bei Drift
 *
 * Hintergrund: Die Provider-Liste existierte vorher doppelt (Literal in
 * src/main.ts + hand- bzw. skriptgenerierte Kopie in src-tauri/web). Beide
 * Frontends sollen sich nur noch an einer Quelle orientieren.
 */
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const srcPath = join(root, "src", "provider-presets.json");
const outPath = join(root, "src-tauri", "web", "presets.js");

const presets = JSON.parse(readFileSync(srcPath, "utf8"));
if (!Array.isArray(presets) || presets.length === 0) {
  console.error("gen-presets: src/provider-presets.json is not a non-empty array");
  process.exit(1);
}

const seen = new Set();
const normalized = presets.map((p, i) => {
  if (!p || typeof p.name !== "string" || !p.name.trim() || typeof p.url !== "string" || !p.url.trim()) {
    console.error(`gen-presets: entry #${i} needs a non-empty "name" and "url"`);
    process.exit(1);
  }
  if (p.format !== undefined && p.format !== "openai" && p.format !== "anthropic") {
    console.error(`gen-presets: entry "${p.name}" has unsupported format "${p.format}"`);
    process.exit(1);
  }
  if (seen.has(p.name)) {
    console.error(`gen-presets: duplicate preset name "${p.name}"`);
    process.exit(1);
  }
  seen.add(p.name);
  // Desktop default: alles außer "anthropic" läuft als OpenAI-kompat.
  return { name: p.name, url: p.url, format: p.format === "anthropic" ? "anthropic" : "openai" };
});

const header =
  "/* Auto-generiert aus src/provider-presets.json - nicht von Hand editieren.\n" +
  "   Neu erzeugen: npm run presets   |   Prüfen (CI): npm run presets:check */\n";
const body =
  "window.PROVIDER_PRESETS = [\n" +
  normalized.map((p) => "  " + JSON.stringify(p)).join(",\n") +
  "\n];\n";
const output = header + body;

if (process.argv.includes("--check")) {
  let current = "";
  try {
    current = readFileSync(outPath, "utf8");
  } catch {
    /* missing file counts as drift */
  }
  if (current !== output) {
    console.error(
      "gen-presets: src-tauri/web/presets.js is stale - run `npm run presets`",
    );
    process.exit(1);
  }
  console.log("presets ok");
} else {
  writeFileSync(outPath, output);
  console.log(`presets: ${normalized.length} entries -> src-tauri/web/presets.js`);
}
