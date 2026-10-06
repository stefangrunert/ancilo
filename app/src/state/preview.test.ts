// Ported from Jan's web-app/src/lib/__tests__/coworkPreview.test.ts
// (Apache-2.0, Menlo Research) where the functions were taken over; the
// kinds are Ancilo's.
import { basenameOf, extensionOf, loaded, markStale, previewKindFor } from "./preview";

describe("previewKindFor", () => {
  it("maps the kinds Ancilo shows", () => {
    expect(previewKindFor("a/Übersicht.xlsx")).toBe("table");
    expect(previewKindFor("export.CSV")).toBe("table");
    expect(previewKindFor("Brief.docx")).toBe("document");
    expect(previewKindFor("README.md")).toBe("text");
  });

  it("falls back to a file card for unknown and binary types", () => {
    expect(previewKindFor("deck.pptx")).toBe("file");
    expect(previewKindFor("report.pdf")).toBe("file");
    expect(previewKindFor("archive.zip")).toBe("file");
    expect(previewKindFor("bin/tool")).toBe("file");
  });

  it("treats a dotfile as having no extension, not an extension", () => {
    expect(extensionOf(".gitignore")).toBe("");
    expect(previewKindFor(".gitignore")).toBe("file");
  });

  it("reads the extension from the basename, not an earlier path segment", () => {
    expect(extensionOf("my.dir/file")).toBe("");
    expect(previewKindFor("v1.2/notes.md")).toBe("text");
  });

  it("handles windows separators", () => {
    expect(basenameOf("a\\b\\Kosten.xlsx")).toBe("Kosten.xlsx");
    expect(previewKindFor("a\\b\\Kosten.xlsx")).toBe("table");
  });
});

describe("preview states", () => {
  it("a file that is not shown never loads", () => {
    expect(loaded("x.pdf", {}, "v1")).toEqual({ status: "unsupported", path: "x.pdf" });
  });

  it("a newer version marks a ready preview instead of replacing it", () => {
    const ready = loaded("k.xlsx", { rows: 1 }, "v1");
    expect(markStale(ready, "v1")).toBe(ready);
    const stale = markStale(ready, "v2");
    expect(stale).toMatchObject({ status: "ready", version: "v1", stale: true, data: { rows: 1 } });
    expect(markStale({ status: "loading", path: "k.xlsx" }, "v2")).toEqual({ status: "loading", path: "k.xlsx" });
  });
});
