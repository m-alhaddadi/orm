"""Definition publishes a prepared snapshot, or leaves the registry intact."""
import pytest
import orm
from orm.fields import Integer

GOOD = '''model User {
  id Int @id
  name String
}'''


def test_definition_is_eager_and_failure_is_atomic():
    registry = orm.Registry()
    User = orm.loads(GOOD, registry=registry)["User"]
    prepared = registry._native
    assert prepared is not None
    before = registry.ir()
    with pytest.raises(orm.SchemaError, match="unknown model"):
        orm.define({"models": [{"name": "Bad", "table": "bad", "fields": [
            {"name": "id", "column": "id", "type": "int", "primary_key": True},
        ], "relations": [{"name": "missing", "kind": "one", "target": "Missing", "from": "id", "to": "id"}]}]}, registry=registry)
    assert registry.ir() == before
    assert registry.native() is prepared
    assert registry.get("User") is User


def test_existing_models_keep_their_prepared_snapshot():
    registry = orm.Registry()
    User = orm.loads(GOOD, registry=registry)["User"]
    snapshot = User._meta.registry.native()
    orm.loads('model Other {\n id Int @id\n}', registry=registry)
    assert User._meta.registry.native() is snapshot
    assert registry.native() is not snapshot


def test_class_declarations_require_preparation_and_keep_previous_snapshot():
    registry = orm.Registry()

    class User(orm.Model, registry=registry):
        id = Integer(primary_key=True)

    with pytest.raises(TypeError, match="prepare"):
        User.objects.sql()
    registry.prepare()
    old_query = User.objects.filter(User.id == 7)
    old_native = User._meta.registry.native()

    class Other(orm.Model, registry=registry):
        id = Integer(primary_key=True)

    assert User._meta.registry.native() is old_native
    assert '"User"' not in old_query.sql()  # Uses the existing lowercase table.
    with pytest.raises(TypeError, match="prepare"):
        Other.objects.sql()
    registry.prepare()
    assert Other._meta.registry.native() is not old_native
    assert User._meta.registry.native() is old_native


def test_definition_owns_input_schema():
    registry = orm.Registry()
    ir = {"models": [{"name": "User", "table": "users", "fields": [
        {"name": "id", "column": "id", "type": "int", "primary_key": True},
    ]}]}
    User = orm.define(ir, registry=registry)["User"]
    ir["models"][0]["table"] = "mutated"
    assert User._meta.registry.ir()["models"][0]["table"] == "users"
