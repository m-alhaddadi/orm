#!/usr/bin/env node
/**
 * `npx orm`: the `orm` command line, the same Rust code as the standalone `orm` binary
 * and `python -m orm` (`cli/`). See `npx orm --help`.
 *
 * The schema file and migrations directory come from `--schema` / `--dir`, or from the
 * `"orm"` key of `package.json` (`{"schema": "schema.prisma", "migrations":
 * "migrations"}`). The database URL comes from `--url` or `ORM_DATABASE_URL`.
 */

import { realpathSync } from "node:fs";
import { pathToFileURL } from "node:url";

import "./index.js"; // loads the modules in their order
import { hasDataSteps, migrateCommand } from "./migrations.js";
import { native } from "./native.js";

/** Runs the command line; gives the exit code. Output goes to stdout / stderr. */
export function main(argv: readonly string[]): Promise<number> {
  const addon = native();
  if (typeof addon.cli !== "function") throw new Error("CLI unavailable; install @orm/native-tooling and set ORM_PROFILE=tooling");
  const found = addon.cliMigrateArgs([...argv]);
  if (found !== null) {
    const [schema, dir, url, target] = found;
    // data.ts runs in TypeScript, so this migrator applies the directory; without a URL the CLI reports the usage error
    if (url && hasDataSteps(dir!)) {
      return migrateCommand(schema!, dir!, url, target ?? null).catch((e: unknown) => {
        process.stderr.write(`error: ${e instanceof Error ? e.message : String(e)}\n`);
        return 1;
      });
    }
  }
  return addon.cli([...argv]);
}

if (process.argv[1] && import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href) {
  main(process.argv.slice(2)).then(
    (code) => process.exit(code),
    (e: unknown) => {
      process.stderr.write(`error: ${e instanceof Error ? e.message : String(e)}\n`);
      process.exit(1);
    },
  );
}
