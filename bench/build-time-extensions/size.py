"""Attribute the machine-code growth between two native artifacts to orm modules.

usage: size.py BEFORE AFTER [ROWS] [DEPTH]

Mach-O only (macOS `nm`). Each `__text` symbol is sized by the distance to the next
symbol and charged to the first orm crate it names, so monomorphized serde code
counts against the orm type it decodes. `orm_core::ir` and `orm_contracts::ir` are
one key, because the IR moved between the two crates.
"""
import collections
import re
import subprocess
import sys

CRATE = re.compile(r'C(?:s[0-9A-Za-z]*_)?\d+(orm_contracts|orm_core|orm_engine|orm_cli|__native|orm_node)')


def identifiers(symbol, position, count):
    out = []
    while len(out) < count:
        m = re.compile(r'[A-Za-z]*?(\d+)').match(symbol, position)
        if not m:
            break
        length = int(m.group(1))
        out.append(symbol[m.end():m.end() + length])
        position = m.end() + length
    return [i for i in out if i]


def attribute(path, depth):
    listing = subprocess.check_output(['nm', '-n', '-m', '--defined-only', path], text=True, errors='replace')
    rows = [(int(line[:16], 16), line.split()[-1]) for line in listing.splitlines() if '(__TEXT,__text)' in line]
    sizes = collections.Counter()
    for (start, symbol), (end, _) in zip(rows, rows[1:]):
        owner = CRATE.search(symbol)
        if owner:
            crate, modules = owner.group(1), identifiers(symbol, owner.end(), depth)
            if crate in ('orm_contracts', 'orm_core') and modules[:1] == ['ir']:
                crate = 'ir'
                modules = modules[1:]
            key = '::'.join([crate, *modules])
        else:
            m = re.search(r'C(?:s[0-9A-Za-z]*_)?(\d+)', symbol)
            key = 'other:' + (symbol[m.end():m.end() + int(m.group(1))] if m else '?')
        sizes[key] += end - start
    return sizes


before_path, after_path = sys.argv[1], sys.argv[2]
rows = int(sys.argv[3]) if len(sys.argv) > 3 else 20
depth = int(sys.argv[4]) if len(sys.argv) > 4 else 1
before, after = attribute(before_path, depth), attribute(after_path, depth)
print(f'| Module | Before | After | Change |\n|---|---:|---:|---:|')
print(f'| all `__text` symbols | {sum(before.values()):,} | {sum(after.values()):,} | {sum(after.values()) - sum(before.values()):+,} |')
for key in sorted(set(before) | set(after), key=lambda k: -abs(after[k] - before[k]))[:rows]:
    print(f'| `{key}` | {before[key]:,} | {after[key]:,} | {after[key] - before[key]:+,} |')
