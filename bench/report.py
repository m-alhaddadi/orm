"""Render results-*.json as markdown tables (median latency per call)."""

import json
import sys
from pathlib import Path

ORDER = [
    "django-sync", "django-async", "django-async-dict", "sqla-asyncpg", "sqla-psycopg",
    "ormcore-obj", "ormcore-dict", "rust-only",
]
OPS = [
    ("read", "Read N posts"),
    ("read_join", "Read N posts + author (JOIN)"),
    ("write_bulk", "Bulk insert N posts (one statement)"),
    ("write_loop", "Insert N posts one at a time (N calls, N commits)"),
]
BASELINE = "django-async"


def fmt_ms(us):
    ms = us / 1000
    return f"{ms:.2f}" if ms < 100 else f"{ms:.0f}"


def render(path):
    data = json.loads(Path(path).read_text())
    res = {(r["contender"], r["op"], r["n"]): r for r in data["results"]}
    sizes = sorted({r["n"] for r in data["results"]})
    out = [f"Transport: **{data['env']['transport']}** — median ms per call "
           f"(× = speed-up vs `{BASELINE}`)\n"]
    for op, title in OPS:
        out.append(f"#### {title}\n")
        out.append("| contender | " + " | ".join(f"N={n}" for n in sizes) + " |")
        out.append("|---|" + "---:|" * len(sizes))
        for c in ORDER:
            cells = []
            for n in sizes:
                r = res.get((c, op, n))
                if not r:
                    cells.append("—")
                    continue
                base = res.get((BASELINE, op, n))
                x = f" ({base['median_us'] / r['median_us']:.1f}×)" if base and c != BASELINE else ""
                cells.append(fmt_ms(r["median_us"]) + x)
            if any(x != "—" for x in cells):
                out.append(f"| `{c}` | " + " | ".join(cells) + " |")
        out.append("")
    return "\n".join(out)


if __name__ == "__main__":
    for p in sys.argv[1:]:
        print(render(p))
