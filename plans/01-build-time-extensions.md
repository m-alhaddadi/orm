# Build-time extension system

Status: specification for implementation; the extension APIs described here are not shipped behavior.

## Problem Statement

Application developers need to extend the ORM beyond its built-in model behavior without maintaining a fork or spreading feature-specific policies throughout the host.
The required extensions include custom Rust validators and transformations, field changes, computed results, model methods, same-table proxy inheritance, and inheritance through storage composition.
They must work through equivalent Python and TypeScript APIs and supported PostgreSQL and SQLite configurations.

Runtime plugin discovery, callback chains, and repeated declaration interpretation would add work to every query or decoded row.
Preparing a callback registry once does not remove its execution overhead.
Developers need optional extensions to behave as efficiently as equivalent built-in code, with disabled functionality excluded from the native artifact.

The current database extension catalog describes database types, functions, and index capabilities; it is not a behavioral extension contract.
Generated models and runtime-loaded schemas also need one compatible extension model so they cannot diverge in validation, query semantics, or decoding.

## Solution

Build selected extensions and application Rust code into a native artifact with fixed composition.
Extensions contribute typed schema transformations, prepared model behavior, migrations, generated language APIs, and compiled execution implementations.
They can change the logical model and its fields while explicitly declaring any physical storage effect.

Resolve dependencies and generate native composition before the native build.
Finish schema interpretation, compatibility checks, reference resolution, and native preparation during `define(_SCHEMA)` or its equivalent schema-definition entry point.
Queries then use immutable prepared state and ordinary compiled planning, validation, execution, and decoding code.

Enabled features pay for their actual behavior, including validation, joins, and diagnostics, with no additional plugin dispatch overhead compared with an equivalent built-in implementation.
Disabled builds contain no extension execution code, runtime dependencies, per-operation availability checks, or extension-specific runtime state.
Initialization, build duration, and artifact size are separate costs.

Learn from Prisma's named, reusable, typed model/client/query/result extensions and explicit computed-field dependencies.
Extend that surface to schema and field transformations, storage ownership, migrations, native Rust, and inheritance.
Prisma's inspected implementation caches callback resolution but executes query callback chains; this system generates static composition instead.
Faster end-to-end queries than Prisma are a benchmark question, not an acceptance assumption.

## User Stories

