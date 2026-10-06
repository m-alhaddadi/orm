# Partial model selection and schema-defined query defaults

## Outcome

Schema declarations set default filters, selected fields, and loaded references.
`select-out` changes default loading rather than removing the field from the model.
All policies are compiled by the optional feature; no dynamic policy registry is
consulted per row.

## Existing behavior

`python/orm/query.py::select` returns projection rows/scalars; it is not a partial
model API. `python/orm/fields.py::Field.__get__` raises `AttributeError` for a missing
instance value. Unloaded one-to-one relations raise `NotLoaded`. TypeScript models
currently assign selected row values directly. Do not document a shared missing-field
exception as existing behavior.

## Selection semantics

- Preserve the existing projection `select()` API.
- Add a separately reviewed partial-model selection API and result typing.
- A schema field excluded by default remains usable in expressions and writes and
  can be explicitly loaded. Exclusion is not authorization or a write restriction.
- Missing model fields raise a consistent `NotLoaded` error in both languages.
  Implement the absent-field behavior without slowing ordinary loaded-field reads.
- Omitted and SQL NULL are distinct: NULL is a loaded value.
- Keep the primary key available internally for row identity and instance writes.
  Other helper columns can be selected internally without exposing them as loaded
  public fields; avoid eagerly fetching excluded large values for implementation ease.
- Serialize only public loaded fields. Refresh should preserve the instance's
  projection unless an explicit expanded projection is requested.
- Missing required insert fields must have a schema/client/database default or be
  supplied by the caller. `select-out` does not make an insert column optional.

Compile ordered output fields and positions; current code assumes whole-model
field ordering during materialization and `_apply_row`. Update instance refresh,
update-returning, serialization, and relation join decoding to respect actual shapes.

## Default filters

Schema expressions can reference `parent.default_filter`. A declared filter replaces
the inherited filter; composition occurs only where the expression requests it.
Resolve references and detect cycles during schema compilation.

Proposal for absent declarations: inherit the nearest parent default. Provide an
explicit no-filter declaration to clear it. Freeze exact syntax during API review.

Filters apply consistently to reads, count/exists, and update/delete queryset
selection. Inserts use schema defaults; they do not infer inserted values from a
filter. A write may move a row outside the proxy's default filter. Return the actual
written row without silently reverting the mutation; a later default-filtered read
may no longer find it. Define an explicit queryset operation to bypass defaults,
independent of user-supplied filters, with equivalent Python/TypeScript semantics.

Relation loading into a filtered target must apply the target default. Put predicates
in the join condition or equivalent scoped plan so missing targets do not remove
source rows through an accidental WHERE clause. This returns absence in the filtered
view of a relation, not proof that no physical target exists.

## Default selection and loading

A model can declare default selected fields and default loaded references in its
schema. Selected field lists replace inherited lists when explicitly declared;
otherwise inherit them. Provide an explicit reset to all fields.

Default reference loading reuses `select_related` / `selectRelated`. Explicit loading
adds named references. Provide a way to clear inherited/default eager loading.
Reject recursive default loading graphs or require a finite expansion; do not
silently create unbounded joins along reference cycles.

## Tickets and acceptance

1. Define output-shape/loaded-state IR and generated types for partial models.
2. Add partial selection and consistent absent-field access in both bindings.
3. Compile schema defaults and parent references, including reset semantics.
4. Apply defaults to roots, related targets, counts, and write selection.
5. Audit serialization, refresh, instance mutation, and internally selected columns.

Verify omitted versus null fields, excluded required insert fields, explicit
selection, defaults bypass, relation joins, and write-returning outside a filter.
Check both databases/languages and absence of default-policy overhead in a build
without the feature. Custom validators are outside this plan.
