"""Run against a package containing the example-native artifact."""
import asyncio
import json
import os
from pathlib import Path
import orm
from orm.fields import Integer, String

SCHEMA = json.loads(Path(os.environ["ORM_EXTENSION_TEST_SCHEMA"]).read_text())

async def check():
    reg = orm.Registry()
    models = orm.define(SCHEMA, registry=reg)
    User = models["NativeUser"]
    Record = models["NativeRecord"]
    Lowered = models["LoweredUser"]
    Nullable = models["NativeNullable"]
    assert User.validate_username("valid") is None
    try:
        User.validate_username("bad!")
    except orm.QueryError:
        pass
    else:
        raise AssertionError("compiled model method did not call the validator")
    from generated import NativeUser as GeneratedUser
    assert GeneratedUser._meta.schema_ir == User._meta.schema_ir
    assert GeneratedUser.validate_username("valid") is None
    assert "display" in User._meta.fields
    assert "display" not in User._meta.input_fields
    snapshot = json.loads(reg.native().snapshot())
    table = next(t for t in snapshot["tables"] if t["name"] == "native_users")
    assert [c["name"] for c in table["columns"]] == ["id", "username"]
    db = await orm.connect(os.environ.get("ORM_EXTENSION_TEST_URL", "sqlite://:memory:"), registry=reg)
    await db.create_tables()
    await db.execute('DELETE FROM "native_users"')
    try:
        nullable = await Nullable.objects.insert(username=None)
        assert nullable.username is None and nullable.display is None
        await nullable.delete()
        await db.execute("INSERT INTO native_users (username) VALUES ('unrequested')")
        only_stored = await User.objects.filter(User.username == "unrequested").select(User.username)
        assert tuple(only_stored[0]) == ("unrequested",)
        try:
            await User.objects.filter(User.username == "unrequested").get()
        except orm.DatabaseError as e:
            assert "must not be computed" in str(e)
        else:
            raise AssertionError("requested computation was not executed")
        await User.objects.filter(User.username == "unrequested").delete()
        from generated import LoweredUser as GeneratedLowered
        lowered = await Lowered.objects.insert(id=7, public_name="Alice")
        assert lowered.public_name == "Alice" and not hasattr(lowered, "secret")
        physical = json.loads(json.dumps(SCHEMA["models"][2]))
        handwritten = json.loads(json.dumps(physical))
        handwritten["fields"][1]["name"] = "public_name"
        handwritten["fields"].pop()
        direct_registry = orm.Registry()
        Direct = orm.define({"dialect":SCHEMA["dialect"], "models":[handwritten],"behavior":{"schema_contract":1,"storage":{"models":[physical]}}}, registry=direct_registry)["LoweredUser"]
        assert Lowered.objects.sql() == Direct.objects.sql()
        assert next(t for t in json.loads(reg.native().snapshot())["tables"] if t["name"] == "lowered_users") == json.loads(direct_registry.native().snapshot())["tables"][0]
        direct = await Direct.objects.using(db).filter(Direct.id == 7).get()
        generated = await GeneratedLowered.objects.using(db).filter(GeneratedLowered.id == 7).get()
        assert direct.public_name == generated.public_name == lowered.public_name
        await lowered.delete()
        record = await Record.objects.insert(left="first", right="second")
        try:
            await record.update(left="changed")
        except orm.QueryError as e:
            assert "all non-null supplied dependencies" in str(e)
        else:
            raise AssertionError("partial record update bypassed dependency validation")
        await record.refresh()
        assert record.left == "first" and record.right == "second"
        await record.delete()
        row = await User.objects.insert(username="  alice  ")
        assert row.username == "alice" and row.display == "Hello, alice!"
        collisions = orm.Registry(dialect=SCHEMA.get("dialect", "postgres"))
        class NativeUser(orm.Model, registry=collisions, table="native_users"):
            id = Integer(primary_key=True, auto_increment=True)
            username = String()
            @staticmethod
            def validate_username(value):
                return "application method"
        before_fields = NativeUser._meta.fields
        try:
            collisions.prepare()
        except TypeError as e:
            assert "collides" in str(e)
        else:
            raise AssertionError("native method overwrote a class member")
        assert NativeUser.validate_username("valid") == "application method"
        assert NativeUser._meta.fields is before_fields and "display" not in before_fields
        classes = orm.Registry(dialect=SCHEMA.get("dialect", "postgres"))
        class NativeUser(orm.Model, registry=classes, table="native_users"):
            id = Integer(primary_key=True, auto_increment=True)
            username = String(default=lambda: "  default_user  ")
        classes.prepare()
        class_db = await orm.connect(os.environ.get("ORM_EXTENSION_TEST_URL", "sqlite://:memory:"), registry=classes, default=False)
        try:
            await class_db.create_tables()
            default = await NativeUser.objects.using(class_db).insert()
            assert default.username == "default_user" and default.display == "Hello, default_user!"
            await default.delete()
        finally:
            await class_db.close()
        generated_row = await GeneratedUser.objects.using(db).filter(GeneratedUser.id == row.id).get()
        assert generated_row.display == row.display
        assert "NULL" in User.objects.sql()
        projection = await User.objects.select(User.display)
        assert len(projection) == 1 and tuple(projection[0]) == ("Hello, alice!",)
        await row.update(username="  bob  ")
        assert row.username == "bob" and row.display == "Hello, bob!"
        try:
            await User.objects.insert_many([{"username": "good"}, {"username": "bad!"}])
        except orm.QueryError as e:
            assert "username" in str(e)
        else:
            raise AssertionError("bulk validator was bypassed")
        assert await User.objects.count() == 1
        try:
            await User.objects.insert(username=" admin ")
        except orm.QueryError as e:
            assert "reserved record" in str(e)
        else:
            raise AssertionError("record validator did not see the transformed value")
        try:
            await User.objects.update(username=None)
        except orm.QueryError as e:
            assert "non-null" in str(e)
        else:
            raise AssertionError("null bypassed validation")
        try:
            await User.objects.filter(User.id == row.id).update(username=User.username + "x")
        except orm.QueryError as e:
            assert "expressions" in str(e)
        else:
            raise AssertionError("expression bypassed native validation")
        try:
            User.objects.filter(User.display == "Hello, bob!").sql()
        except orm.QueryError as e:
            assert "computed" in str(e)
        else:
            raise AssertionError("native computation incorrectly offered SQL filtering")
        rows = await User.objects.insert_many([{"username": "carol"}, {"username": "dave"}])
        assert [r.display for r in rows] == ["Hello, carol!", "Hello, dave!"]
        await User.objects.update_many([{"id": rows[0].id, "username": " eve "}, {"id": rows[1].id, "username": " frank "}])
        loaded = await User.objects.filter(User.id >= rows[0].id).order_by(User.id)
        assert [r.display for r in loaded] == ["Hello, eve!", "Hello, frank!"]
        await db.execute('UPDATE "native_users" SET username = \'raw!\' WHERE id = ' + str(row.id))
        raw = await User.objects.filter(User.id == row.id).get()
        assert raw.username == "raw!" and raw.display == "Hello, raw!!"
    finally:
        await db.drop_tables()
        await db.close()

