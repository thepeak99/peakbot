// Minimal ambient typing for the Node APIs tests need to read source files
// and resolve paths (the web package deliberately does not depend on @types/node).
// Vitest stubs `import "*.css"` to an empty string, so CSS-contract tests must
// read the file from disk instead.
declare module "node:fs" {
  export function readFileSync(path: string, encoding: "utf8"): string;
  export function readdirSync(path: string): string[];
  export function readdirSync(path: string, opts: { withFileTypes: true }): Array<{
    name: string;
    isFile(): boolean;
    isDirectory(): boolean;
  }>;
  export function statSync(path: string): { isFile(): boolean; isDirectory(): boolean };
}

declare module "node:path" {
  export function join(...segments: string[]): string;
}

declare const __dirname: string;
