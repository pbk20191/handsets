#!/usr/bin/env node
// Stages one platform's MCP bundle tree for `mcpb pack`:
//
//   <out>/manifest.json   mcpb/manifest.json with version and platform set
//   <out>/server/hs[.exe] the CLI, run as `hs mcp`
//   <out>/server/hs.jar   the on-device daemon (hs finds it next to itself)
//   <out>/LICENSE, README.md
//
// usage: node scripts/mcpb-stage.mjs --version v1.2.3 --platform darwin|win32|linux \
//          --hs path/to/hs[.exe] --jar build/hs.jar --out dist/mcpb
import { copyFileSync, mkdirSync, readFileSync, rmSync, writeFileSync, chmodSync, existsSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const args = {};
for (let i = 2; i < process.argv.length; i += 2) {
  args[process.argv[i].replace(/^--/, "")] = process.argv[i + 1];
}
for (const k of ["version", "platform", "hs", "jar", "out"]) {
  if (!args[k]) {
    console.error(`missing --${k}`);
    process.exit(2);
  }
}
if (!["darwin", "win32", "linux"].includes(args.platform)) {
  console.error(`--platform must be darwin, win32 or linux, not ${args.platform}`);
  process.exit(2);
}

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const manifest = JSON.parse(readFileSync(join(root, "mcpb", "manifest.json"), "utf8"));
// Tags are v-prefixed; the manifest wants plain semver.
manifest.version = args.version.replace(/^v/, "");
manifest.compatibility = { ...manifest.compatibility, platforms: [args.platform] };

rmSync(args.out, { recursive: true, force: true });
mkdirSync(join(args.out, "server"), { recursive: true });
writeFileSync(join(args.out, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");

const hs = join(args.out, "server", basename(args.hs));
copyFileSync(args.hs, hs);
if (args.platform !== "win32") chmodSync(hs, 0o755);
copyFileSync(args.jar, join(args.out, "server", "hs.jar"));
for (const f of ["LICENSE", "README.md"]) {
  if (existsSync(join(root, f))) copyFileSync(join(root, f), join(args.out, f));
}
console.log(`staged ${manifest.name} ${manifest.version} (${args.platform}) in ${args.out}`);
