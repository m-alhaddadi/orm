"""Schema composition reaches migrations, runtime identity and typed generation."""
import importlib
import os
from pathlib import Path
import subprocess
import sys

import pytest

from orm import Registry
import orm
import orm.model


@pytest.mark.parametrize("first", ["main", "child"])
@pytest.mark.parametrize("external", [False, True])
async def test_generated_schema_modules(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, first: str, external: bool) -> None:
    package = tmp_path / "composed"
    billing = tmp_path / "library" if external else package / "billing"
    package.mkdir()
    billing.mkdir(parents=True)
    (package / "__init__.py").touch()
    (billing / "__init__.py").touch()
    root = package / "schema.prisma"
    root.write_text('''datasource db {
 provider = "sqlite"
}
import "billing/schema.prisma" (prefix: "billing_")
model User {
 id Int @id
 invoices Invoice[]
}
''')
    (billing / "schema.prisma").write_text('''model Invoice {
 id Int @id
 user_id Int
 user User @relation(fields: [user_id], references: [id])
}
''')
    if external:
        root.write_text(root.read_text().replace('billing/schema.prisma', '../library/schema.prisma'))
    loaded = orm.load(root, registry=Registry())
    assert loaded["Invoice"]._meta.ir()["table"] == "billing_invoice"
    repo = Path(__file__).resolve().parents[1]
    result = subprocess.run(
        ["cargo", "run", "-q", "-p", "orm-cli", "--", "--schema", str(root), "generate", "python"],
        cwd=repo, text=True, capture_output=True,
    )
    assert result.returncode == 0, result.stderr
    monkeypatch.syspath_prepend(str(tmp_path))
    reg = Registry()
    monkeypatch.setattr(orm.model, "registry", reg)
    child_name = "library.models" if external else "composed.billing.models"
    try:
        importlib.import_module("composed.models" if first == "main" else child_name)
        main = importlib.import_module("composed.models")
        child = importlib.import_module(child_name)
        assert main.User is reg.get("User")
        assert child.Invoice is reg.get("Invoice")
        assert not hasattr(main, "Invoice")
        assert not hasattr(child, "User")
        assert child.Invoice._meta.ir()["table"] == "billing_invoice"
        assert reg.ir()["dialect"] == "sqlite"
        reg.native()
        db = await orm.connect("sqlite://:memory:", registry=reg, default=False)
        try:
            await db.create_tables()
            await main.User.objects.using(db).insert(id=1)
            await child.Invoice.objects.using(db).insert(id=2, user_id=1)
            invoices = await child.Invoice.objects.using(db).load(child.Invoice.user)
            assert len(invoices) == 1 and invoices[0].user.id == 1
        finally:
            await db.close()
        stub_import = "composed._orm_models" if external else ".._orm_models"
        assert f"from {stub_import} import Invoice as Invoice" in (billing / "models.pyi").read_text()
        assert "from ._orm_models import User as User" in (package / "models.pyi").read_text()
        assert len(list(reg)) == 2
        if first == "main" and external:
            consumer = tmp_path / "check.py"
            consumer.write_text("from typing import assert_type\nfrom orm import ColumnRef\nfrom composed.models import User\nfrom library.models import Invoice\nassert_type(User.invoices.id, ColumnRef[int])\nassert_type(Invoice.user.id, ColumnRef[int])\n")
            checked = subprocess.run(
                [sys.executable, "-m", "mypy", "--strict", str(consumer)],
                cwd=tmp_path, text=True, capture_output=True,
                env={**os.environ, "MYPYPATH": f"{repo / 'python'}:{tmp_path}"},
            )
            assert checked.returncode == 0, checked.stdout + checked.stderr
            checked = subprocess.run(
                [str(Path(sys.executable).with_name("pyright")), "--pythonpath", sys.executable, str(consumer)],
                cwd=tmp_path, text=True, capture_output=True,
            )
            assert checked.returncode == 0, checked.stdout + checked.stderr
    finally:
        for name in list(sys.modules):
            if name in ("composed", "library") or name.startswith(("composed.", "library.")) or name == f"_orm_schema:{package / '_orm_models.py'}":
                del sys.modules[name]
