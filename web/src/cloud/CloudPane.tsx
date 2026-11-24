// P4 — Settings → Cloud pane (design §A.1): one view per CloudState.

import { useState, type ReactNode } from "react";
import { COPY } from "./copy";
import { store } from "./store";
import { SafeDialog } from "./SafeDialog";
import { SasWords } from "./SasWords";
import { useCloud } from "./useCloud";
import type { Device, StateBody } from "./types";

type Confirm =
  | { kind: "first" }
  | { kind: "revoke"; device: Device }
  | { kind: "signout" };

const BTN =
  "rounded-md border border-zinc-600 bg-zinc-800 px-3 py-1.5 text-sm text-zinc-100 hover:bg-zinc-700 disabled:opacity-50";

/** Inline lines under the pane header: `last_login_error` + store errors. */
function statusLines(snapshot: StateBody | null, error: string | null, errorCode: string | null): ReactNode {
  if (!snapshot) return null;
  const lle =
    snapshot.state !== "active" && snapshot.last_login_error
      ? COPY.lastLoginError(snapshot.last_login_error)
      : null;
  const mapped =
    error && errorCode
      ? (COPY.err as Record<string, string>)[errorCode] ?? error
      : error;
  if (!lle && !mapped) return null;
  return (
    <div className="mb-3 space-y-1 text-sm">
      {lle && <p className="text-red-400">{lle}</p>}
      {mapped && <p className="text-red-400">{mapped}</p>}
    </div>
  );
}

function dotClass(online: boolean | null): string {
  if (online === true) return "h-2.5 w-2.5 rounded-full bg-green-500";
  if (online === false) return "h-2.5 w-2.5 rounded-full bg-zinc-600";
  return "h-2.5 w-2.5 rounded-full border border-zinc-500";
}

function dotLabel(online: boolean | null): string {
  if (online === true) return COPY.active.online;
  if (online === false) return COPY.active.offline;
  return COPY.active.unknown;
}

