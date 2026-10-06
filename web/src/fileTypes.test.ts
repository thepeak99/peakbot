// RED (T10): `fileTypes.ts` does not exist yet. These tests are written
// against the §5.7 SPA contract:
//
//   fileGroup(name, mime) — by extension first, then mime prefix:
//     pdf: pdf · doc: txt md markdown rst doc docx odt rtf tex log
//     sheet: csv tsv xls xlsx ods · code: rs ts tsx js jsx py go java c h
//       cpp hpp cs rb php swift kt sh bash zsh ps1 sql json yaml yml toml
//       xml html css scss lua r ipynb · archive: zip tar gz tgz bz2 xz 7z
//       rar zst · audio: audio/* · video: video/* · everything else: other
//   GROUP_STYLE — tailwind accent class per group (pdf red-400, doc sky-400,
//     sheet emerald-400, code violet-400, archive amber-400, audio pink-400,
//     video orange-400, other zinc-400)
//   extLabel(name) — "PDF", "TS", ≤4 chars, "" if none
//   middleTruncate(name, max = 24) — keeps the extension: "very-lo…name.pdf"
//   formatSize(bytes) — mirrors Rust human_size (base 1024, 1 dp)
//   isThumbnailable(mime) — png/jpeg/gif/webp only
import { describe, expect, it } from "vitest";
import {
  GROUP_STYLE,
  extLabel,
  fileGroup,
  formatSize,
  isThumbnailable,
  middleTruncate,
} from "./fileTypes";

describe("fileGroup", () => {
  // One case per group (doc and code get a second, common extension).
  it.each([
    ["spec.pdf", "application/pdf", "pdf"],
    ["notes.md", "text/markdown", "doc"],
    ["report.docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document", "doc"],
    ["data.csv", "text/csv", "sheet"],
    ["main.rs", "text/x-rustsrc", "code"],
    ["app.tsx", "text/plain", "code"],
    ["stuff.zip", "application/zip", "archive"],
    ["track.mp3", "audio/mpeg", "audio"],
    ["clip.mov", "video/quicktime", "video"],
    ["mystery.xyz", "application/octet-stream", "other"],
  ])("fileGroup(%s, %s) → %s", (name, mime, group) => {
    expect(fileGroup(name, mime)).toBe(group);
  });

  it("matches uppercase extensions", () => {
    expect(fileGroup("SPEC.PDF", "application/pdf")).toBe("pdf");
    expect(fileGroup("NOTES.MD", "text/markdown")).toBe("doc");
    expect(fileGroup("DATA.CSV", "text/csv")).toBe("sheet");
  });

  it("prefers the extension over the mime prefix", () => {
    // Extension-first rule: a .md file with a useless mime is still doc.
    expect(fileGroup("notes.md", "application/octet-stream")).toBe("doc");
  });

  it("falls back to the mime prefix for audio/video with no known extension", () => {
    expect(fileGroup("track", "audio/mpeg")).toBe("audio");
    expect(fileGroup("clip", "video/mp4")).toBe("video");
  });

  it("returns other for no extension and no matching mime", () => {
    expect(fileGroup("README", "application/octet-stream")).toBe("other");
  });
});

describe("GROUP_STYLE", () => {
  it("covers every group with its design colour", () => {
    expect(GROUP_STYLE.pdf.color).toContain("red-400");
    expect(GROUP_STYLE.doc.color).toContain("sky-400");
    expect(GROUP_STYLE.sheet.color).toContain("emerald-400");
    expect(GROUP_STYLE.code.color).toContain("violet-400");
    expect(GROUP_STYLE.archive.color).toContain("amber-400");
    expect(GROUP_STYLE.audio.color).toContain("pink-400");
    expect(GROUP_STYLE.video.color).toContain("orange-400");
    expect(GROUP_STYLE.other.color).toContain("zinc-400");
  });
});

describe("extLabel", () => {
  it("uppercases the extension", () => {
    expect(extLabel("spec.pdf")).toBe("PDF");
    expect(extLabel("app.tsx")).toBe("TSX");
    expect(extLabel("data.csv")).toBe("CSV");
  });

  it("uses the last extension for multi-dot names", () => {
    expect(extLabel("archive.tar.gz")).toBe("GZ");
  });

  it("returns an empty string when there is no extension", () => {
    expect(extLabel("README")).toBe("");
    expect(extLabel("")).toBe("");
  });

  it("caps the label at 4 characters", () => {
    const label = extLabel("doc.xhtml");
    expect(label.length).toBeLessThanOrEqual(4);
    expect(label).toMatch(/^XHT/);
  });
});

describe("middleTruncate", () => {
  it("leaves short names untouched", () => {
    expect(middleTruncate("cat.png")).toBe("cat.png");
  });

  it("leaves a name of exactly the default max length untouched", () => {
    const name = "a".repeat(20) + ".pdf"; // 24 chars = default max
    expect(name).toHaveLength(24);
    expect(middleTruncate(name)).toBe(name);
  });

  it("truncates the middle of long names, keeping the extension", () => {
    const out = middleTruncate("very-long-filename-here.pdf");
    expect(out.length).toBeLessThanOrEqual(24);
    expect(out).toContain("…");
    expect(out.endsWith(".pdf")).toBe(true);
    expect(out.startsWith("very")).toBe(true);
  });

  it("truncates names without an extension too", () => {
    const out = middleTruncate("a".repeat(40));
    expect(out.length).toBeLessThanOrEqual(24);
    expect(out).toContain("…");
  });

  it("honours a custom max", () => {
    const out = middleTruncate("abcdefghij.pdf", 12);
    expect(out.length).toBeLessThanOrEqual(12);
    expect(out).toContain("…");
    expect(out.endsWith(".pdf")).toBe(true);
  });
});

describe("formatSize (mirrors Rust human_size: base 1024, 1 dp)", () => {
  it("formats bytes below 1 KiB as 'N B'", () => {
    expect(formatSize(0)).toBe("0 B");
    expect(formatSize(512)).toBe("512 B");
    expect(formatSize(1023)).toBe("1023 B");
  });

  it("formats KiB with one decimal", () => {
    expect(formatSize(1024)).toBe("1.0 KB");
    expect(formatSize(1229)).toBe("1.2 KB");
    expect(formatSize(1536)).toBe("1.5 KB");
  });

  it("formats MiB with one decimal", () => {
    expect(formatSize(1048576)).toBe("1.0 MB");
    expect(formatSize(2411724)).toBe("2.3 MB");
  });
});

describe("isThumbnailable", () => {
  it.each(["image/png", "image/jpeg", "image/gif", "image/webp"])(
    "accepts %s",
    (mime) => {
      expect(isThumbnailable(mime)).toBe(true);
    },
  );

  it.each(["image/svg+xml", "image/heic", "application/pdf", "text/plain"])(
    "rejects %s",
    (mime) => {
      expect(isThumbnailable(mime)).toBe(false);
    },
  );
});
