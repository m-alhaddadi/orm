# Build-time behavioral extensions

Behavioral extensions are selected before building the Python or Node native
library. They are ordinary Rust dependencies; installing a crate does not modify
an existing native library. Database type/function/index catalogs remain separate:
`import "catalog.toml"` supplies database definitions, not executable Rust.

## Contracts and preparation

`orm-contracts` contains schema IR, dialects and versioned extension contracts.
It depends on neither bindings nor extension implementations. Compiler machinery
lives in `orm-extension-build`. Native profile types live in `orm_contracts::native`. Host contract 1 supports schema transformations,
physical schema contributions, scalar string validation/transformation, borrowed
string record validation, native string results, and static model methods.

Python `define(schema, registry=...)` and TypeScript `define(schema, registry)`
prepare a complete candidate before publishing models. Invalid declarations,
missing exports, unsupported language/database combinations, stale native
specializations and member collisions fail during definition. Failed definitions
preserve the previous registry. Existing models retain their prepared snapshot.
Returned IR is owned by its caller; mutating it does not change prepared models.

Python class declarations use `Registry(dialect="sqlite")` when needed, followed
by `registry.prepare()`. Queries on an unprepared class registry fail. Connecting
also prepares registered classes during connection setup. Class preparation can
normalize field and relation descriptors; model renames/additions and removal of
inherited descriptors require a fresh `define` batch. Callable Python client
field defaults run before native transformations and validation; they are not
SQL defaults and do not affect other database writers.

## Build configuration

Run `cargo run -p orm-extension-build -- path/to/build.json`. Paths are relative
to the configuration file. `output` must be a new directory. For example:

```json
{
  "host": "../orm",
  "output": "../compiled-orm",
  "dependencies": {
    "rules": {"package": "my-rules", "path": "../my-rules"}
  },
  "bindings": ["python", "node"],
  "specialization": {
    "schema": "schema.json",
    "models": [{
      "model": "User",
      "fields": [{"field": "username", "transforms": ["app.trim"], "validators": ["app.username"]}],
      "records": [{"dependencies": ["username"], "export": "app.user"}],
      "computed": [{"field": "display", "dependency": "username", "export": "app.display"}],
      "methods": [{"name": "validate_username", "export": "app.username"}]
    }]
  }
}
```

`offline: true` uses installed Cargo dependencies. `prepare_only: true` emits the
workspace and composition without building the bindings. `inputs` lists extra
configuration/resource files that must participate in rebuild detection. Declare
all selected behavioral extension crates in `dependencies`, including extensions
required by another manifest. Cargo resolves Rust package versions; the frontend
checks extension dependency constraints against that resolved selection.

Instead of a dependency, `modules` accepts an application source module:
`{"alias":"rules","source":"rules.rs","manifest":{...},"dependencies":{...}}`.
The frontend wraps the source in a Cargo crate and supplies `orm-contracts`.
Its metadata and export contracts are identical to those of a packaged crate.

The frontend copies host sources into the output workspace, resolves dependencies
and reads Cargo metadata before generating direct calls. It emits `artifact.json`,
`manifests.json`, `source-identities.json`, `normalized.schema.json` for selected
profiles, and fixed Rust composition sources. It builds ordinary `orm-python`
and `orm-node` release libraries. Package the resulting library with the matching
Python/TypeScript package, replacing its native library. Do not mix libraries from
different profiles. Existing package build/install tools handle the platform
library suffix and Python extension suffix.

Rebuilding an emitted workspace requires its composition environment paths:
`ORM_CORE_COMPOSITION`, and for native behaviors `ORM_ENGINE_COMPOSITION`,
`ORM_PYTHON_METHODS`, `ORM_NODE_METHODS`. A generated build guard rejects changed
recorded configuration/source inputs and asks for a fresh frontend output.
Use the frontend for a changed selection rather than editing generated files.

## Extension metadata and source syntax

A crate declares `[package.metadata.orm-extension]`, either inline or as
`manifest = "orm-extension.toml"` referencing a resource inside its package.
The version must match its Cargo package version. Required fields include `id`,
`version`, `host_contract`, and `schema_contract`. Optional lists describe
`languages` (`python`, `typescript`), `databases` (`postgres`, `sqlite`), host
`capabilities`, semver extension `dependencies`, `attributes`, `passes`, and
`exports`. Unknown metadata fields are errors.

An attribute declares its `name`, `target` (`model` or `field`), and typed `arguments`
(`string`, `integer`, `boolean`, `list`). Source declarations use namespaced
attributes, for example `name String @app.rename(to: "public_name")` or
`@@app.proxy(parent: "User")`. Names, arguments and source locations are preserved
in schema requirements. Unknown namespaces request a rebuild; `@db.*` keeps its
existing database meaning.

A pass declares `id`, `phase`, `rust`, `after`, and owned `effects` paths. Phases
run declaration, logical, storage, behavior, validation, generation. Ordering is
stable; cycles, missing pass dependencies, duplicate IDs and overlapping effect
ownership fail. Its Rust function takes `&mut orm_contracts::ir::SchemaIr` and
returns `Result<(), String>`. Rust paths can use `crate::function`, the owning
crate name, or its selected dependency alias; composition rewrites the prefix.
Namespaced model declarations may derive their primary key during selected passes.
Ordinary models require `@id` before lowering; annotated models must have one
after preparation. Feature-derived relations should be contributed by the pass.

