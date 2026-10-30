// Minimal ambient typing for the one Node API tests need to read a source
// file as text (the web package deliberately does not depend on @types/node).
// Vitest stubs `import "*.css"` to an empty string, so CSS-contract tests must
// read the file from disk instead.
declare module "node:fs" {
  export function readFileSync(path: string, encoding: "utf8"): string;
}
