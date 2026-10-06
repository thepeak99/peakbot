// Composer attachment state: pre-checks, one XHR upload per file, object-URL
// previews. Lives in App (not Composer) so the drop overlay shares it.

import { useCallback, useEffect, useRef, useState } from "react";
import { uploadFile } from "./uploads";
import { formatSize, isThumbnailable } from "./fileTypes";
import type { UploadLimits, WireAttachment } from "./state";

export type Pending = { key: string; file: File; preview?: string } & (
  | { state: "uploading"; progress: number; abort: () => void }
  | { state: "done"; attachment: WireAttachment }
  | { state: "error"; message: string }
);

export interface Attachments {
  pending: Pending[];
  add: (files: File[]) => void;
  remove: (key: string) => void;
  /** Revokes previews and aborts in-flight uploads. */
  reset: () => void;
  /** Done attachments' ids, in chip order. */
  ids: string[];
  /** No chip is uploading or errored. */
  ready: boolean;
}

const MB = 1024 * 1024;

export function useAttachments(
  convo: string | null,
  limits: UploadLimits,
): Attachments {
  const [pending, setPending] = useState<Pending[]>([]);
  // Mirror of `pending` that is current within one event: add() may run twice
  // before a render, and upload callbacks need the latest chips without
  // re-subscribing.
  const chips = useRef<Pending[]>([]);
  const seq = useRef(0);
  const lastConvo = useRef(convo);

  const commit = useCallback((next: Pending[]) => {
    chips.current = next;
    setPending(next);
  }, []);

  const patch = useCallback(
    (key: string, fn: (c: Pending) => Pending) => {
      // A chip removed mid-flight must not be resurrected by its own promise.
      if (!chips.current.some((c) => c.key === key)) return;
      commit(chips.current.map((c) => (c.key === key ? fn(c) : c)));
    },
    [commit],
  );

  const dispose = useCallback((list: Pending[]) => {
    for (const c of list) {
      if (c.preview) URL.revokeObjectURL(c.preview);
      if (c.state === "uploading") c.abort();
    }
  }, []);

  const reset = useCallback(() => {
    const old = chips.current;
    // Clear first: abort() rejects asynchronously and patch() then no-ops.
    commit([]);
    dispose(old);
  }, [commit, dispose]);

  const remove = useCallback(
    (key: string) => {
      const gone = chips.current.filter((c) => c.key === key);
      commit(chips.current.filter((c) => c.key !== key));
      dispose(gone);
    },
    [commit, dispose],
  );

  const add = useCallback(
    (files: File[]) => {
      let next = chips.current;
      const started: { key: string; file: File }[] = [];
      for (const file of files) {
        const key = `a${(seq.current += 1)}`;
        const preview = isThumbnailable(file.type)
          ? URL.createObjectURL(file)
          : undefined;
        const fail = (message: string) => {
          next = [...next, { key, file, preview, state: "error", message }];
        };
        // Errored chips don't count: they never reach the server.
        const live = next.filter((c) => c.state !== "error").length;
        if (!convo) {
          fail("Not connected — try again");
        } else if (file.size > limits.max_file_mb * MB) {
          fail(
            `Too large (${formatSize(file.size)} · max ${limits.max_file_mb} MB)`,
          );
        } else if (live >= limits.max_files) {
          fail(`Too many files (max ${limits.max_files})`);
        } else {
          next = [
            ...next,
            { key, file, preview, state: "uploading", progress: 0, abort: () => {} },
          ];
          started.push({ key, file });
        }
      }
      commit(next);
      for (const { key, file } of started) {
        const handle = uploadFile(convo!, file, (progress) =>
          patch(key, (c) => (c.state === "uploading" ? { ...c, progress } : c)),
        );
        // abort() only exists once the XHR does, so wire it in after the call.
        patch(key, (c) =>
          c.state === "uploading" ? { ...c, abort: handle.abort } : c,
        );
        handle.done.then(
          (attachment) =>
            patch(key, (c) => ({
              key: c.key,
              file: c.file,
              preview: c.preview,
              state: "done",
              attachment,
            })),
          (err: unknown) =>
            patch(key, (c) => ({
              key: c.key,
              file: c.file,
              preview: c.preview,
              state: "error",
              message: err instanceof Error ? err.message : "Upload failed",
            })),
        );
      }
    },
    [convo, limits.max_file_mb, limits.max_files, commit, patch],
  );

  // A conversation switch invalidates every stored id (uploads are per-convo).
  useEffect(() => {
    if (lastConvo.current === convo) return;
    lastConvo.current = convo;
    reset();
  }, [convo, reset]);

  // Unmount: don't leak blob URLs or leave uploads running.
  useEffect(() => () => dispose(chips.current), [dispose]);

  const ids = pending.flatMap((c) => (c.state === "done" ? [c.attachment.id] : []));
  const ready = pending.every((c) => c.state === "done");
  return { pending, add, remove, reset, ids, ready };
}