Completed passes and prepared model names (`lowered_models`) are recorded so a generated or serialized normalized schema is
not lowered twice. Additional definition batches lower their new declarations
against the complete existing definition context. Attribute-independent passes
use `lowered_models` to distinguish previously prepared models from a new batch.

## Logical fields and physical storage

The compiler captures a physical schema before lowering namespaced declarations.
Logical renames/removals leave that storage snapshot intact. Passes intending a
physical change update `behavior.storage.models` explicitly. Migration planning
and snapshots use the physical schema, so a public field removal does not drop
its column. Logical fields must resolve to an existing physical column with a
compatible encoding and primary key. Prepared `ResolvedField` entries retain
logical identity, storage owner, column identity and both value types.

A same-table proxy can contribute another logical model sharing its parent's
physical owner without creating a second migration table. Shared-primary-key
storage composition contributes `behavior.field_storage` entries (`model`,
`field`, `owner`, `column`) and `behavior.owner_links` (`child`, `parent`,
`child_key`, `parent_key`). Definition resolves these to numeric owner/column
identities, requires an explicit `behavior.storage` snapshot, rejects cycles or incompatible keys/encodings, and requires an
ordinary physical foreign key. A field owner must be the local owner or an
ancestor. Inherited scalar reads, projections and filters use ordinary SQL
through those prepared owners. Inherited relation links, `outer()` and
`distinct(on=...)` on inherited columns need an expanded host primitive.

The Rust host API `orm_engine::ownership::prepare_write` consumes a typed
`WriteContract<WriteValue<sea_query::Value>>` with one `OwnerWrite` per owner,
ordered parent to child. `WriteValue::Returned` copies a previously returned
primary key; explicit owner links prevent ambiguous key propagation. Values
are typed native inputs and native validators run before execution.
Ancestor inserts may supply their shared primary key explicitly even when the
logical child exposes only its local identity. Every written owner must belong
to the declared ancestor chain.
`run_write` executes the prepared insert sequence in its own transaction or
savepoint and rolls back that scope on failure. It supports a complete logical
return shape. It requires an explicit physical schema and currently supports
single composed inserts. Updates, bulk identity strategies, attach and deletion
policy remain plan 06 work. Ordinary model writes reject inherited storage
instead of writing nonexistent columns. Full filters, selection defaults,
proxy warnings, loaders and the composition-facing language APIs belong to
plans 03–06; this framework does not expose those feature APIs yet.

## Native behavior profiles

Exports declare stable `id`, `rust`, `kind`, `input`, and optionally `output`:

| Kind | Rust signature |
|---|---|
| `string_validator` | `fn(&str) -> Result<(), String>` |
| `string_transform` | `fn(&str) -> Result<String, String>` |
| `string_record_validator` | `fn(&[&str]) -> Result<(), String>` |
| `string_computed` | `fn(&str) -> Result<String, String>` |

Inputs are scalar String/Text fields. Transforms run in declared order before
validators. Record validators inspect transformed values through borrowed inputs.
Bulk writes validate their supplied rows before executing SQL. Expression writes,
upsert conflict outcomes and omitted/defaulted validated insert values are
rejected; use database constraints or explicit transactions for those strategies.
Null is rejected unless a field rule explicitly sets `allow_null: true`, in which
case its field functions are skipped. Record dependencies require all non-null
values whenever any dependency is updated; partial records are rejected.
Raw SQL bypasses ORM write validation.

Computed fields are read-only and depend on one stored scalar string field.
Projection loads that dependency; model and returning shapes reuse its selected
value. Computation occurs inside the existing native call before materialization,
without language callbacks or hidden queries. NULL dependencies produce NULL.
Native-only computations cannot be used in SQL filters, ordering, grouping,
expressions, conflict targets or CTE outputs. Nested/cyclic computed dependencies
require an expanded profile and currently fail at build time. Relation-dependent
computation is not supported by this primitive.

Static model methods use scalar string exports with generated Python/TypeScript
types and collision checks. Names must be valid Python identifiers, excluding
keywords and `__debug__`. Generated write types omit computed fields and exclude
expressions for native validated fields. Generation and runtime definition both
consume normalized schema requirements and the same compiled adapters.

Profiles fingerprint the normalized model, relevant enums/storage, native
configuration, extension composition, resolved dependencies and local source
content. Runtime definitions can add unrelated models but must match compiled
behavior shapes and versions. A stale profile fails before connection or queries
with a rebuild instruction. There is no interpreter fallback.

## Execution and performance

A core-only build has no composition dependencies, model behavior state or
execution hooks. Enabled profiles generate direct Rust calls and numeric field
positions; runtime execution does not parse manifests or declarations. Dispatch
for computation is resolved at the output-shape boundary rather than for each
row. The host retains binding, SQL execution and transaction ownership.

See `bench/build-time-extensions/` for paired baseline comparisons, allocation
measurements and enabled-profile controls. Warm framework cost must be below
1%; definition, build duration and artifact size are reported separately.
Timing intervals that cross the limit remain inconclusive.