1. As an application developer, I want optional extensions compiled into my native ORM artifact, so that enabled behavior has the same execution structure as built-in behavior.
2. As an application developer, I want disabled extensions excluded from runtime dependencies and execution state, so that unused features do not affect ordinary queries.
3. As an extension author, I want to distribute a normal Rust crate, so that users can install my extension without editing the host's extension list.
4. As an application developer, I want to supply a local Rust module or crate, so that application-specific behavior does not require a published package.
5. As an application developer, I want extension configuration resolved before the native build, so that the resulting artifact has a known fixed capability set.
6. As an application developer, I want schema preparation completed during definition, so that the first query does not resolve extensions or interpret declarations.
7. As an application developer, I want generated models and runtime-loaded schemas to use the same contracts, so that equivalent schemas behave consistently.
8. As an application developer, I want actionable compatibility errors during definition, so that an incompatible native artifact fails before I execute queries.
9. As an extension author, I want namespaced attributes with typed arguments and source locations, so that invalid declarations produce useful diagnostics.
10. As an extension author, I want deterministic pass ordering and explicit dependencies, so that extension combinations produce reproducible artifacts.
11. As an extension author, I want conflicting ownership and transformations rejected, so that incompatible extensions do not silently change each other's meaning.
12. As an extension author, I want typed schema, query, write, result, migration, and generation contracts, so that I can contribute behavior without bypassing host guarantees.
13. As an application developer, I want to add stored or computed fields through extensions, so that new domain behavior can ship outside the host.
14. As an application developer, I want to rename or remove public fields independently of storage, so that a view change does not accidentally rename or drop database columns.
15. As an application developer, I want explicit physical field transformations to produce migrations, so that storage changes are reviewable and reproducible.
16. As an application developer, I want logical fields to retain their storage owners, so that inherited fields use the correct table in queries and writes.
17. As an application developer, I want field defaults prepared once and distinct from database defaults, so that client behavior does not silently change other database writers.
18. As an application developer, I want fields excluded from default selection to remain explicitly loadable and writable, so that selection policy does not become an implicit access restriction.
19. As an application developer, I want null and unloaded values distinguished, so that partial model reads preserve their actual state.
20. As an application developer, I want schema-defined filters and loading defaults, so that model behavior is consistent across reads, counts, relations, and write selection.
21. As an application developer, I want inherited defaults to support replacement and explicit reset, so that proxy chains have predictable behavior.
22. As an application developer, I want same-table proxy models, so that several logical views can share one physical table.
23. As an application developer, I want proxy narrowing violations to preserve actual values and emit aggregated warnings, so that a mistaken view declaration does not discard rows or change pagination.
24. As an application developer, I want strict validation declared separately from proxy narrowing, so that adding validation does not silently change warning semantics.
25. As an application developer, I want shared-primary-key child models, so that a child can store local fields while exposing fields owned by its parent.
26. As an application developer, I want one parent to have independent Employee and Customer records, so that composition does not impose exclusive sibling types.
27. As an application developer, I want parent queries to return parent objects, so that composition does not introduce automatic downcasting.
28. As an application developer, I want ordinary named relations and explicit async loaders for composed models, so that loading preserves identity and transaction context without implicit I/O.
29. As an application developer, I want composed creates and updates executed atomically, so that a failure cannot leave some storage owners changed and others unchanged.
30. As an application developer, I want an explicit attach-to-existing-parent operation, so that creating a child does not overwrite its parent's fields.
31. As an application developer, I want bulk composed writes to use one consistent matched identity set, so that changing parent fields does not change which children are updated mid-operation.
32. As an application developer, I want child deletion to preserve ancestors and siblings, so that deleting one role does not delete the whole person.
33. As an application developer, I want parent deletion to cascade through ordinary dependent foreign keys, so that composed records follow declared storage ownership.
34. As an application developer, I want custom Rust field and record validators, so that domain rules execute natively without per-row language callbacks.
35. As an application developer, I want typed value transformations ordered before validation, so that validators inspect the values intended for the write.
36. As an application developer, I want explicit validation rules for null, omission, partial updates, bulk operations, expressions, and upserts, so that unsupported value modes cannot silently bypass rules.
37. As an extension author, I want borrowed typed validator inputs and prepared configuration, so that validation avoids unnecessary copies, allocation, and repeated parsing.
38. As an application developer, I want computed fields with declared dependencies, so that only required inputs are loaded and computation causes no hidden database access.
39. As an application developer, I want SQL-capable computations distinguished from native computations, so that filtering and aggregation promises reflect actual database support.
40. As an extension author, I want generated model/client methods and language types, so that extension APIs are ordinary methods with collision checks and accurate input/output types.
41. As an application developer, I want failed schema definition to preserve the previous registry, so that configuration errors do not corrupt working models.
42. As an application developer, I want executing queries to retain an immutable schema snapshot, so that a later definition cannot change their behavior.
43. As an application developer, I want unsupported native behavior combinations rejected with a rebuild instruction, so that runtime loading does not silently switch to a slower plugin interpreter.
44. As a release maintainer, I want extension and schema contract versions recorded in artifacts, so that stale generated models cannot accidentally use incompatible native code.
45. As a release maintainer, I want exact builds and useful prebuilt profiles, so that third-party native code can be distributed without pretending package installation modifies an existing binary.
46. As an application developer, I want existing database extension imports preserved, so that adopting behavioral extensions does not break type/function catalogs.
47. As a performance maintainer, I want direct-versus-extension comparisons with equivalent behavior, so that framework overhead is distinguished from extra joins, checks, and computation.
48. As a performance maintainer, I want build, preparation, allocation, and execution costs reported separately, so that specialization and setup costs remain visible.

