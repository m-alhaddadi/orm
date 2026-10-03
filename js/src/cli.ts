#!/usr/bin/env node
/**
 * `orm`: schema and migration commands (`npx orm ...`), like `python -m orm`.
 *
 * ```
 * orm check
 * orm generate [-o models.ts] [--import orm]     models.ts from the schema
 * orm makemigrations [name] [--empty] [--check]
 * orm sqlmigrate <migration> [--down]
 * orm migrate [target]
 * orm rollback [--steps N | --to <migration>|zero]
 * orm showmigrations
 * ```
 *
 * The schema file and migrations directory come from `--schema` / `--dir`, or from the
 * `"orm"` key of `package.json` (`{"schema": "schema.prisma", "migrations":
 * "migrations"}`). The database URL comes from `--url` or `ORM_DATABASE_URL`.
 */

import { existsSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";

import { connect } from "./db.js";
import { SchemaError } from "./errors.js";
import { Migrations, Migrator, MigrationError } from "./migrations.js";
import { Registry, define } from "./model.js";
import { call, native } from "./native.js";

const USAGE = `usage: orm [--schema FILE] [--dir DIR] [--url URL] <command>
  check                                   compile the schema and report errors
  generate [-o models.ts] [--import orm]  write models.ts from the schema
  makemigrations [name] [--empty] [--check]
  sqlmigrate <migration> [--down]
  migrate [target]
  rollback [--steps N | --to <migration>|zero]
  showmigrations`;

interface Args {
  readonly positional: string[];
  readonly options: Map<string, string | true>;
}

const FLAGS = new Set(["--empty", "--check", "--down", "-h", "--help"]);

function parse(argv: readonly string[]): Args {
  const positional: string[] = [];
  const options = new Map<string, string | true>();
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]!;
    if (FLAGS.has(a)) {
      options.set(a, true);
    } else if (a.startsWith("-")) {
      const v = argv[++i];
      if (v === undefined) {
        throw new UsageError(`${a} needs a value`);
      }
      options.set(a, v);
    } else {
      positional.push(a);
    }
  }
  return { positional, options };
}

class UsageError extends Error {}

function config(): Record<string, string> {
  if (!existsSync("package.json")) {
    return {};
  }
  const pkg = JSON.parse(readFileSync("package.json", "utf8")) as { orm?: Record<string, string> };
  return pkg.orm ?? {};
}

/** Runs the CLI; gives the exit code. `out` / `err` collect output (tests). */
export async function main(
  argv: readonly string[],
  out: (line: string) => void = (l) => process.stdout.write(l + "\n"),
  err: (line: string) => void = (l) => process.stderr.write(l + "\n"),
): Promise<number> {
  let args: Args;
  try {
    args = parse(argv);
  } catch (e) {
    err(`error: ${(e as Error).message}\n${USAGE}`);
    return 2;
  }
  const [command, ...rest] = args.positional;
  if (!command || args.options.has("-h") || args.options.has("--help")) {
    (command ? out : err)(USAGE);
    return command ? 0 : 2;
  }
  const opt = (name: string) => {
    const v = args.options.get(name);
    return typeof v === "string" ? v : undefined;
  };
  const cfg = config();
  const schema = opt("--schema") ?? cfg["schema"] ?? "schema.prisma";
  const migrations = new Migrations(opt("--dir") ?? cfg["migrations"] ?? "migrations", schema);
  try {
    switch (command) {
      case "check":
        call(() => native().compileSchemaFile(schema));
        out(`${schema}: ok`);
        return 0;
      case "generate": {
        const target = opt("-o") ?? opt("--out") ?? join(dirname(schema), "models.ts");
        const source = call(() => native().generateTypescript(schema, opt("--import") ?? "orm"));
        mkdirSync(dirname(target), { recursive: true });
        writeFileSync(target, source);
        out(`wrote ${target}`);
        return 0;
      }
      case "makemigrations": {
        const plan = migrations.plan();
        if (args.options.has("--check")) {
          for (const s of plan.up) {
            out(`  ${s.summary}`);
          }
          if (plan.up.length) {
            err("the schema has changes without a migration");
          }
          return plan.up.length ? 1 : 0;
        }
        const m = migrations.make(rest[0], { empty: args.options.has("--empty") });
        if (m === null) {
          out("No changes.");
          return 0;
        }
        out(`Created ${m.path}`);
        for (const s of plan.up) {
          out(`  - ${s.summary}`);
          if (s.warning) {
            out(`    ! ${s.warning}`);
          }
        }
        return 0;
      }
      case "sqlmigrate": {
        if (!rest[0]) {
          throw new UsageError("sqlmigrate needs a migration");
        }
        const m = migrations.get(rest[0]);
        out((args.options.has("--down") ? m.downSql : m.upSql).replace(/\n$/, ""));
        return 0;
      }
      case "migrate":
      case "rollback":
      case "showmigrations":
        return await database(command, rest, migrations, schema, opt, out, err);
      default:
        throw new UsageError(`unknown command ${command}`);
    }
  } catch (e) {
    if (e instanceof UsageError) {
      err(`error: ${e.message}\n${USAGE}`);
      return 2;
    }
    if (e instanceof MigrationError || e instanceof SchemaError) {
      err(`error: ${e.message}`);
      return 1;
    }
    throw e;
  }
}

async function database(
  command: string,
  rest: string[],
  migrations: Migrations,
  schema: string,
  opt: (name: string) => string | undefined,
  out: (line: string) => void,
  err: (line: string) => void,
): Promise<number> {
  const url = opt("--url") ?? process.env["ORM_DATABASE_URL"];
  if (!url) {
    err("error: no database; pass --url or set ORM_DATABASE_URL");
    return 2;
  }
  const registry = new Registry();
  define(call(() => native().compileSchemaFile(schema)), { registry });
  const db = await connect(url, { maxConnections: 1, default: false, registry });
  try {
    const migrator = new Migrator(db, migrations);
    if (command === "migrate") {
      const done = await migrator.upgrade(rest[0]);
      for (const m of done) {
        out(`Applied ${m.name}`);
      }
      if (!done.length) {
        out("Nothing to apply.");
      }
    } else if (command === "rollback") {
      const to = opt("--to");
      const steps = opt("--steps");
      const done = await migrator.downgrade(to !== undefined ? { target: to } : { steps: steps === undefined ? 1 : Number(steps) });
      for (const m of done) {
        out(`Reverted ${m.name}`);
      }
      if (!done.length) {
        out("Nothing to revert.");
      }
    } else {
      for (const s of await migrator.status()) {
        out(`[${s.applied ? "x" : " "}] ${s.migration.name}${s.appliedAt ? `  (${s.appliedAt})` : ""}`);
      }
    }
  } finally {
    await db.close();
  }
  return 0;
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
