# Generic foreign keys with a schema-generated ContentType enum

Status: specification for implementation; generic relations and multi-file schema
composition described here are not shipped behavior.

## Problem Statement

Application developers need one record, such as a tag, attachment, or activity,
to reference records from several model tables through one named relation.
Ordinary foreign keys bind each relation to one target model.

The desired behavior follows Django's generic foreign key concept, but model
types must be defined by the application schema rather than registered as rows in
a database table. Models may later be split across multiple schema files; their
stored type identities must survive that move.

## Solution

Generate one shared ContentType enum from the complete application schema set.
Require globally unique model names across every file in that set, and use those
names for readable enum members. Store stable positive integer IDs as the enum
values. Keep the generated model-to-ID mapping in a version-controlled schema
identity manifest, so model additions never renumber existing values and removed
IDs cannot be reused. Each referencing row stores a ContentType integer and an
object ID. An optional compiled extension
resolves that pair to the appropriate model and provides forward and reverse
generic relation behavior.

Enums remain a native core schema capability. The extension owns generic relation
declarations, target routing, loading, and relation writes. No model-type registry
table, registration rows, or database lookup of model metadata is introduced.

## User Stories

1. As an application developer, I want one relation to reference several models, so that tags, attachments, and activities can share one storage model.
2. As an application developer, I want ContentType generated from my schema models, so that I do not maintain a second list of model types.
3. As an application developer, I want model identities fixed before queries run, so that database contents cannot register new target types.
4. As an application developer, I want readable model-name enum members with frozen integer values, so that APIs are clear and stored references remain stable.
5. As an application developer, I want discriminator values unique across all schema files, so that each stored reference resolves to exactly one model.
6. As an application developer, I want to move models between schema files without changing ContentType values, so that reorganizing source files does not rewrite data.
7. As an application developer, I want relations to target models declared in another schema file, so that file boundaries do not restrict the domain model.
8. As an application developer, I want one shared enum in generated Python and TypeScript APIs, so that every generated model uses the same type identities.
9. As an application developer, I want runtime schema loading and generated models to agree, so that choosing a loading mode does not change stored references.
10. As an application developer, I want duplicate model names, table identities, or integer IDs rejected before database access, so that ambiguous schemas cannot produce usable models.
11. As an application developer, I want table renames to preserve frozen identities, so that a physical rename does not silently redirect existing references.
12. As an application developer, I want integer enum storage handled by the existing core enum machinery, so that PostgreSQL and SQLite use the same discriminator representation.
13. As an application developer, I want to write a saved target instance through a generic relation, so that its discriminator and object ID stay consistent.
14. As an application developer, I want to write an explicit type and key pair, so that bulk operations do not require loading every target.
15. As an application developer, I want invalid target models and incompatible key types rejected, so that malformed references do not silently reach the wrong table.
16. As an application developer, I want nullable references to clear both stored fields together, so that partially empty references are rejected.
17. As an application developer, I want synchronous access to preserve NotLoaded, so that reading an attribute does not perform hidden database work.
18. As an application developer, I want explicit asynchronous loading, so that I control database access and transaction context.
19. As an application developer, I want heterogeneous targets prefetched in batches, so that loading generic references avoids one query per source row.
20. As an application developer, I want identical object IDs in different target tables distinguished by ContentType, so that prefetching never mixes their results.
21. As an application developer, I want missing targets represented consistently, so that dangling references remain inspectable.
22. As an application developer, I want optional named reverse generic relations, so that targets can query and create related records through ordinary related-set APIs.
23. As an application developer, I want reverse queries to include both type and key predicates, so that records from other target tables cannot leak into results.
24. As an application developer, I want reference caches invalidated when either stored field changes, so that subsequent loads cannot return an old target.
25. As an application developer, I want generated target union types, so that Python and TypeScript describe the possible loaded objects accurately.
26. As an application developer, I want extension compatibility failures during schema definition, so that missing native functionality fails before queries execute.
27. As an application developer, I want ordinary enums to work without this extension, so that using enum fields does not require generic relation support.
28. As a maintainer, I want model additions and removals to produce reviewable migrations, so that stored references are preserved or deliberately migrated.
29. As a maintainer, I want the generated identity manifest committed with the schema, so that every environment assigns the same IDs.
30. As a maintainer, I want removed IDs retained as tombstones, so that old references can never resolve to a newly added model.
31. As an application developer, I want explicit rename metadata to retain a model's ID, so that renaming a model does not become a delete-and-create operation.