## Implementation Decisions

1. **Scope and host boundaries.** Provide typed contributions for declarations, model/field transformations, planning, native validation/transformation, results, migrations, and language APIs. The host owns parameter binding, database execution, transaction boundaries, relation execution, and loaded-state tracking. New execution primitives require an explicit versioned host-contract change; do not add speculative general hooks.
2. **SDK dependency direction.** Introduce a small shared contract layer that depends on neither extensions nor bindings. Keep compiler-only dependencies separate from runtime contracts where practical. Extensions and host implementations depend on this layer; a generated composition crate connects the selected host, extensions, application Rust code, and binding.
3. **Build frontend.** Generate the composition dependency manifest before the final native build, use Cargo to resolve package versions and produce the lockfile, read resolved extension metadata, then generate fixed composition and invoke the ordinary binding build. The frontend resolves extension semantics and pass ordering, not Rust package versions. Third-party extension IDs must not require new host policy branches.
4. **Build-script limits.** Build scripts may generate source from already selected dependencies; they cannot add dependencies or enable optional dependencies through compiler configuration. Do not recursively build the final package from its own build script. Compiler code runs on the build host, runtime implementations compile for the target, and all configuration/source inputs participate in rebuild detection.
5. **Optional compilation.** Use additive Cargo features, optional dependencies, and conditional compilation for host capabilities. Audit feature unification across tooling and both bindings. Exclude disabled extension runtime dependencies, execution code, availability branches, and extension-specific runtime layout. A lockfile entry alone does not mean a dependency is present in the runtime build.
6. **Canonical manifest.** Store behavioral metadata in Cargo package metadata, optionally referencing a manifest resource packaged with the crate. Declare extension identity/version, host/declaration/prepared-data contracts, extension dependencies, host capabilities, languages/databases, attribute targets/arguments, pass phases/effects/order, exported runtime behaviors, and compiler/runtime adapter requirements. Cargo remains authoritative for package dependencies.
7. **Separate database catalogs.** Preserve existing imported definitions of database types, functions, and indexes. Behavioral manifests are a separate contract; catalog imports do not become executable-code imports.
8. **Artifact requirements.** Compiled schemas record their schema IR version, used extension requirements, and lowering phase state. Native artifacts expose available contract versions and stable compiled export IDs. Specialized behavior records fingerprints covering normalized types, configuration, dependency composition, and every schema assumption embedded in generated code. Process addresses and unstable Rust type names are not persistent identities.
9. **Compatibility failure timing.** Definition rejects missing exports, incompatible contracts, unsupported languages/databases, unknown required metadata, and stale specialization fingerprints. Errors name the declaration location, requirement, available artifact, and corrective action. Live database capabilities are checked during connection initialization. Value-dependent errors and unsupported dynamic write modes remain operation-time validation/planning errors.
10. **Preparation pipeline.** Native builds order passes and generate composition. Schema compilation or definition validates declarations, expands logical models, resolves storage, prepares policies/behavior, checks invariants, and emits or binds artifacts. Mark completed phases so definition does not apply lowering twice to normalized generated schemas.
11. **Deterministic composition.** Topologically order passes within host phase boundaries, using stable extension/pass IDs to break ties between independent passes. Reject missing dependencies, cycles, duplicate attribute ownership, invalid phase dependencies, and conflicting transformations without an explicit composition contract. Record transformation provenance and require versioned dependencies when consuming another extension's contract.
12. **Semantic validation.** Pass ordering cannot prove arbitrary Rust code compatible. Check declared effects and host invariants after relevant phases, reject unresolved conflicts, and identify both contributions in diagnostics. Do not claim automatic inference of arbitrary transformation semantics.
13. **Typed schema contracts.** Separate declarations with source locations, logical models/fields/defaults/relations, and physical storage owners/columns/encoding/constraints/provenance. Use stable resolved identities to connect logical fields to storage. Declaration metadata may be serialized, but execution consumes typed prepared state without name-based extension lookup or generic metadata deserialization.
14. **Typed execution contracts.** Query contracts carry expressions, bound value references, ownership, selected/helper fields, and result shapes. Write contracts carry operation kind, supplied/omitted values, affected identities, owner writes, returned values, and validation dependencies. Result contracts carry physical versus logical types, field positions, loaded-state mapping, computed dependencies, and diagnostic requirements. Generation contracts carry typed methods and collision information.
15. **Field transformations.** Allow additions, removals, renames, overrides, and derivation of logical fields, with explicit logical/physical effects. Public renames retain storage identity unless a physical rename is declared. Hiding/removing a public field does not drop its column. Stored additions/type changes require migrations and compatible encoding; physical renames preserve provenance. Proxy-specific override restrictions still apply.
16. **Field defaults and selection.** Normalize client defaults once and preserve separate database defaults. Selection exclusion retains expression/write access, explicit loading, and required insert constraints. Null is a loaded value; omission is unloaded. Primary keys/helper columns may be internally selected without exposing helper fields as publicly loaded. Refresh, serialization, returning, and relation decoding use actual output shapes.
17. **Policy inheritance.** Resolve parent-default references and cycles during setup. An explicit default filter or selection replaces the inherited policy; composition occurs only when declared, and explicit reset clears it. Apply filters consistently to reads, count/exists, relation targets, and update/delete selection. Scope target filters to joins so absent targets do not remove source rows. Filters do not synthesize insert values; returning exposes actual writes even if a row leaves the filtered view. Default loading is bounded and can be explicitly cleared.
18. **Proxy inheritance.** Resolve chains to logical model identities sharing one physical owner. Support selection exclusions, non-null/enum-subset views, client defaults, and inherited policies. Proxies add no tables or narrowed physical constraints and cannot change encoding or PK identity. Do not infer filters from narrowing. Selected violations preserve values and aggregate structured warnings without raw values, extra inspection queries, or post-decode row removal. Runtime enum representation remains compatible with unexpected values.
19. **Storage composition.** Support one parent per child, shared PK/FK identity, ancestor chains, owner-resolved flattened fields, and ordinary one-to-one relations. Child PKs do not independently auto-increment. Reject cycles and ambiguous fields. Child reads join required ancestors using correct aliases; parent reads retain parent type and do not fetch every child. Siblings are independent records. Migrations emit each physical owner's columns once.
20. **Composed writes.** Produce host transaction plans for root-to-leaf creates and shared-key propagation, explicit attach without overwriting parents, multi-owner updates, and actual stored returning values. Bulk writes use one consistent matched identity set with database-appropriate locking/transaction semantics. Failures roll back the operation and never commit an existing outer transaction. Constraints protect attach/create races. Child deletion preserves ancestors/siblings; parent deletion cascades through ordinary dependent FKs.
21. **Reference loading.** Reuse ordinary relation planning and explicit async loaders with connection/transaction routing, per-instance identity, cached optional absence, reload/invalidation, and concurrent-load behavior from the reference-loading plan. Synchronous field access performs no hidden database work. Detect generated-method collisions.
22. **Custom Rust exports.** Accept normal Cargo crates and local Rust modules included in composition. Resolve stable export IDs to type-checked validators, transformations, computed values, decoders, compiler passes, and plan transformations. A schema function name alone cannot introduce new executable code. Rust extensions have the trust model of application dependencies; host contracts provide invariants, not a native-code sandbox.
23. **Direct validation composition.** Generate direct Rust calls or concrete generic dispatch for each required model/behavior contract. Prefer borrowed typed inputs, typed configuration prepared during definition, and failure-only error allocation. Record validators declare their field dependencies; multiple transformations and validators have deterministic direct sequencing. Run them within existing native write calls, including bulk writes, with no additional per-row language callbacks or FFI crossings.
24. **Validation ordering and compatibility.** Preserve existing public default handling and error timing where applicable. The value pipeline is conversion and default resolution, declared transformations, validation, then binding/write planning. Do not validate the same prepared value at multiple layers. Client defaults resolve before transformations; database defaults remain server behavior. Strict validators are explicitly separate from warning-only proxy narrowing.
25. **Validation modes.** Contracts declare null handling and supported insert/update/bulk/expression/upsert modes. Partial field updates validate supplied changed values. Record validators needing missing fields require an explicit transaction plan to obtain them or reject the operation before execution. Database-generated final values and conflict outcomes cannot always be validated from input; require a constraint/transactional strategy or reject unsupported modes. Raw SQL does not implicitly gain model validation.
26. **Computed results.** Declare typed dependencies, output, and SQL/native/accessor evaluation policy. Resolve cycles and dependency expansion during definition; ordinary shape planning applies prepared rules to dynamic selections. Load only requested dependencies and preserve hidden-helper versus public-loaded state. Relation dependencies require bounded explicit loading. Only SQL-lowerable computations can support database filtering/ordering/aggregation; arbitrary Rust is not automatically translatable to SQL. Avoid eager computation of every declared field.
27. **Decoding.** Generate ordinary shape-aware decoding/computation and requested diagnostics. Choose decoding at the operation/shape boundary without a plugin callback per field or row. Dynamic shapes remain ordinary planner work. No extension registry, declaration parser, or manifest compatibility check runs during materialization.
28. **Runtime schemas and specialization.** Runtime-loaded schemas can lower into ordinary prepared host representations, including proxy/composition metadata, or bind a matching compiled Rust specialization. Definition cannot create new machine code. A cached callback pointer/list still has execution overhead. Unsupported Rust combinations requiring specialization fail during definition with a rebuild instruction; no silent interpreter, JIT, or dynamic library fallback is permitted.
29. **Specialization size.** Generate only required behavior/model specializations, not the Cartesian product of hypothetical combinations. Keep parameters, filters, projections, and ordinary model routing dynamic through normal host mechanisms. Extra branches must implement necessary semantics and be present in the equivalent built-in control. Measure code size/build time and do not promise guaranteed inlining or identical assembly.
30. **Atomic definition.** Prepare a candidate registry snapshot, resolve references, create native state/class associations, validate, and publish atomically before definition returns usable models. Failure preserves the old registry. Existing queries/instances retain their immutable snapshot. New definitions explicitly prepare new snapshots rather than modifying executing queries.
31. **Registry compatibility.** Resolve references against existing registered models or the same definition batch. Reject unresolved forward references with instructions to define dependent schemas together. Preserve class-declaration APIs with an explicit preparation entry point after registry changes; first query must not perform extension preparation. Apply equivalent definition semantics in both languages.
32. **Adapters and distribution.** Generate ordinary model/client methods and accurate field/input/output types for supported bindings; reject collisions. Generated and runtime APIs share compiled native implementations. Source builds contain custom Rust; package installation cannot inject it into existing binaries. Use native profiles/exact builds from the packaging plan, record reproducible configuration, and keep one native profile authoritative throughout a transaction.
33. **Delivery and scope.** First deliver contracts/eager preparation, then the build frontend/static wiring, a schema-lowering proof, and a specialized Rust validator/computed-field proof in both bindings. Separate catalogs and record artifact compatibility. Use proxies/composition as contract consumers; their complete user semantics remain defined in their linked plans. Complete those dependent features in their own slices after the framework gates pass. A manifest-only or callback-based proof does not complete this framework spec.
34. **Agent autonomy.** Inspect existing implementation before changing contracts, preserve unrelated workspace changes, and use existing domain vocabulary. Implement routine internal choices directly and document APIs. New source syntax needs parser fixtures and documentation; unresolved incompatible public requirements must be surfaced rather than silently invented. Do not mark unsupported or unverified capabilities complete.

