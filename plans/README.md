# Optional features, proxy models, composition, file storage, and generic relations

Status: implementation plans, not implemented behavior.

These plans record the decisions from the model inheritance and file storage
discussions. ORM features
are declared in the schema file and selected during the build. Python and
TypeScript expose equivalent behavior; each extension declares which databases
and languages it supports. Features can ship in separate tickets.

## Implementation order

1. [Build-time extension system](01-build-time-extensions.md).
2. [Optional dependencies and distribution](02-optional-packaging.md).
3. [Selection and schema-defined query defaults](03-selection-and-defaults.md).
4. [Explicit async reference loading](04-async-reference-loading.md).
5. [Same-table proxy models](05-proxy-models.md).
6. [Composition with shared primary keys](06-model-composition.md).
7. [Independent storage and ORM file fields](07-file-storage.md).
8. [Generic foreign keys with a schema-generated ContentType enum](08-generic-foreign-keys.md).

Plans 3 and 4 can proceed independently once extension contracts are available.
Plan 5 depends on plan 3. Plan 6 reuses relations, transactions, and plan 4; partial
selection is needed when exposing flattened parent fields on child models.
Packaging can be delivered incrementally alongside the extension system.
The independent storage project in plan 7 can proceed separately. Its optional ORM
extension depends on plans 1 and 2; managed lifecycle/cleanup is later opt-in work.
Plan 8 depends on plans 1 and 2 and reuses plan 4's explicit loading contract.
Its shared ContentType generation collects globally unique model names across the
complete schema set. A committed identity manifest freezes integer storage IDs
across file moves and model/table renames; retired IDs are never reused.

## Agreed behavior

- Schema files define proxy models, composition, field overrides, defaults, and
  default query behavior. Exact new schema syntax is still a proposal.
- Proxy chains share the source table. Fields can be excluded from default
  selection, narrowed to non-null or an enum subset, and given different defaults.
- Incorrect narrowing logs a warning and preserves the actual value. It does not
  add implicit SQL filters, remove result rows, or raise a shape-contract error.
- A declared default filter can reference `parent.default_filter`, or replace it
  with its own expression. Clearing and absent declarations are specified in plan 3.
- A person may have both an employee and a customer record.
- Parent queries return parent instances. Named child references use ordinary
  `select_related` / `selectRelated`; no built-in `child`, `load_child`, or
  `cast_down` API is required.
- Loaded, missing optional references return `None` / `null`. Unloaded references
  retain `NotLoaded`; explicit async loading is available through generated loaders.
- Child deletion preserves its parent and siblings. Parent deletion cascades to
  children. Reverse deletion ownership is a separate `owningFk` feature.
- Disabled extensions have no query, write, or decoding overhead. Enabled
  extensions use the same compiled execution path as an equivalent core feature.

## Separate future work

Custom validators, `owningFk`, polymorphic downcasting, multiple composed parents,
and writable views / trigger-driven parent insertion are outside these tickets.
Trigger-driven inserts may be reconsidered after engine-managed atomic writes
work consistently across PostgreSQL and SQLite.

## Review points

The following are implementation proposals that need API review before coding:
generated `load_<relation>()` / `loadRelation()` method names, partial-model
selection syntax, namespaced extension attributes, and native distribution package
names. No new source syntax or package names in these plans are shipped APIs.