## Implementation Decisions

The enum/extension boundary, autogenerated enum, multi-file support, and integer
storage are user requirements. The design uses globally unique model names for
schema references and a committed identity manifest for frozen storage IDs.
The remaining behavior below is the implementation proposal for this plan; exact
declaration syntax needs API review.

1. **Core and extension ownership.** Core retains ordinary enum parsing, validation,
   representation, migration, value conversion, and language generation. Schema
   preparation synthesizes ContentType as an ordinary core enum. The optional
   generic relation extension requests that synthesis and contributes relation
   semantics through the build-time extension contracts. No special generic
   relation behavior is activated merely by declaring an enum.
2. **One schema set.** Collect all explicitly included schema sources into one
   logical application schema before resolving cross-file references or generating
   ContentType. A single file is a one-element set. Future multi-file loading must
   use the same collection contract, rather than merging independently generated
   enums. Retain each declaration's original source location. The existing import
   mechanism loads extension catalogs; model-file inclusion requires its own
   defined contract and must preserve those catalog imports.
3. **Collection consistency.** Compilation, validation, migration snapshots,
   generated bindings, and runtime definition consume the same complete schema
   set. Reject duplicate model declarations, conflicting datasource/dialect
   settings, conflicting enum declarations, and ambiguous generated names with
   diagnostics identifying both sources. File enumeration order must not change
   normalized identities or cause enum migrations. Do not scan unrelated files or
   construct a process-global enum across independent applications.
4. **Enum membership.** Include each concrete stored model in the schema set once.
   Generic relations may declare a narrower allowed target set; that restriction
   does not create a separate ContentType enum. Logical proxy models sharing one
   table share its storage identity and resolve to the concrete storage model in
   this first version. Distinct physical models cannot share a table identity.
   Proxy-specific generic identities are separate future work.
5. **Names and identities.** Model names are globally unique within the logical
   schema set, even when files differ; file paths are not namespaces. Public
   ContentType members use concrete model names. Stored values are positive
   signed 32-bit integers unrelated to table names, member order, or file order.
   Validate model-name uniqueness, generated language-name collisions, integer
   uniqueness/range, and physical table uniqueness independently. Respect table
   mappings and database identifier normalization; qualified table identities must
   remain distinct if database namespaces are introduced.
6. **Persistent allocation.** Generate one versioned schema identity manifest per
   application schema set, committed alongside the schema. Each entry records its
   immutable integer ID, current model name, and active/retired status. On first
   generation, allocate IDs starting at 1 in sorted model-name order. Later
   generations preserve every existing ID and append new IDs above the highest
   ever allocated value, sorting only the new models. Reserve 0 and never reuse a
   retired ID. IDs are persisted allocations, not ordinal positions or name hashes.
   Detect duplicate IDs from branch merges and require repair before generation;
   only IDs that have never shipped may be reassigned without a data migration.
   Exhaustion is an actionable generation error, never wraparound.
   Explicit generation updates the manifest atomically and exposes its diff;
   compilation, builds, CI, migrations, and runtime loading read and validate it
   without allocating missing IDs or silently rewriting it. Missing/stale mappings
   fail with an instruction to regenerate. Inline runtime schemas must supply the
   same identity metadata, or consume compiled IR containing it.
   Model renames require an explicit old-to-new association retaining the ID;
   table renames and file moves retain it automatically. Removed entries become
   permanent tombstones. Reintroducing the same model name does not implicitly
   resurrect its retired ID: restoration requires explicit identity intent.
   Embed resolved IDs in canonical schema IR, generated bindings, and migration
   snapshots. Runtime execution never needs a database metadata lookup. Final
   manifest location and rename syntax remain implementation choices.