## Testing Decisions

1. **Primary seam.** Prefer the existing public schema-definition → query/write → returned model behavior through Python and TypeScript. Submit equivalent schemas and operations, then assert values, error timing, warnings, loaded state, query effects, and transaction outcomes. Use this as one shared behavioral contract rather than testing each internal pass independently.
2. **Build seam.** Native artifact generation needs a separate build-level seam because public queries cannot prove disabled dependencies or static composition. Build a core-only artifact and a local third-party extension artifact without changing the host extension list. Check selected runtime dependencies, compatibility metadata, reproducible normalized output, and generated composition. Generated-code/layout inspection is structural performance evidence, not a substitute for behavioral tests.
3. **Test quality.** Assert external behavior and stable contract diagnostics. Avoid tests coupled to private pass layouts or snapshots of every generated line. Use focused native contract tests only when build/preparation failures or operation-cost isolation cannot be observed reliably through public APIs.
4. **Prior art.** Reuse existing schema compiler fixtures, migration/snapshot tests, public SQL-shape tests, Python/TypeScript query and value tests, database migration tests, prepared-query tests, SQLite coverage, and strict language typing checks. Reuse the existing paired public API benchmark approach and query-construction measurements. Follow the repository's documented implementation checks when code changes are made.
5. **Lowering parity.** A namespaced lowering extension must match handwritten behavior in normalized schema meaning, SQL, physical migrations, and returned results. Cover source-loaded and generated-schema paths and show completed passes are not applied twice.
6. **Combination failures.** Verify deterministic results for equivalent manifests/lockfiles and actionable errors for missing dependencies, cycles, duplicate ownership, conflicting effects, wrong types, unknown required metadata, unsupported database/language, and stale behavior fingerprints. Setup failures occur before usable models or queries are returned.
7. **Field effects.** Test logical renames/removals/exclusion without unintended column changes, explicit physical transformations with one migration contribution, preserved provenance, type/collision errors, default precedence, and null versus omission. Test serialization, refresh, returning, and relation shapes through both bindings.
8. **Proxy consumer.** Test inherited replacement/reset, explicit parent-filter composition, target filters preserving source rows, unchanged physical migration, narrowing that warns and retains values, aggregated diagnostics, counts/pagination, explicit hidden-field loading, and writes returning rows outside the default filter. Full feature coverage follows the proxy/default plans.
9. **Composition consumer.** Test independent Employee/Customer siblings, longer chains, inherited ownership, parent result types, root-to-leaf keys, attach races, rollback after each owner failure, bulk identity consistency, outer transaction ownership, returning, and child/parent deletion. Verify PostgreSQL/SQLite parity as each slice is delivered.
10. **Native Rust consumer.** Exercise field/record validators and computed values through both bindings, including bulk writes. Cover defaults/transformation order, null, omission, partial inputs, missing record dependencies, wrong export types, expression/upsert rejection or supported strategies, and no implicit raw-SQL validation.
11. **Preparation and loading.** Verify failed definition preserves the prior registry, executing queries retain snapshots, first query performs no extension preparation, supported runtime schemas reuse static adapters, unsupported Rust combinations request rebuilds, and live capability checks occur during connection initialization.
12. **Computed dependencies.** Test cycles, requested-only evaluation, hidden dependencies, omitted values, bounded explicit relation loading, and no hidden accessor I/O. Native-only computed fields must not claim unsupported database filtering or aggregation.
13. **Equivalent controls.** Compare disabled, handwritten built-in, and extension builds with the same behavior, native functions, data, SQL work, optimizer settings, and release profile. Separate build/artifact size and definition costs from warm query construction, planning, validation, decoding, and awaited public execution.
14. **Performance gate.** Use paired repeated measurements and report distributions/uncertainty. Include simple reads, writes, bulk writes, dynamic shapes, and proxy/composed workloads. Measure allocations and inspect dispatch, metadata parsing, argument cloning, and FFI crossings. Use focused native timings alongside public calls so database latency does not conceal framework overhead. Timing parity alone cannot prove static composition; repeatable machinery overhead is an unmet requirement until resolved.
15. **Completion evidence.** Report delivered capabilities, explicit unsupported modes, behavioral/database/language coverage, artifact/dependency findings, and build/setup/runtime measurements. Stop optional verification when required gates pass and remaining concrete risks are resolved. Do not claim general speedups over Prisma from this architecture alone.

