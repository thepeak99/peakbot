// Pure router for typed session verbs. While a turn runs the server queues
// every session verb except stop/pause/resume until it ends, so `switch_cwd`
// and a typed `/new` / `/load` get lost on Stop — this maps raw composer text
// to an action that never sends a session verb to a busy session.

export type VerbAction =
  | { kind: "send" }
  | { kind: "cd"; path: string }
  | { kind: "switch_cwd"; path: string }
  | { kind: "load"; convo: string }
  | { kind: "refuse"; message: string };

const REFUSE_LOAD_INDEX = "/load by index needs the turn to finish — press Stop first.";

// Absolute as typed: leading / \ ~, or a Windows drive letter.
const ABSOLUTE = /^([/\\~]|[A-Za-z]:)/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** The server's resolver resolves relative paths against the PROCESS cwd,
 * not the session cwd — the client must resolve them, without doubling a
 * separator the cwd already carries. */
function joinCwd(cwd: string, arg: string): string {
  return /[/\\]$/.test(cwd) ? cwd + arg : `${cwd}/${arg}`;
}

export function routeSessionVerb(
  text: string,
  ctx: { busy: boolean; cwd: string },
): VerbAction {
  const m = /^\/(\S+)(?:\s+([\s\S]*))?$/u.exec(text.trim());
  if (!m) return { kind: "send" };
  const verb = m[1].toLowerCase();
  const arg = (m[2] ?? "").trim();

  if (verb === "cd") {
    if (arg === "") return { kind: "send" };
    return { kind: "cd", path: ABSOLUTE.test(arg) ? arg : joinCwd(ctx.cwd, arg) };
  }
  if (verb === "new") {
    if (arg !== "") return { kind: "send" };
    return ctx.busy ? { kind: "switch_cwd", path: ctx.cwd } : { kind: "send" };
  }
  if (verb === "load") {
    if (UUID.test(arg)) {
      return ctx.busy ? { kind: "load", convo: arg } : { kind: "send" };
    }
    return ctx.busy ? { kind: "refuse", message: REFUSE_LOAD_INDEX } : { kind: "send" };
  }
  return { kind: "send" };
}