export function CloudPane() {
  const view = useCloud();
  const [confirm, setConfirm] = useState<Confirm | null>(null);
  const signout = (): void => setConfirm({ kind: "signout" });
  // The url of the last sign-in attempt, so the pane can offer a manual link
  // when the popup is blocked (WebKitGTK/AppImage) and we don't navigate.
  const [authUrl, setAuthUrl] = useState<string | null>(null);
  const [signInOpened, setSignInOpened] = useState(false);
  const { snapshot, busy, error, error_code } = view;
  // A `login_in_flight` 409 is a "keep waiting", not an error.
  const visibleError = error_code === "login_in_flight" ? null : error;

  async function signIn(): Promise<void> {
    setSignInOpened(false);
    setAuthUrl(null);
    try {
      const res = (await store.act("/api/cloud/login")) as { auth_url?: string } | null;
      const url = res?.auth_url;
      if (!url) return;
      setAuthUrl(url);
      setSignInOpened(true);
      // One path for desktop and mobile: noopener returns null by spec, so
      // never pass it — sever `opener` on the handle and navigate in-tab if
      // the popup was blocked.
      let w: Window | null = null;
      try {
        w = window.open(url, "_blank");
      } catch {
        w = null;
      }
      if (w) w.opener = null;
      else window.location.assign(url);
    } catch {
      // The store holds the message; a pending flow just keeps its note.
      if (store.getSnapshot().error_code === "login_in_flight") setSignInOpened(true);
    }
  }

  const signOut = confirm?.kind === "signout" && (
    <SafeDialog
      title={COPY.confirm.signOut.title}
      onLater={() => setConfirm(null)}
      actions={[
        { label: COPY.btn.cancel, kind: "neutral", onClick: async () => setConfirm(null) },
        {
          label: COPY.confirm.signOut.confirm,
          kind: "danger",
          onClick: async () => {
            try {
              await store.act("/api/cloud/logout");
            } finally {
              setConfirm(null);
            }
          },
        },
      ]}
    >
      <p>{COPY.confirm.signOut.body}</p>
    </SafeDialog>
  );

  if (!snapshot) {
    return (
      <div className="p-1 text-sm text-zinc-300">
        <p>{COPY.loading}</p>
      </div>
    );
  }

  let body: ReactNode;
  switch (snapshot.state) {
    case "disabled":
      body = <p>{COPY.disabled}</p>;
      break;
    case "logged_out":
      body = (
        <>
          <p>{COPY.loggedOut[snapshot.reason]}</p>
          {signInOpened && (
            <p className="mt-2 text-xs text-zinc-400">{COPY.signInOpened}</p>
          )}
          {authUrl && (
            <p className="mt-1 text-xs">
              <a
                className="text-sky-400 underline"
                href={authUrl}
                target="_blank"
                rel="noopener"
              >
                {COPY.btn.openSignInPage}
              </a>
            </p>
          )}
          <div className="mt-3">
            <button type="button" className={BTN} disabled={busy} onClick={() => void signIn()}>
              {COPY.btn.signIn}
            </button>
          </div>
        </>
      );
      break;
    case "connecting":
      body = (
        <>
          <p>{COPY.connecting(snapshot.attempt)}</p>
          {snapshot.last_error && (
            <p className="mt-1 text-xs text-zinc-400">{snapshot.last_error}</p>
          )}
        </>
      );
      break;
    case "pending":
      if (snapshot.rejected) {
        body = (
          <>
            <p className="text-red-400">{COPY.rejected.title}</p>
            <p className="mt-1 text-red-300">{COPY.rejected.body}</p>
            <div className="mt-3 flex flex-wrap gap-2">
              <button
                type="button"
                className={BTN}
                disabled={busy}
                onClick={() => void store.act("/api/cloud/enroll/retry")}
              >
                {COPY.btn.tryAgain}
              </button>
              <button
                type="button"
                className={BTN}
                disabled={busy}
                onClick={signout}
              >
                {COPY.btn.signOut}
              </button>
            </div>
          </>
        );
      } else if (snapshot.empty_account) {
        body = (
          <>
            <p className="font-medium">{COPY.pendingEmpty.title}</p>
            <p className="mt-1">{COPY.pendingEmpty.body}</p>
            <div className="mt-3">
              <button
                type="button"
                className={BTN}
                disabled={busy}
                onClick={() => setConfirm({ kind: "first" })}
              >
                {COPY.btn.makeFirst}
              </button>
            </div>
          </>
        );
      } else if (snapshot.sas) {
        body = (
          <>
            <p className="font-medium">{COPY.pending.title}</p>
            <SasWords words={snapshot.sas} />
            <p className="text-sm text-zinc-300">{COPY.pending.body}</p>
            <div className="mt-3">
              <button
                type="button"
                className={BTN}
                disabled={busy}
                onClick={signout}
              >
                {COPY.btn.signOut}
              </button>
            </div>
          </>
        );
      } else {
        body = <p>{COPY.pending.requesting}</p>;
      }
      break;
    case "active":
      body = (
        <>
          <p className="font-medium">{COPY.active.title}</p>
          {snapshot.requests.length > 0 && (
            <ul className="mt-2 space-y-1">
              {snapshot.requests.map((r) => (
                <li key={r.letter_id} className="flex items-center gap-2 text-sm">
                  <span>{COPY.active.waiting(r.name, r.platform)}</span>
                  <button
                    type="button"
                    className={BTN}
                    disabled={busy}
                    onClick={() => store.undismiss(r.letter_id)}
                  >
                    {COPY.btn.review}
                  </button>
                </li>
              ))}
            </ul>
          )}
          <ul className="mt-2 divide-y divide-zinc-800">
            {snapshot.devices.map((d) => (
              <li key={d.id} className="flex items-center gap-2 py-1.5 text-sm">
                <span aria-label={dotLabel(d.online)} className={dotClass(d.online)} />
                <span className="text-zinc-100">{d.name}</span>
                <span className="text-xs text-zinc-400">{d.platform}</span>
                {d.self && <span className="text-xs text-zinc-400">{COPY.active.self}</span>}
                {!d.self && (
                  <button
                    type="button"
                    className={`${BTN} ml-auto`}
                    disabled={busy}
                    onClick={() => setConfirm({ kind: "revoke", device: d })}
                  >
                    {COPY.btn.revoke}
                  </button>
                )}
              </li>
            ))}
          </ul>
          <div className="mt-3">
            <button
              type="button"
              className={BTN}
              disabled={busy}
              onClick={signout}
            >
              {COPY.btn.signOut}
            </button>
          </div>
        </>
      );
      break;
    case "trust":
      body = (
        <>
          <p className="font-medium">{COPY.trust.paneTitle}</p>
          <p className="mt-1">{COPY.trust.paneBody}</p>
          <p className="mt-1 text-xs text-zinc-400">{COPY.banner[snapshot.trust_error]}</p>
          <div className="mt-3">
            <button
              type="button"
              className={BTN}
              disabled={busy}
              onClick={signout}
            >
              {COPY.btn.signOut}
            </button>
          </div>
        </>
      );
      break;
    default: {
      const _exhaustive: never = snapshot;
      void _exhaustive;
      body = null;
      break;
    }
  }

  return (
    <div className="p-1 text-sm text-zinc-300">
      {/* Confirms render FIRST in DOM order: their buttons may share labels
          with the pane's (e.g. "Revoke"), and the tests resolve by text. */}
      {confirm?.kind === "first" && (
        <SafeDialog
          title={COPY.confirm.first.title}
          onLater={() => setConfirm(null)}
          actions={[
            { label: COPY.btn.cancel, kind: "neutral", onClick: async () => setConfirm(null) },
            {
              label: COPY.confirm.first.confirm,
              kind: "danger",
              onClick: async () => {
                try {
                  await store.act("/api/cloud/genesis");
                } finally {
                  setConfirm(null);
                }
              },
            },
          ]}
        >
          <p>{COPY.confirm.first.body}</p>
        </SafeDialog>
      )}
      {confirm?.kind === "revoke" && (
        <SafeDialog
          title={COPY.confirm.revoke.title(confirm.device.name)}
          onLater={() => setConfirm(null)}
          actions={[
            { label: COPY.btn.cancel, kind: "neutral", onClick: async () => setConfirm(null) },
            {
              label: COPY.confirm.revoke.confirm,
              kind: "danger",
              onClick: async () => {
                try {
                  await store.act(`/api/cloud/devices/${confirm.device.id}/revoke`);
                } finally {
                  setConfirm(null);
                }
              },
            },
          ]}
        >
          <p>{COPY.confirm.revoke.body}</p>
        </SafeDialog>
      )}
      {signOut}
      {statusLines(snapshot, visibleError, error_code)}
      {body}
    </div>
  );
}