## Out of Scope

- Runtime discovery of executable plugins, hot-loading Rust into an existing binary, JIT compilation during definition, monkeypatching, and callback-registry fallbacks.
- Automatic translation of arbitrary Rust into SQL or acceptance of every behavior combination without a rebuild.
- A universal extension hook for unrelated future features or a sandbox for arbitrary native code.
- Automatic polymorphic downcasting, exclusive sibling types, multiple composed parents, reverse parent deletion through an owning foreign key, and trigger-driven ancestor insertion in the initial inheritance implementation.
- Reparenting populated tables or converting independent identities to shared primary keys without a separate migration design.
- Implicit asynchronous database access from synchronous fields or computed properties.
- Per-row Python/JavaScript validator callbacks as part of the native extension contract.
- Replacing the existing projection-selection API or treating selection exclusion as authorization.
- Implementing every extension policy inside the host. The host supplies primitives; feature crates own their policies.

## Further Notes

The framework must make the planned inheritance and field behavior implementable; a narrow client-method wrapper is insufficient.
Full defaults, loader, proxy, and composition features have their own implementation slices. Their agreed semantics are preserved by this spec:

- [Optional dependencies and distribution](02-optional-packaging.md).
- [Selection and schema-defined query defaults](03-selection-and-defaults.md).
- [Explicit async reference loading](04-async-reference-loading.md).
- [Same-table proxy models](05-proxy-models.md).
- [Composition with shared primary keys](06-model-composition.md).

