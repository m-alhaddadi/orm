// Builds the Rust addon (bindings/node) and copies it next to package.json as orm.node.
//   node scripts/build-native.mjs [--release]
import { execFileSync } from "node:child_process";
import { copyFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..");
const profileIndex = process.argv.indexOf("--profile");
const profile = profileIndex < 0 ? undefined : process.argv[profileIndex + 1];
const featuresIndex = process.argv.indexOf("--features");
const features = featuresIndex < 0 ? undefined : process.argv[featuresIndex + 1];
if ((profileIndex >= 0 && !["postgres", "sqlite", "combined", "tooling"].includes(profile)) ||
    (featuresIndex >= 0 && !features) || (profile && features)) throw new Error("choose --profile postgres|sqlite|combined|tooling or --features <exact Cargo features>");
const release = process.argv.includes("--release");
execFileSync("cargo", ["build", "-p", "orm-node", ...(release ? ["--release"] : []), ...(profile || features ? ["--no-default-features", "--features", profile ? `profile-${profile}` : features] : [])], { cwd: root, stdio: "inherit" });
const lib = { darwin: "liborm_node.dylib", win32: "orm_node.dll" }[process.platform] ?? "liborm_node.so";
copyFileSync(join(process.env.CARGO_TARGET_DIR ?? join(root, "target"), release ? "release" : "debug", lib), join(here, "..", "orm.node"));
console.log("wrote orm.node");