7. **Database representation and evolution.** Synthesize ContentType with existing
   core integer enum storage on both PostgreSQL and SQLite: an integer column
   constrained to the active enum values, with normal enum conversion in bindings.
   Python exposes IntEnum members; TypeScript exposes named integer-valued members
   using its existing enum generation contract. The enum is native to core; no
   PostgreSQL CREATE TYPE enum is required for this discriminator. Ordinary user
   enums retain their configured storage modes.
   Adding a model adds one allowed ID without changing prior IDs. Removing a model
   retires its ID in the manifest and removes it from the active enum only after
   existing referencing rows are deleted, cleared, or explicitly migrated. A
   migration must fail rather than silently discard or reinterpret those rows.
   Compare against the prior snapshot to reject reassignment of shipped IDs.
   Canonicalize generated enum order by ID so model renames do not create a
   physical enum change. Keep artifact fingerprints sensitive to ID-to-model
   routing as well as enum membership.
8. **Stored reference.** Each generic relation names its discriminator column,
   object-ID column, logical relation, and allowed target models. The relation is
   virtual; only those two ordinary scalar columns are stored. Recommend an
   explicit composite index on the discriminator and object ID. Never create a
   conventional foreign key pretending that one column targets several tables.
9. **Key compatibility.** Initially support single-column primary keys with one
   compatible scalar/storage representation across all targets of a relation.
   Validate database type and binding conversion compatibility during preparation.
   Do not silently stringify mixed integer/UUID/string keys. Composite keys and
   heterogeneous key codecs require a separate design.
10. **Null and dangling state.** Required references require both columns;
    optional references allow both null or both present, with an ordinary check
    constraint enforcing paired nullability. A known type/key whose target no
    longer exists resolves to None/null after loading while preserving the stored
    pair, including for a required pair. Required storage does not promise target
    existence. Unknown discriminators and unmapped types are compatibility/data
    errors, distinct from missing targets.
11. **Writes.** Accept a saved instance of an allowed target or an explicit typed
    pair. Instance writes derive both values; clearing writes both null. Reject
    unsaved/disallowed targets, contradictory instance/pair input, and half-pairs.
    Explicit updates may change one field only when the resulting pair is valid;
    bulk and expression updates must enforce the same result invariant or reject
    unsupported modes. No implicit target creation or per-row existence query is
    required. Database foreign-key integrity cannot be promised for this pair.
12. **Prepared routing.** Resolve enum integer IDs to immutable model/storage identities
    during schema definition. Queries use compiled native relation behavior and
    prepared state rather than model-name parsing or extension callback registries.
    Respect the extension framework's artifact compatibility and snapshot rules.
    The current ordinary relation representation has one target; introduce only
    the versioned host contracts needed for generic routing and batch loading.
    Existing extension export kinds alone do not implement this feature.
13. **Forward loading.** Reuse the explicit async loader contract and transaction
    routing from the reference-loading plan. Attribute access preserves NotLoaded;
    a fully null pair can return None/null immediately. Cache successful loads and
    loaded absence; invalidate when either column changes through writes/refresh.
    Coalesce concurrent loads and permit forced reload as specified by that plan.
14. **Batch loading.** Extend ordinary prefetch to group deduplicated IDs by target
    identity and issue bounded queries per represented target, splitting at dialect
    parameter limits. Match results by both type and key, preserve source ordering,
    row counts, and pagination, and keep all queries in the caller's context.
    Include hidden discriminator/key dependencies when loading a partially
    selected model without exposing them as selected public values.
15. **Query contract.** Scalar discriminator/key filtering remains ordinary query
    behavior. Forward target equality lowers to both predicates. Unqualified
    traversal into heterogeneous target fields, ordering/aggregation across that
    union, and generic select_related joins are initially rejected with an
    actionable instruction to use prefetch or an explicit target query.
16. **Reverse relations.** An optional named reverse declaration binds one concrete
    target and one forward generic relation. Related-set reads, counts, filters,
    prefetch, and creation include the fixed type plus target key. Related creation
    fills both columns; deleting a referencing record never deletes its target.
    A reverse declaration does not silently enable deletion ownership.
17. **Deletion scope.** The initial extension leaves stored references intact when
    a target is deleted. Django's reverse GenericRelation can collect related rows
    for deletion; automatic reverse cascade is a separate follow-up requiring an
    explicit lifecycle contract covering instance/bulk deletion and transactions.
    Raw SQL and external writers cannot receive ORM-only integrity guarantees.
