"""A build without query-defaults rejects compiled policies and keeps the registry."""
import json

import orm
from orm import _native as module
assert "query-defaults" not in json.loads(module.native_artifact())["capabilities"]
registry = orm.Registry()
plain = {"models": [{"name": "Plain", "table": "plain", "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": True}]}]}
orm.define(plain, registry=registry)
before = registry.ir()
bad = {"models": [{"name": "Bad", "table": "bad", "fields": [{"name": "id", "column": "id", "type": "int", "primary_key": True}]}], "behavior": {"schema_contract": 1, "query_defaults": [{"model": "Bad", "fields": []}]}}
try: orm.define(bad, registry=registry)
except orm.SchemaError as error: assert "query-defaults" in str(error) and "rebuild" in str(error)
else: raise AssertionError("disabled artifact accepted policies")
assert registry.ir() == before
print("Disabled Python capability/atomic definition: passed")
