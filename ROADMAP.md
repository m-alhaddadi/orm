# Roadmap

Improvements we want but haven't planned yet. Phases and what's done are in
[`PLAN.md`](PLAN.md).

## Performance

### Shortcut joins through a shared key — not scheduled

A relation path `A → B → C` where B reaches C through the same key that A uses to
reach B, e.g. `Order.shop.config` with `ShopConfig.shop_id` both the key and a FK to
`Shop.id`:

```sql
-- today
LEFT JOIN shops j1 ON j1.id = orders.shop_id
LEFT JOIN shop_configs j2 ON j2.shop_id = j1.id
-- shortcut
LEFT JOIN shop_configs j2 ON j2.shop_id = orders.shop_id
```

* **Skip B** when nothing else of B is read (reading `B.<key>` resolves to
  `A.<fk>`) and either relation is a FK, so B's row is known to exist. This is the
  main win: one table fewer in joins (`select()`, `order_by`, `group_by`) and one
  `EXISTS` level fewer in filters.
* **Keep B but link C to A** when B is read: lets the planner join C before B, e.g.
  to apply a selective filter on C first. Needs the same FK condition. For filters,
  `EXISTS(B ... AND EXISTS(C ...))` can become `EXISTS(B ...) AND EXISTS(C ...)`
  without a FK, since B's key is unique.
* `select_related` of B always keeps the join.

Why not leave it to Postgres: for inner joins it derives `orders.shop_id =
shop_configs.shop_id` but still joins `shops`, since it doesn't trust FKs for join
removal; for LEFT JOINs it derives nothing. Where: `ensure_join` and `exists_via` in
`native/src/plan.rs` (marked TODO).
