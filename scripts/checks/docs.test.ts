import { describe, expect, test } from "bun:test";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { inspectDocuments, lintDocuments, markdownFiles, renderDiagrams } from "./docs";

async function fixture(files: Record<string, string>, verify: (root: string) => Promise<void>) {
  const root = await mkdtemp(join(tmpdir(), "f5y-docs-test-"));
  try {
    for (const [path, content] of Object.entries(files)) {
      const target = join(root, path);
      await mkdir(join(target, ".."), { recursive: true });
      await writeFile(target, content);
    }
    await verify(root);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

describe("documentation validation", () => {
  test("recursively resolves reference links, encoded paths and duplicate heading anchors", async () => {
    await fixture({
      "README.md": "# Home\n\n[Guide][guide]\n\n[guide]: docs/nested/my%20guide.md#install-1\n",
      "docs/nested/my guide.md": "# Install\n\n## Install\n\n[Home](../../README.md#home)\n",
      "node_modules/ignored/README.md": "[Missing](missing.md)",
    }, async (root) => {
      const files = await markdownFiles(root);
      expect(files).toHaveLength(2);
      expect((await inspectDocuments(root, files)).errors).toEqual([]);
    });
  });

  test("rejects missing files, missing anchors, malformed escapes and root traversal", async () => {
    await fixture({ "README.md": "# Home\n\n[File](missing.md)\n[Anchor](#absent)\n[Escape](%ZZ)\n[Outside](../escape.md)\n" }, async (root) => {
      const { errors } = await inspectDocuments(root, await markdownFiles(root));
      expect(errors).toHaveLength(4);
      expect(errors.join("\n")).toContain("heading anchor does not exist");
      expect(errors.join("\n")).toContain("link leaves documentation root");
    });
  });

  test("extracts nested Mermaid fences while ignoring example text", async () => {
    await fixture({ "README.md": "# Diagram\n\n> ```mermaid\n> graph TD\n>   A --> B\n> ```\n\n````text\n```mermaid\ninvalid\n```\n````\n" }, async (root) => {
      const { diagrams } = await inspectDocuments(root, await markdownFiles(root));
      expect(diagrams).toHaveLength(1);
      expect(diagrams[0].source).toContain("A --> B");
    });
  });

  test("rejects malformed Markdown through the pinned linter", async () => {
    await fixture({ "README.md": "#Heading\n" }, async (root) => {
      await expect(lintDocuments(await markdownFiles(root))).rejects.toThrow("MD018");
    });
  });

  test("renders a real SVG in the pinned browser", async () => {
    await renderDiagrams([{ label: "valid fixture", source: "flowchart LR\n A[Input] --> B[Output]" }]);
  }, 120_000);

  test("rejects invalid Mermaid through the real renderer", async () => {
    await expect(renderDiagrams([{ label: "invalid fixture", source: "flowchart LR\n A[unclosed" }])).rejects.toThrow("Parse error");
  }, 120_000);
});
