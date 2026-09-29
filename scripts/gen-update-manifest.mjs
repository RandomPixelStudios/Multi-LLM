#!/usr/bin/env node
/**
 * Schreibt das Update-Manifest der Website (update.json) aus der
 * package.json-Version, damit Release und Manifest nicht auseinanderlaufen.
 *
 *   node scripts/gen-update-manifest.mjs [--out <website-dir>]
 *   node scripts/gen-update-manifest.mjs --check
 *
 * Hintergrund: Die App fragt beim Start eine JSON-Datei ab
 * (src-tauri/src/update_check.rs). Ohne diesen Schritt muesste die Version
 * nach jedem Release von Hand in die Website nachgezogen werden - und genau
 * an dieser Stelle vergessen Updates zu erscheinen.
 *
 * Die Website liegt absichtlich NICHT im Repo (sie wird als eigener Branch
 * gepusht). Deshalb wird das Ziel ueber --out oder MLM_WEBSITE_DIR bestimmt
 * und faellt auf ~/Schreibtisch/Website zurueck.
 */
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);

function outDir() {
  const flag = args.indexOf("--out");
  if (flag >= 0 && args[flag + 1]) { return resolve(args[flag + 1]); }
  if (process.env.MLM_WEBSITE_DIR) { return resolve(process.env.MLM_WEBSITE_DIR); }
  return join(homedir(), "Schreibtisch", "Website");
}

const dir = outDir();
const target = join(dir, "update.json");

const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
const version = pkg.version;
if (typeof version !== "string" || !/^\d+\.\d+\.\d+/.test(version)) {
  console.error(`gen-update-manifest: package.json version is not x.y.z: ${version}`);
  process.exit(1);
}

const out = JSON.stringify(
  {
    version,
    download_url: "https://github.com/RandomPixelStudios/Multi-LLM/releases",
    changelog:
      `Multi LLM ${version}. Desktop-App fuer Windows und Linux, ` +
      "Docker-Image fuer Teams. Installer und Pakete: GitHub Releases.",
  },
  null,
  2
) + "\n";

if (args.includes("--check")) {
  if (!existsSync(target)) {
    console.error(`update.json not found at ${target}. Run: npm run update-manifest`);
    process.exit(1);
  }
  if (readFileSync(target, "utf8") !== out) {
    console.error("update.json is out of date. Run: npm run update-manifest");
    process.exit(1);
  }
  console.log("update.json is current");
} else {
  writeFileSync(target, out);
  console.log(`wrote ${target} (version ${version})`);
}
