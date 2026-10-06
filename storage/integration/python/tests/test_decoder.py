import unittest
from orm_file_storage import FileField
from orm_file_storage.decoder import prepare_decoder
from orm_storage import Reference


class DecoderTest(unittest.TestCase):
    def test_public_slots_only_null_and_omission(self):
        fields = {"file": FileField("file", "reports", True), "hidden": FileField("hidden", "reports")}
        decode = prepare_decoder(fields, [("id", 2), ("file", 1)])
        destination = {"id": 42}
        decode(["invalid hidden helper", {"v": 1, "storage": "reports", "key": "x"}, 42], 0, destination)
        self.assertIsInstance(destination["file"], Reference)
        self.assertNotIn("hidden", destination)
        decode(["helper", None, 42], 0, destination)
        self.assertIsNone(destination["file"])
        omitted = {"id": 42}
        prepare_decoder(fields, [("id", 0)])([42], 0, omitted)
        self.assertNotIn("file", omitted)
