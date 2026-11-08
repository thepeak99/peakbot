// Routes composer submits through `routeSessionVerb` and applies the result
// without ever sending a session verb to a busy session (the server would
// queue it until the turn ends and lose it on Stop) — busy switches re-attach
// to a fresh conversation carrying the verb as its first frame instead.

import { useCallback, useEffect, useRef } from "react";
import { routeSessionVerb } from "./sessionVerbs";
import type { AgentConnection } from "./useAgent";

export function useSessionVerbs({
  agent,
  busy,
  cwd,
  loadConversation,
  onError,
}: {
  agent: Pick<AgentConnection, "send" | "switchConvo" | "dirListing">;
  busy: boolean;
  cwd: string;
  loadConversation: (convo: string) => void;
  onError: (msg: string) => void;
}): { submit(text: string, ids: string[]): void; switchCwd(path: string): void } {
  // A /cd is resolved via list_dir first: the reply carries the canonical
  // path, and the switch_cwd rides on that reply.
  const pendingCd = useRef(false);
  // Latest values: the dir_listing effect must act with the busy/cwd as of
  // reply time, not as of the moment the verb was typed.
  const busyRef = useRef(busy);
  const cwdRef = useRef(cwd);
  const loadConversationRef = useRef(loadConversation);
  const onErrorRef = useRef(onError);
  useEffect(() => {
    busyRef.current = busy;
    cwdRef.current = cwd;
    loadConversationRef.current = loadConversation;
    onErrorRef.current = onError;
  }, [busy, cwd, loadConversation, onError]);

  const switchCwd = useCallback(
    (path: string) => {
      if (busyRef.current) {
        // Busy: switch_cwd on the live socket would be queued — re-attach to
        // a fresh conversation that carries it as its first frame.
        agent.switchConvo(null, undefined, undefined, { type: "switch_cwd", path });
      } else {
        agent.send({ type: "switch_cwd", path });
      }
    },
    [agent],
  );

  const submit = useCallback(
    (text: string, ids: string[]) => {
      const cwd = cwdRef.current;
      const action = routeSessionVerb(text, { busy: busyRef.current, cwd });
      switch (action.kind) {
        case "send":
          agent.send({ type: "send_message", text, attachments: ids });
          break;
        case "cd":
          pendingCd.current = true;
          agent.send({ type: "list_dir", path: action.path });
          break;
        case "switch_cwd":
          switchCwd(action.path);
          break;
        case "load":
          loadConversationRef.current(action.convo);
          break;
        case "refuse":
          onErrorRef.current(action.message);
          break;
      }
    },
    [agent, switchCwd],
  );

  useEffect(() => {
    if (!pendingCd.current) return;
    pendingCd.current = false;
    const listing = agent.dirListing;
    if (!listing) return;
    if (listing.error) {
      onErrorRef.current(`/cd: ${listing.error}`);
      return;
    }
    switchCwd(listing.path);
  }, [agent, agent.dirListing, switchCwd]);

  return { submit, switchCwd };
}
