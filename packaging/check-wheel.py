"""Check native namespace separation and the absence of native payload in orm."""
from pathlib import Path
import sys
from zipfile import ZipFile

wheels = Path(sys.argv[1])
profile = sys.argv[2]
thin = next(wheels.glob("orm-*.whl"))
with ZipFile(thin) as archive:
    assert not any(name.endswith((".so", ".pyd", ".dylib", ".dll")) for name in archive.namelist())
    assert "orm/_native.py" in archive.namelist()
    assert not any("orm_native_" in name for name in archive.namelist())
native = next(wheels.glob(f"orm_native_{profile}-*.whl"))
with ZipFile(native) as archive:
    payload = [name for name in archive.namelist() if name.endswith((".so", ".pyd"))]
    assert len(payload) == 1 and payload[0].startswith(f"orm_native_{profile}/_native.")
    assert not any(name.startswith("orm/") for name in archive.namelist())
print(f"wheel contents verified: {profile}")
