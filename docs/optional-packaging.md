# Static native profiles

The `orm` Python wheel and npm package contain language runtime code only. They do
not install drivers, TLS, CLI or generators. A native profile supplies one engine
for the entire process. Profile selection is fixed during initialization; existing
schemas, objects, pools and transactions cannot switch engines.

| Profile | Databases | CLI | Generators | Feature adapters |
| --- | --- | --- | --- | --- |
| postgres | PostgreSQL | no | no | none |
| sqlite | SQLite | no | no | none |
| combined | PostgreSQL + SQLite | no | no | none |
| tooling | PostgreSQL + SQLite | yes | Python + TypeScript | none |

Python installs use `orm[postgres]`, `orm[sqlite]`, `orm[combined]` or `orm[tooling]`.
These extras select complete distributions, not changes to an installed binary.
Each native wheel owns a unique `orm_native_<profile>._native` namespace.
Installing `orm[postgres,sqlite]` installs two engines, not a combined engine. Use
`orm[combined]` for both databases. With exactly one installed profile, initialization
selects it automatically. With multiple profiles, set `ORM_PROFILE` to one profile.
An unknown/uninstalled selector, ambiguous install, incompatible ABI/version,
unexpected backend or adapter set fails before queries. Changing the environment
after initialization does not change the selected engine.

Node installs use `orm` alongside `@orm/native-postgres`, `@orm/native-sqlite`,
`@orm/native-combined` or `@orm/native-tooling`. Optional peers do not install every
profile. Each selected profile's optional dependencies are **platform artifacts**,
not additional database features. Supported prebuilt targets are darwin-arm64,
linux-x64 (glibc) and win32-x64. Unsupported targets require a source build. The
release workflow builds CPython 3.11–3.14 wheels and N-API 8 Node artifacts. Artifacts
are retained by CI; publishing requires the coordinator's release process.

Commands below use Nushell. Once distributions are published:

```nu
python -m pip install 'orm[sqlite]'
$env.ORM_PROFILE = "sqlite"
npm install orm @orm/native-sqlite
```

`python -m orm`, `npx orm`, migration/schema APIs and standalone `orm` remain
available. The language CLI entry points require the tooling profile; runtime
migration/schema APIs remain in runtime profiles for existing applications. Python
exposes its generator only when selected, and Node exposes its generator only when
selected. The tooling profile contains both generator implementations so either
language CLI can generate both languages.

```nu
python -m pip install 'orm[tooling]'
$env.ORM_PROFILE = "tooling"
python -m orm generate
npm install orm @orm/native-tooling
npx orm generate typescript
```

## Exact source builds

Cargo features are additive. Reproducible minimal builds always pass
`--no-default-features`; source-development defaults preserve both backends and
existing tooling. Engine backend features are `postgres` and `sqlite`. Bindings
forward selected backends without enabling dependency defaults. `cli` selects the
CLI crate. `generate-python` and `generate-typescript` select generator modules
independently, including through an optional CLI dependency. `composition` selects
the build-time static extension host; extension contract/compiler logic belongs to
the build-time extension system. It is distinct from model composition.

Baseline native profiles do not claim optional query-defaults, reference-loading,
proxy-models, model-composition, file-storage or generic-relations support. Their
feature owners supply the real code and exclusion gates. Custom source artifacts
may select verified capabilities and extensions. A marker package cannot add code
to an already compiled engine. Refer to [build-time extensions](build-time-extensions.md)
for extension manifests and build environment requirements.

Build a custom Python artifact using the `custom` template. Record the exact Cargo
features, Cargo.lock revision, Rust toolchain, target triple, Python version and
extension manifest/compiler configuration with the artifact. This template has no
backend by default; at least one backend must be explicitly selected. Additional
features on the command line are additive to the template's extension-module flag.

```nu
cd packaging/python/custom
maturin build --features postgres,generate-python --out ../../../target/custom-wheels
cd ../../..
python -m pip install .
python -m pip install --no-index --find-links target/custom-wheels orm-native-custom
$env.ORM_PROFILE = "custom"
```

The custom loader validates ABI/version/language, unique supported backend names,
boolean capability metadata and adapter/capability consistency. It does not
pretend to know a custom artifact's exact feature set; application build records
must specify it. Install one custom wheel per environment. For Node, an explicit
source-built path is selected once with `ORM_NATIVE`:

```nu
cd js
node scripts/build-native.mjs --features postgres,generate-typescript
$env.ORM_NATIVE = ($env.PWD | path join "orm.node")
```

For named Node release profiles, `node packaging/build-node-profile.mjs sqlite
--release` stages a platform distribution under `target/node-profiles/`. Python
profile pyprojects under `packaging/python/<profile>/` select only their named Cargo
profile with defaults disabled. Never enable multiple named profile features in one
build; exact metadata validation rejects broadened named artifacts.

## Immutable adapter contract

Native `profile_metadata()` / `profileMetadata()` returns JSON ABI 1 with `version`,
`language`, `profile`, `backends`, boolean `capabilities`, and selected `adapters`.
This metadata is separate from schema/extension manifests. An adapter name must
have a true capability with the same canonical name. Python
`orm._capabilities.ADAPTERS` is a frozen set initialized at module import;
TypeScript `nativeAdapters()` returns a cached frozen array. Selected adapter modules
and specialized model methods are bound at module/definition initialization.
Disabled paths retain ordinary decoding/writes; they do not import providers,
copy rows, scan fields or test flags on every operation. Profile builds must include
exactly their selected adapters and dependencies. No query-time plugin discovery.

## Verification

`packaging/check-dependencies.py` audits isolated graphs for every binding/profile,
including backend SQL-builder unification and absence of CLI/TLS/unused drivers.
`packaging/check-wheel.py` checks thin/native wheel payload separation.
`packaging/check-node-install.mjs` packs and installs one native profile offline,
checks the dependency lock and runs installed-package smoke tests. Python
`packaging/smoke-python.py` runs against fresh installed wheels. Set
`ORM_TEST_DATABASE_URL` to an isolated PostgreSQL database for PostgreSQL checks;
SQLite checks use in-memory databases. CI Linux jobs provide an isolated service.
The release workflow retains wheels/tarballs without publishing them.
