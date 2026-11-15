// Path display helpers. Used by CwdPicker, ActivityRow and DirectoryBrowser.

/** Truncate long paths to a 40-char max, eliding the middle. */
export function displayPath(path: string): string {
  if (path.length <= 40) return path;
  const head = path.slice(0, 16);
  const tail = path.slice(-20);
  return `${head}…${tail}`;
}

/** Last path component (POSIX or Windows separators). */
export function basename(path: string): string {
  const parts = path.replace(/[/\\]+$/, "").split(/[/\\]+/);
  return parts[parts.length - 1] || "/";
}