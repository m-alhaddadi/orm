# Explicit async reference loading

## Outcome

One-to-one and belongs-to references support explicit database loading without
implicit I/O on synchronous attribute access. Generated loaders reuse relation
querying and the instance's database/transaction context.

## API proposal

```python
# Unloaded synchronous access retains NotLoaded.
person = await customer.load_person()
assert customer.person is person
```

```typescript
const person = await customer.loadPerson();
// customer.person is the cached reference after successful loading.
```

These names are proposals; generation must detect collisions with schema members
and other generated methods. A generic explicit loader is a fallback if collision
handling makes generated names impractical.

The exact pair `customer.person` raising `NotLoaded` and
`await customer.person()` loading cannot coexist on the same attribute: access
raises before the call is evaluated. A callable relation handle would require
changing synchronous reference access and wrapping loaded objects. Prefer generated
loader methods, preserving normal object identity and current attribute behavior.

## State and caching

- Unloaded: attribute access raises; explicit loader performs the query.
- Loaded and present: attribute and loader return the same cached instance.
- Loaded and absent: an optional reference returns `None` / `null`, and subsequent
  loader calls use the cached absence.
- Forced reload: explicitly refresh the reference; returned and cached objects agree.
- A physically missing required target is a data-integrity failure, distinct from
  optional absence. Specify filtered-view absence consistently with plan 3.

Use schema-defined default eager loading when ordinary attribute reads should always
be ready. Loading employee and customer references can use normal `select_related`;
there is no special child loader or child alias.

Perform no query when loaded data is valid. Preserve existing FK-change/cache
invalidation behavior. Instance updates or refreshes that change an FK invalidate
its reference. Refreshing a parent must define whether reverse one-to-one caches
are invalidated; default to invalidating them to avoid stale cached absence.

Coalesce concurrent explicit loads of the same reference on the same instance.
Do not cache failed/cancelled loads permanently. No global identity map is required;
the identity guarantee concerns each loaded reference on its owning instance.

## Tickets and acceptance

1. Add a shared reference-load operation using existing relation metadata.
2. Generate Python and TypeScript loader methods/types in the selected extension.
3. Implement cache, reload, invalidation, and concurrent-load behavior.
4. Verify transaction/connection routing and reference filters.

Check loaded object identity, cached absence, missing optional targets, FK changes,
forced reload, query counts, concurrent calls, and loading inside transactions.
Schema-level explicit selection can remain projection-only; loaders operate on
model instances. This ticket does not implement implicit async attribute reads.
