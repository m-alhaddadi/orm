"""Render results-*.json (Python, Node, Bun, Go) as markdown tables (median latency per call)."""

import json
import sys
from pathlib import Path

ORDER = [
    "django-sync", "django-async", "django-async-dict", "sqla-asyncpg", "sqla-psycopg",
    "drizzle-pg", "gorm", "pgx",
    "ormcore-obj", "ormcore-dict", "ormcore-async", "ormcore-cgo", "ormcore-sync", "rust-only",
]
OPS = [
    ("read", "Read N posts"),
    ("read_join", "Read N posts + author (JOIN)"),
    ("write_bulk", "Bulk insert N posts (one statement)"),
    ("write_loop", "Insert N posts one at a time (N calls, N commits)"),
]
DEFAULT_BASELINE = "django-async"


def fmt_ms(us):
    ms = us / 1000
    return f"{ms:.2f}" if ms < 100 else f"{ms:.0f}"


def render(path):
    data = json.loads(Path(path).read_text())
    res = {(r["contender"], r["op"], r["n"]): r for r in data["results"]}
    sizes = sorted({r["n"] for r in data["results"]})
    env = data["env"]
    baseline = env.get("baseline", DEFAULT_BASELINE)
    seen = list(dict.fromkeys(r["contender"] for r in data["results"]))
    order = [c for c in ORDER if c in seen] + [c for c in seen if c not in ORDER]
    runtime = env.get("runtime") or f"python {env.get('python', '')}"
    out = [f"{runtime}, transport: **{env['transport']}** — median ms per call "
           f"(× = speed-up vs `{baseline}`)\n"]
    for op, title in OPS:
        out.append(f"#### {title}\n")
        out.append("| contender | " + " | ".join(f"N={n}" for n in sizes) + " |")
        out.append("|---|" + "---:|" * len(sizes))
        for c in order:
            cells = []
            for n in sizes:
                r = res.get((c, op, n))
                if not r:
                    cells.append("—")
                    continue
                base = res.get((baseline, op, n))
                x = f" ({base['median_us'] / r['median_us']:.1f}×)" if base and c != baseline else ""
                cells.append(fmt_ms(r["median_us"]) + x)
            if any(x != "—" for x in cells):
                out.append(f"| `{c}` | " + " | ".join(cells) + " |")
        out.append("")
    return "\n".join(out)


if __name__ == "__main__":
    for p in sys.argv[1:]:
        print(render(p))
