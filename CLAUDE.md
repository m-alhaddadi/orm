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
cargo test -q && cargo clippy -q -p orm-native -p orm-core
mypy --strict python/orm && pyright python/orm
```

Regenerate the example models after changing code generation:
`cd examples/blog && python -m orm generate`.
