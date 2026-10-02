# Vendored patches

## `sqlx-core` 0.9.0 — `TCP_NODELAY`

Unmodified copy of `sqlx-core` 0.9.0 from crates.io, plus one change in
`src/net/socket/mod.rs` (marked `PATCH(orm)`): `set_nodelay(true)` on the Tokio
`TcpStream` after connecting.

Without the patch, sqlx leaves Nagle's algorithm on, so large multi-packet requests
stall on the server's delayed ACK. A 1000-row bulk INSERT over localhost TCP took
~55 ms instead of ~12 ms (see `bench/RESULTS.md`). libpq (psycopg) and asyncpg both
disable Nagle.

Drop this patch once sqlx ships the fix upstream, or when the engine moves off sqlx.
