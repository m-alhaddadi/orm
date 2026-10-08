# Working on this repo

## Git

- Commit directly on `main` and push to `origin main`. No feature branches or pull
  requests unless asked for one.
- Before committing, run the checks below and keep them green.

## Tests

- Always prefer bulk database actions in tests and fixtures to reduce database
  round trips: batch inserts with `insert_many()` / `insertMany()`, and use bulk
  updates or deletes where applicable. Use individual operations when their
  behavior is what the test verifies or later operations depend on earlier results.

## Checks

```bash
service postgresql start          # the test database (postgres:postgres@localhost/orm_test)
. .venv/bin/activate && uv pip install -e .   # the thin `orm` package (python/)
(cd packaging/python/tooling && maturin develop)   # the tooling native profile
python -m pytest -q               # Python end-to-end, SQL shape and typing tests
cargo test -q && cargo clippy -q -p orm-core -p orm-engine -p orm-python -p orm-node -p orm-contracts -p orm-extension-build
mypy --strict python/orm && pyright python/orm
cd js && npm install && npm run build:native   # the TypeScript package (js/), Node addon
npm test && npm run test:bun && npm run typecheck
```

## Feature artifacts

The checks above build no optional feature. `scripts/feature-check.sh` builds one
artifact per feature extension (proxy-models, query-defaults, model-composition,
file-storage, generic-relations) and one with all of them through `orm-extension-build`;
the extension manifests select the Cargo features, never a hand-set `--features`.
On each artifact it runs pytest, `npm test`, bun and `cargo test`, the query-defaults
public scripts, and once the `storage/` packages and their end-to-end scripts.
CI runs it (`.github/workflows/feature-check.yml`). Run it before you commit a change
to an extension crate, `contracts/`, `extension-build/` or feature-gated host code:

```bash
PYTHON=python3.14 ORM_TEST_DATABASE_URL=postgres://postgres:postgres@localhost/orm_test \
  scripts/feature-check.sh            # or name artifacts: scripts/feature-check.sh proxy-models all
```

It needs `uv`, `node`, `npm` and `bun`, and writes `target/feature-check/matrix.txt` and `logs/`.
It does not touch the default `.venv` profile or `js/orm.node`.

Regenerate the example models after changing code generation:
`cd examples/blog && python -m orm generate`, and for TypeScript
`cargo run -q -p orm-cli -- --schema examples/blog/schema.prisma generate typescript`
and the same with `-o js/test/blog/models.ts --import ../../src/index.js`
(a test checks both are current).
