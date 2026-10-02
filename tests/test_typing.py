"""Runs mypy and pyright over tests/typing/check_models.py.

Every line marked `# E:` must produce an error and no other line may.
"""

import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
CHECK = ROOT / "tests" / "typing" / "check_models.py"
EXPECTED = {i for i, line in enumerate(CHECK.read_text().splitlines(), 1) if "# E:" in line}


def error_lines(output: str, pattern: str) -> set[int]:
    return {int(m.group(1)) for m in re.finditer(pattern, output, re.M)}


def test_mypy():
    pytest.importorskip("mypy")
    out = subprocess.run(
        [sys.executable, "-m", "mypy", "--strict", "--no-incremental", str(CHECK)],
        capture_output=True,
        text=True,
        env={"MYPYPATH": f"{ROOT / 'python'}:{ROOT / 'examples'}", "PATH": ""},
        cwd=ROOT,
    ).stdout
    assert error_lines(out, r"check_models\.py:(\d+): error") == EXPECTED, out


def test_pyright():
    exe = shutil.which("pyright")
    if exe is None:
        pytest.skip("pyright not installed")
    out = subprocess.run([exe, str(CHECK)], capture_output=True, text=True, cwd=CHECK.parent).stdout
    assert error_lines(out, r"check_models\.py:(\d+):\d+ - error") == EXPECTED, out
