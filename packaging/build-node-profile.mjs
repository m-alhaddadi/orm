// Build one profile's platform artifact; do not publish.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const profile = process.argv[2];
if (!["postgres", "sqlite", "combined", "tooling"].includes(profile)) throw new Error("expected a named profile");
const release = process.argv.includes("--release");
execFileSync("cargo", ["build", "-p", "orm-node", "--no-default-features", "--features", `profile-${profile}`, ...(release ? ["--release"] : [])], { cwd: root, stdio: "inherit" });
const platform = `${process.platform}-${process.arch}`;
if (!["darwin-arm64", "linux-x64", "win32-x64"].includes(platform)) throw new Error(`unsupported prebuilt platform ${platform}`);
const output = join(root, "target", "node-profiles", `${profile}-${platform}`);
mkdirSync(output, { recursive: true });
const filename = { darwin: "liborm_node.dylib", win32: "orm_node.dll" }[process.platform] ?? "liborm_node.so";
copyFileSync(join(process.env.CARGO_TARGET_DIR ?? join(root, "target"), release ? "release" : "debug", filename), join(output, "orm.node"));
writeFileSync(join(output, "index.cjs"), "module.exports = require('./orm.node');\n");
writeFileSync(join(output, "package.json"), JSON.stringify({
  name: `@orm/native-${profile}-${platform}`, version: "0.1.0", main: "index.cjs",
  files: ["orm.node", "index.cjs"], os: [process.platform], cpu: [process.arch], engines: { node: ">=20" },
}, null, 2) + "\n");
console.log(output);
