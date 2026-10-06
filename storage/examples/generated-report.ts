/** A report producer independent of ORM models and SQL transactions. */
import type { Provider, Reference } from "@orm/storage";

export async function generateReport(storage: Provider): Promise<Reference> {
  return storage.upload(new TextEncoder().encode("product,total\nexample,42\n"), {
    filename: "report.csv", contentType: "text/csv",
  });
}

// Application composition:
// const reference = await generateReport(configuredReportsProvider);
// const row = await Report.objects.insert({ file: reference });
// Keep reference in application scope after uncertain SQL/outer commit outcomes.
// Replacement, deletion and rollback never delete the physical report.
