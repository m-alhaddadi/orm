# Disabled native artifact size: cause (Q14)

All numbers are measured on 2026-10-08 (Apple M5, macOS, rustc 1.97.0).
Each artifact is a default-feature release build (fat LTO, one codegen unit) of `orm-python` and `orm-node`, from `git archive` of the named commit, with every source file touched before the build.
`size.py` attributes `__text` symbols to the first orm crate that the mangled symbol names.

## File sizes (bytes)

| Build | Python `_native.so` | Node `orm.node` |
|---|---:|---:|
| `3a710ca` (before branch 01) | 10,493,424 | 9,122,224 |
| `360685a` (branch 01) | 11,067,808 (+5.5% against `3a710ca`) | 9,616,080 (+5.4% against `3a710ca`) |
| `d244e5d` (main before the merges) | 10,532,624 | 9,159,712 |
| `136dc1d` without `reference-loading` | 11,454,432 | 9,985,152 |
| `136dc1d` (main) | 11,517,088 (+9.3% against `d244e5d`) | 10,039,152 (+9.6% against `d244e5d`) |

The branch 01 rebuilds have the same byte counts as `results/final-structural.json`.

## Branch 01, Python (`3a710ca` to `360685a`, +574,384 bytes)

Sections: `__text` +385,496, `__LINKEDIT` (symbol and string tables) +114,688, `__eh_frame` +31,436, `__gcc_except_tab` +18,504, other sections +24,260.

| Module | Before | After | Change |
|---|---:|---:|---:|
| all `__text` symbols | 6,563,252 | 6,948,748 | +385,496 |
| `ir` (schema and query IR decoders, moved to `orm_contracts`) | 768,660 | 924,328 | +155,668 |
| `orm_contracts::extension` (extension contract types and their checks) | 0 | 153,664 | +153,664 |
| `orm_contracts` (other) | 0 | 33,772 | +33,772 |
| `orm_core::dsl` | 110,344 | 126,020 | +15,676 |
| `orm_core` (other) | 105,388 | 111,816 | +6,428 |
| `orm_cli` | 203,420 | 208,200 | +4,780 |
| `orm_engine` (all modules) | 691,812 | 691,864 | +52 |
| `__native` (Python binding, all modules) | 909,280 | 909,116 | −164 |

Node shows the same pattern: `__text` +362,156, of which `ir` +148,660 and `orm_contracts::extension` +140,540.

## Main (`d244e5d` to `136dc1d`, Python +984,464 bytes)

`__text` +674,968: `orm_contracts::extension` +246,668, `ir` +213,980, `orm_contracts` other +42,952, `orm_contracts::identity` +30,764, `orm_contracts::generic` +28,040, `orm_core` other +26,556, `orm_core::dsl` +20,472, `orm_core::identity` +19,236.
`reference-loading` (on by default) is +62,656 bytes of the Python file and +54,000 of the Node file.

## Cause

Almost all growth is serde decoder code, not extension execution code.
The disabled build decodes the schema `behavior` block (declarations, storage, owner links, specializations, proxy models, query defaults, generic relations) so that it can reject or check a schema that needs an extension, as decision 29 permits.
These are the `orm_contracts::extension` and `orm_contracts::generic` rows.
The IR decoders (`ir`) also grew: they moved from `orm_core` to `orm_contracts`, and the IR has more fields.
No extension crate is linked into the disabled build (no `orm_proxy`, `orm_query_defaults`, `orm_generic`, `orm_model_composition` or `orm_file_storage` symbol; the all-features build has 790).
The engine did not grow in branch 01 (+52 bytes) and grew 7,928 bytes from `d244e5d` to `136dc1d`.
The other file growth (unwind tables, symbol tables) follows the code.

## Not done

A smaller disabled build needs decoders that only detect and reject extension data, for example `IgnoredAny` fields behind a feature of `orm_contracts`.
That changes the shared contract types that every extension crate compiles against.
Estimated gain: at most the `orm_contracts::extension` row, about 154 KB of `__text` (1.4% of the Python file).
