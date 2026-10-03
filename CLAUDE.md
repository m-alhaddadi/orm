# Working on this repo

## Git

- Commit directly on `main` and push to `origin main`. No feature branches or pull
  requests unless asked for one.
- Before committing, run the checks below and keep them green.

## Checks

```bash
service postgresql start          # the test database (postgres:postgres@localhost/orm_test)
. .venv/bin/activate && maturin develop
python -m pytest -q               # Python end-to-end, SQL shape and typing tests
cargo test -q && cargo clippy -q -p orm-core -p orm-engine -p orm-python -p orm-node
mypy --strict python/orm && pyright python/orm
cd js && npm install && npm run build:native   # the TypeScript package (js/), Node addon
npm test && npm run test:bun && npm run typecheck
```

Regenerate the example models after changing code generation:
`cd examples/blog && python -m orm generate`, and for TypeScript
`orm generate typescript examples/blog/schema.prisma -o examples/blog/models.ts --import orm`
and the same with `-o js/test/blog/models.ts --import ../../src/index.js`
(a test checks both are current).