Issue tracker and triage-label configuration were not provided in this session or the repository instructions inspected.
This file is the local deliverable. Tracker publication requires configuration from `/setup-matt-pocock-skills`; when published, apply `ready-for-agent`.

Research references:

- [Cargo features](https://doc.rust-lang.org/cargo/reference/features.html): optional dependencies and feature unification.
- [Cargo build scripts](https://doc.rust-lang.org/cargo/reference/build-scripts.html): source generation, build-host dependencies, and configuration limits.
- [Cargo metadata](https://doc.rust-lang.org/cargo/commands/cargo-metadata.html): resolved dependencies and package metadata.
- [Rust generic performance](https://doc.rust-lang.org/book/ch10-01-syntax.html#performance-of-code-using-generics): static composition through monomorphization.
- [Rust code generation attributes](https://doc.rust-lang.org/reference/attributes/codegen.html): optimizer limits and inlining hints.
- [Prisma extension surfaces](https://docs.prisma.io/docs/orm/v6/prisma-client/client-extensions) and [computed fields](https://docs.prisma.io/docs/orm/v7/prisma-client/client-extensions/result): reusable typed APIs and explicit dependencies.
- [Prisma resolution source](https://raw.githubusercontent.com/prisma/prisma/main/packages/client/src/runtime/core/extensions/MergedExtensionsList.ts) and [query execution source](https://raw.githubusercontent.com/prisma/prisma/main/packages/client/src/runtime/core/extensions/applyQueryExtensions.ts): inspected implementation, not a pinned compatibility dependency.
