import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const profile = process.argv[2];
if (!["postgres", "sqlite", "combined", "tooling"].includes(profile)) throw new Error("expected named profile");
const packs = join(root, "target", "npm-packs"), install = join(root, "target", "node-installs", profile);
mkdirSync(packs, { recursive: true }); mkdirSync(install, { recursive: true });
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const env = { ...process.env, npm_config_cache: join(root, "target", "npm-cache") };
const artifacts = [join(root, "js", "node_modules", "decimal.js"), join(root, "js"), join(root, "packaging", "node", profile), join(root, "target", "node-profiles", `${profile}-${process.platform}-${process.arch}`)].map(cwd => {
  const packed = JSON.parse(execFileSync(npm, ["pack", "--ignore-scripts", "--json", "--pack-destination", packs], { cwd, env, encoding: "utf8" }))[0];
  if (cwd === join(root, "js") && packed.files.some(file => file.path.endsWith(".node"))) throw new Error("thin package contains native addon");
  return join(packs, packed.filename);
});
writeFileSync(join(install, "package.json"), JSON.stringify({ private: true, type: "module" }));
execFileSync(npm, ["install", "--offline", "--ignore-scripts", "--no-audit", "--no-fund", ...artifacts], { cwd: install, env, stdio: "inherit" });
const lock = JSON.parse(readFileSync(join(install, "package-lock.json"), "utf8"));
for (const name of Object.keys(lock.packages)) {
  if (name.includes("node_modules/@orm/native-") && !name.startsWith(`node_modules/@orm/native-${profile}`)) throw new Error(`unselected native dependency ${name}`);
}
copyFileSync(join(root, "packaging", "smoke-node.mjs"), join(install, "smoke.mjs"));
execFileSync(process.execPath, [join(install, "smoke.mjs")], { cwd: install, env: { ...env, ORM_PROFILE: profile }, stdio: "inherit" });
