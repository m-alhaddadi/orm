# Explicit async reference loading

Build both the native binding and model generator with the `reference-loading`
Cargo feature. Baseline artifacts retain ordinary eager/prefetch relation support,
but exclude this adapter and its generated instance methods. Generated modules
record their required capability and reject incompatible artifacts at definition.

For a belongs-to or reverse one-to-one relation:

```python
person = await customer.load_person()
assert customer.person is person
person = await customer.load_person(reload=True)
```

```typescript
const person = await customer.loadPerson();
const fresh = await customer.loadPerson({ reload: true });
```

Synchronous access performs no I/O and raises `NotLoaded` until ready. TypeScript
loaders return the target type; relation properties retain the existing
query-loaded typing rules. Generated loader names must not collide with fields,
relations, runtime members, or extension methods. Collisions reject the schema.

The loader uses ordinary target queries and the owner's database binding, including
its current task transaction. An explicit reference cache belongs to that owner;
there is no global identity map. Valid cached objects and cached optional absence
need no query. Reverse one-to-one absence and optional missing targets return
`None`/`null`. A physically missing required target raises `IntegrityError`.
Targets with a prepared default filter may return filtered-view absence, including
required references; their generated return types are nullable.

Calls for the same reference, key and database/transaction context share an
inflight query. Concurrent forced reloads share it too. A failed query can be
retried. Cancelling a Python waiter does not cancel the shared query or other
waiters; the shared operation completes normally, and exceptions remain observed.
Node uses ordinary promises and has no separate cancellation API.

Changing a source key through instance update/refresh invalidates its reference.
Refresh also invalidates reverse one-to-one caches, even if the source key is
unchanged, so cached absence can be refreshed after inserting a target. An older
inflight query may still return its original result to its callers, but cannot
repopulate a cache invalidated by a mutation. A forced reload refreshes the cached
reference without changing the owning row's scalar fields. Other instances remain
snapshots until explicitly refreshed/reloaded.

Selected adapters resolve hidden helper keys with public-first lookup while
keeping them hidden from ordinary attribute access. Internal coalescing primitives
are reusable by generic-reference adapters; they do not implement generic routing.