18. **Bindings and delivery.** Generate equivalent Python and TypeScript target
    unions, enum members, relation loaders, and reverse APIs with collision checks.
    Deliver schema-set collection and identity generation first, then extension
    preparation and forward writes/loading, then batch and reverse relations.
    Reuse optional packaging and exclude generic execution code from disabled
    artifacts. Report which slices are implemented rather than calling partial
    routing full generic relation support.

## Testing Decisions

1. **Primary seam.** Prefer the existing public schema-definition → write/query →
   returned-model seam in Python and TypeScript. Exercise equivalent schemas and
   operations through both bindings. Test values, errors, loaded state, query
   counts, and transaction outcomes rather than private routing structures.
2. **Prior art.** Reuse public relation/prefetch tests, enum round-trip tests,
   definition failure tests, strict generated typing checks, compiler fixtures,
   and migration snapshot/database tests. Focused schema-set fixtures complement
   the public seam where compilation diagnostics need direct coverage.
3. **Multi-file acceptance.** Compare one-file and split-file versions of the same
   models. Move declarations, reorder files, and reference models across files;
   assert identical stored integers, logical mappings, and migration meaning.
   Verify identical results through runtime-loaded and generated models.
4. **Identity acceptance.** Test mapped table names, duplicate table identities in
   different files, duplicate model names, generated member collisions, and
   retention of IDs through table/model renames. Add a model that sorts before
   existing models and verify no renumbering. Test retirement, name reuse, explicit
   restoration, branch-merge ID conflicts, missing/stale manifests, and ID range
   limits. Reject reuse or reassignment of a shipped ID. Verify a clean checkout
   reproduces IDs from the committed manifest and compilation/runtime loading do
   not mutate it. Snapshot and generated binding mappings must agree.
5. **Relation acceptance.** Round-trip at least two target tables with overlapping
   primary key values. Cover instance and pair writes, null pairs, disallowed
   models, incompatible keys, missing targets, unknown IDs, updates, bulk writes,
   loaded identity, cache invalidation, concurrent loads, and rollback.
6. **Batch and reverse acceptance.** Assert queries scale with represented target
   groups and parameter chunks, rather than source rows. Verify pagination,
   deduplication, partial selection, transaction routing, reverse filtering/counts,
   related creation, and no cross-type leakage for equal object IDs.
7. **Storage and compatibility seam.** Use existing migration checks to assert
   one shared enum and the expected scalar columns/checks/indexes, with no registry
   table or multi-target foreign key. Test enum additions/removals with existing
   data and warnings from existing migration machinery. Missing extension artifacts
   fail during definition; core-only enum use remains valid.
8. **Dialect evidence.** Cover integer ContentType enums on PostgreSQL and SQLite,
   including checks rejecting unknown/retired IDs and failed removal migrations
   while references remain. Unsupported combinations fail early. Verify intentional
   dangling references after target deletion and rejection of unsupported generic
   traversal rather than asserting nonexistent foreign-key enforcement.

## Out of Scope

- A runtime database catalog of model types or dynamically registered targets.
- Implicit asynchronous I/O during synchronous attribute access.
- Composite keys, mixed target-key codecs, automatic target creation, and generic
  traversal/aggregation across unrelated target field sets in the initial slice.
- Automatic proxy downcasting or distinct ContentType values for same-table views.
- Database triggers for referential integrity, automatic target-delete cascades,
  and reverse deletion ownership in this initial extension.
- Implementing the feature as part of this planning task.

## Further Notes

This plan depends on the [build-time extension system](01-build-time-extensions.md),
[optional packaging](02-optional-packaging.md), and
[explicit async reference loading](04-async-reference-loading.md). Multi-file
schema collection is necessary infrastructure; existing single-file compilation
and extension-catalog imports must not be described as existing model-file support.

[Django's generic relation documentation](https://docs.djangoproject.com/en/5.2/ref/contrib/contenttypes/#generic-relations)
provides the reference concept: a type/key pair, a virtual object reference,
optional reverse relations, and dangling references after target deletion.
This proposal replaces its database ContentType rows with schema-generated enum
identities and follows this ORM's explicit loading conventions. It does not claim
complete Django API or deletion compatibility.

Issue tracker and triage-label configuration were not provided in this session or
the repository instructions inspected. This file is the local deliverable.
Run `/setup-matt-pocock-skills` to configure publication; apply `ready-for-agent`
when the spec is published.
