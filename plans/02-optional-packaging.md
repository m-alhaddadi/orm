# Optional dependencies and distribution

## Outcome

Users choose their language binding, databases, and model features. An installation
does not bring unrelated drivers, TLS stacks, generators, CLI code, or extensions.
Feature availability is fixed in the built artifact, not toggled per operation.

Currently `engine/Cargo.toml` includes PostgreSQL and SQLite dependencies together.
Both bindings depend on the CLI and explicitly enable PostgreSQL SQL-builder
features. Python and Node already have separate binding crates; preserve that seam.

## Rust build graph

Introduce additive Cargo features for PostgreSQL, SQLite, CLI/migrations, language
generators, proxies, composition, and explicit relation loading where separable.
Exact names should follow the final dependency graph rather than force an arbitrary
flag for every public method.

- Make driver and TLS dependencies optional and gate their modules and exports.
- Forward only selected SQL-builder backend/type features through every dependent
  crate. Audit feature unification: one binding must not re-enable an excluded backend.
- Remove unconditional CLI coupling from runtime bindings. Keep schema/runtime
  facilities needed by current APIs, and select tooling separately.
- Keep Python and Node generation dependencies independently selectable.
- Have build-time extension manifests select their required host capabilities.

## Python distribution

Use `[project.optional-dependencies]` for installation choices, following the
familiar `package[postgres,proxy]` form. Package names here are illustrative.

Extras select distributions; they do not recompile or shrink the wheel containing
the main package. Adding `proxy` to an extra cannot inject proxy Rust code into an
already installed native module.

Ship a thin language package and named native profile packages, each with a unique
import namespace. A profile describes an exact statically compiled set of databases
and native features. Convenience extras choose documented profiles/adapters, and
the loader selects one compatible native engine once during initialization.

Avoid assuming separately installed driver packages can link themselves into a
monolithic engine. For this initial static design, every native profile contains
its chosen driver code. Do not run different engines for different parts of a
transaction or mix instances across profiles.

Publish a bounded set of useful prebuilt profiles. Provide an explicit source-build
configuration for exact combinations and third-party extensions. Validate extras
combinations against available profiles; adding individually valid extras does not
necessarily produce a valid combined native artifact. Define conflict diagnostics
and a documented profile selector before release.

## npm distribution

Use separate feature/profile packages with declared peer/dependency requirements.
Node users install the desired profile alongside the language package. npm has no
Python-style bracket extras.

`optionalDependencies` is useful for native platform artifacts and dependencies
whose installation may fail without failing the main package. It is not a named
feature selector: optional dependencies are normally attempted during installation,
so listing every backend there would not meet the minimal-install requirement.

Generate the binding adapter set when building a profile. Package initialization
may select its native platform artifact once; queries do not discover plugins or
branch on install flags.

## Tickets and acceptance

1. Audit and split the Cargo feature/dependency graph, including both bindings.
2. Split runtime and tooling packages without removing existing documented tooling.
3. Define native profiles, compatibility metadata, and deterministic loader rules.
4. Add Python extras and npm package metadata for the available profiles.
5. Add release jobs and installation tests for selected profiles.

Verify PostgreSQL-only and SQLite-only builds, both language bindings, minimal
runtime installs, and one combined profile. Inspect dependency trees and artifact
contents to prove unused backends are absent. Unsupported selections fail before
queries execute. Record build configuration for reproducible custom artifacts.

Sources: [Cargo features](https://doc.rust-lang.org/cargo/reference/features.html),
[Python optional dependencies](https://packaging.python.org/en/latest/specifications/pyproject-toml/#dependencies-optional-dependencies),
[npm package metadata](https://docs.npmjs.com/cli/configuring-npm/package-json/#optionaldependencies).
