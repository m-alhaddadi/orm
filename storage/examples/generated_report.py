"""A report producer independent of ORM models and SQL transactions."""
from orm_storage import Provider, Reference


async def generate_report(storage: Provider) -> Reference:
    # A producer may supply a large async stream instead of these CSV bytes.
    return await storage.upload(b"product,total\nexample,42\n", filename="report.csv", content_type="text/csv")

# Application composition:
# reference = await generate_report(configured_reports_provider)
# row = await Report.objects.insert(file=reference)
# Keep reference in application scope if the SQL/outer commit acknowledgement fails.
# No row replacement, deletion or rollback deletes the physical report.
