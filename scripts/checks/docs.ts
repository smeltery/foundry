import { readdir, readFile, stat, mkdtemp, writeFile, rm } from "node:fs/promises";
import { dirname, join, relative, resolve, sep } from "node:path";
import { tmpdir } from "node:os";
import { lexer, walkTokens, type Token } from "marked";
import GithubSlugger from "github-slugger";

const projectRoot = resolve(import.meta.dir, "../..");
const ignoredDirectories = new Set([".git", ".flox", "node_modules", "target"]);
const markdownExtension = /\.(?:md|markdown)$/i;

type Document = { path: string; tokens: ReturnType<typeof lexer> };
type Diagram = { source: string; label: string };

export async function markdownFiles(root: string): Promise<string[]> {
  const files: string[] = [];
  async function visit(directory: string) {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory() && !ignoredDirectories.has(entry.name)) await visit(path);
      else if (entry.isFile() && markdownExtension.test(entry.name)) files.push(path);
    }
  }
  await visit(root);
  return files.sort();
}

function plainText(tokens: Token[]): string {
  return tokens.map((token) => {
    if ("tokens" in token && token.tokens) return plainText(token.tokens);
    if (token.type === "html") return "";
    return "text" in token ? token.text : "";
  }).join("");
}

function anchors(document: Document): Set<string> {
  const slugger = new GithubSlugger();
  const result = new Set<string>();
  walkTokens(document.tokens, (token) => {
    if (token.type === "heading") result.add(slugger.slug(plainText(token.tokens)));
  });
  return result;
}

export async function inspectDocuments(root: string, files: string[]) {
  const documents = new Map<string, Document>();
  for (const path of files) documents.set(path, { path, tokens: lexer(await readFile(path, "utf8")) });
  const errors: string[] = [];
  const diagrams: Diagram[] = [];
  for (const document of documents.values()) {
    const links: string[] = [];
    let diagramIndex = 0;
    walkTokens(document.tokens, (token) => {
      if (token.type === "link" || token.type === "image") links.push(token.href);
      if (token.type === "code" && token.lang?.trim().toLowerCase() === "mermaid") {
        diagrams.push({ source: token.text, label: `${relative(root, document.path)} diagram ${++diagramIndex}` });
      }
    });
    for (const href of links) {
      if (/^(?:[a-z][a-z\d+.-]*:|\/\/)/i.test(href)) continue;
      try {
        const hash = href.indexOf("#");
        const fragment = hash < 0 ? "" : decodeURIComponent(href.slice(hash + 1));
        const pathname = decodeURIComponent((hash < 0 ? href : href.slice(0, hash)).split("?")[0]);
        const target = pathname ? resolve(pathname.startsWith("/") ? root : dirname(document.path), pathname.replace(/^\//, "")) : document.path;
        const outside = relative(root, target);
        if (outside === ".." || outside.startsWith(`..${sep}`)) throw new Error("link leaves documentation root");
        const information = await stat(target);
        if (fragment && markdownExtension.test(target) && information.isFile()) {
          const linked = documents.get(target) ?? { path: target, tokens: lexer(await readFile(target, "utf8")) };
          if (!anchors(linked).has(fragment)) throw new Error("heading anchor does not exist");
        }
      } catch (error) {
        errors.push(`${relative(root, document.path)}: ${href}: ${error instanceof Error ? error.message : String(error)}`);
      }
    }
  }
  return { errors, diagrams };
}

async function command(arguments_: string[], environment = process.env): Promise<void> {
  const child = Bun.spawn(arguments_, { cwd: projectRoot, env: environment, stdout: "pipe", stderr: "pipe" });
  const timeout = setTimeout(() => child.kill(), 120_000);
  try {
    const [code, stdout, stderr] = await Promise.all([
      child.exited, new Response(child.stdout).text(), new Response(child.stderr).text(),
    ]);
    if (code !== 0) throw new Error(`Command failed (${code}): ${stdout}${stderr}`);
  } finally {
    clearTimeout(timeout);
  }
}

export async function lintDocuments(files: string[]): Promise<void> {
  await command(["node", join(projectRoot, "node_modules/markdownlint-cli2/markdownlint-cli2-bin.mjs"), "--config", join(import.meta.dir, "docs.markdownlint.jsonc"), ...files]);
}

export async function renderDiagrams(diagrams: Diagram[]): Promise<void> {
  const temporary = await mkdtemp(join(tmpdir(), "f5y-docs-"));
  const environment = { ...process.env, PUPPETEER_CACHE_DIR: join(projectRoot, ".flox/cache/puppeteer") };
  try {
    for (const [index, diagram] of diagrams.entries()) {
      const input = join(temporary, `${index}.mmd`);
      const output = join(temporary, `${index}.svg`);
      await writeFile(input, diagram.source);
      try {
        await command(["node", join(projectRoot, "node_modules/@mermaid-js/mermaid-cli/src/cli.js"), "-i", input, "-o", output], environment);
        if (!(await readFile(output, "utf8")).includes("<svg")) throw new Error("renderer did not produce SVG");
      } catch (error) {
        throw new Error(`${diagram.label}: ${error instanceof Error ? error.message : String(error)}. Prepare the browser with bun run docs:browser.`);
      }
    }
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

async function main() {
  const arguments_ = process.argv.slice(2);
  if (arguments_.length) throw new Error("Usage: bun scripts/checks/docs.ts");
  const files = await markdownFiles(projectRoot);
  if (files.length === 0) throw new Error("No Markdown documents found");
  await lintDocuments(files);
  const { errors, diagrams } = await inspectDocuments(projectRoot, files);
  if (errors.length) throw new Error(errors.join("\n"));
  if (diagrams.length === 0) throw new Error("Documentation must contain at least one Mermaid diagram");
  await renderDiagrams(diagrams);
  console.log(`Documentation passed: ${files.length} files, ${diagrams.length} diagrams rendered.`);
}

if (import.meta.main) {
  main().catch((error) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
