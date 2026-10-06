# Working on this repo

## Git

- Commit directly on `main` and push to `origin main`. No feature branches or pull
  requests unless asked for one.
- Before committing, run the checks below and keep them green.

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

Regenerate the example models after changing code generation:
`cd examples/blog && python -m orm generate`, and for TypeScript
`cargo run -q -p orm-cli -- --schema examples/blog/schema.prisma generate typescript`
and the same with `-o js/test/blog/models.ts --import ../../src/index.js`
(a test checks both are current).
