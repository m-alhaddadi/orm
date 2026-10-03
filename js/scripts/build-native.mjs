// Builds the Rust addon (bindings/node) and copies it next to package.json as orm.node.
//   node scripts/build-native.mjs [--release]
import { execFileSync } from "node:child_process";
import { copyFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..");
const release = process.argv.includes("--release");
execFileSync("cargo", ["build", "-p", "orm-node", ...(release ? ["--release"] : [])], { cwd: root, stdio: "inherit" });
const lib = { darwin: "liborm_node.dylib", win32: "orm_node.dll" }[process.platform] ?? "liborm_node.so";
copyFileSync(join(root, "target", release ? "release" : "debug", lib), join(here, "..", "orm.node"));
console.log("wrote orm.node");