async def check_ownership():
    schema = json.loads(Path(__file__).with_name("ownership-schema.json").read_text())
    schema["dialect"] = SCHEMA["dialect"]
    physical = json.loads(json.dumps(schema["models"]))
    physical[1]["fields"].pop()
    schema["behavior"]["storage"] = {"models": physical}
    registry = orm.Registry()
    models = orm.define(schema, registry=registry)
    Child = models["OwnerChild"]
    db = await orm.connect(os.environ["ORM_EXTENSION_TEST_URL"], registry=registry, default=False)
    try:
        await db.drop_tables()
        await db.create_tables()
        await db.execute('INSERT INTO extension_owner_parents (id, name) VALUES (7, \'Alice\')')
        await db.execute('INSERT INTO extension_owner_children (id, role) VALUES (7, \'employee\')')
        child = await Child.objects.using(db).get()
        assert child.name == "Alice" and child.role == "employee"
        assert await Child.objects.using(db).filter(Child.name == "Alice").count() == 1
        assert tuple((await Child.objects.using(db).select(Child.name))[0]) == ("Alice",)
        await child.refresh()
        assert child.name == "Alice"
        try:
            await child.update(role="customer")
        except orm.QueryError as e:
            assert "owner statement sequence" in str(e)
        else:
            raise AssertionError("ordinary update bypassed multi-owner planning")
    finally:
        await db.drop_tables()
        await db.close()

asyncio.run(check())
asyncio.run(check_ownership())
# Shape drift is rejected at definition, before any connection.
stale = json.loads(json.dumps(SCHEMA))
stale["models"][0]["fields"][1]["nullable"] = True
try:
    orm.define(stale, registry=orm.Registry())
except orm.SchemaError as e:
    assert "rebuild" in str(e)
else:
    raise AssertionError("stale specialization accepted")
print("Python native extension contract passed")
