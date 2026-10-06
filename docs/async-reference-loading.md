# Explicit async reference loading

The `reference-loading` Cargo feature adds explicit reference loaders. The Python
and Node bindings, the `orm` CLI and the tooling profile enable it. The runtime
profiles `postgres`, `sqlite` and `combined` are baseline artifacts. A baseline artifact keeps
ordinary eager and prefetch relation support, but it does not have this adapter or
the generated loader methods. Generated modules record the capability that they
require. An artifact without that capability rejects them at definition.

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

Synchronous access does no I/O. It raises `NotLoaded` until the reference is
loaded. TypeScript loaders return the target type. Relation properties keep the
existing query-loaded typing rules, with one exception: a target with a prepared
default filter makes the relation property nullable too. A generated loader name
must not be the same as a field, a relation, a runtime member, or an extension
method. The schema fails with an error when two names are the same.

The loader uses ordinary target queries and the database of the owner instance.
When the current task has a transaction, the loader uses that transaction. The
reference cache belongs to the owner instance. There is no global identity map.
A valid cached object or a cached optional absence needs no query. An absent
reverse one-to-one target and an absent optional target return `None` in Python
and `null` in TypeScript. A required target that is not in the database raises
`IntegrityError`. A target with a prepared default filter can be absent because
the filter hides it. This applies to required references too, so the generated
return types for these targets are nullable.

Calls for the same reference, key, database, and transaction share one query that
is in progress. Concurrent forced reloads also share it. After a query fails, a
new call starts a new query. In Python, when you cancel one waiter, the shared
query and the other waiters continue. The shared operation completes normally, and
its exception is always observed. Node uses ordinary promises and has no separate
cancellation API.

When an instance update or refresh changes a source key, the reference becomes
invalid. A refresh also makes reverse one-to-one caches invalid, even when the
source key does not change. Thus a refresh after you insert a target replaces a
cached absence. A query that started before such a change can still return its
original result to its callers, but it does not write that result to the cache.
A forced reload replaces the cached reference and does not change the scalar
fields of the owner row. Other instances stay as they are until you refresh or
reload them.

An adapter reads hidden helper keys: it looks first at the public attribute, then
at the hidden values. Ordinary attribute access does not show the hidden values.
Adapters for generic references can use the internal function that shares
concurrent loads. That function does not route generic references.
