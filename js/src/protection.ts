/** `@@protected_write`: ORM writes to a protected model run only inside {@link allowWrites}. */

import { AsyncLocalStorage } from "node:async_hooks";

import type { ModelMeta } from "./model.js";

/** The model names the current scope allows to write; scopes nest by union. */
const scope = new AsyncLocalStorage<readonly string[]>();
const none: readonly string[] = [];

/**
 * Runs `fn` with ORM writes allowed to `models`, which `@@protected_write` otherwise
 * rejects with `WriteProtected`. An application-level check: raw SQL (`db.execute`),
 * migrations and other database clients still write. Work that `fn` starts gets the
 * scope too. It starts no transaction. Gives what `fn` gives.
 */
export function allowWrites<T>(models: readonly { readonly _meta: ModelMeta }[], fn: () => Promise<T>): Promise<T> {
  const names = models.map((m: unknown) => {
    const meta = typeof m === "object" && m !== null ? (m as { _meta?: ModelMeta })._meta : undefined;
    if (!meta) throw new TypeError("allowWrites() takes models");
    return meta.name;
  });
  return scope.run([...allowedWrites(), ...names], fn);
}

/** @internal The model names the current scope allows to write. */
export function allowedWrites(): readonly string[] {
  return scope.getStore() ?? none;
}
