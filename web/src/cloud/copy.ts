// P4 — the cloud copy (design §D), verbatim. ONE file; the views render these
// constants and the tests assert against the same constants, so the strings
// can't drift.

export const COPY = {
  // D.1 pane
  loading: "Connecting to PeakBot…",
  disabled: "Cloud mode is off on this machine.",
  loggedOut: {
    never: "Sign in to sync your devices.",
    expired: "Your session expired. Sign in again.",
    signed_out: "You signed out.",
  },
  btn: {
    signIn: "Sign in",
    makeFirst: "Make this the first device",
    tryAgain: "Try again",
    review: "Review",
    revoke: "Revoke",
    signOut: "Sign out",
    reject: "Reject",
    approve: "Approve",
    later: "Not now",
    cancel: "Cancel",
    openCloudSettings: "Open Cloud settings",
  },
  signInOpened: "Finish signing in in the new tab.",
  connecting: (n: number) => `Connecting… (attempt ${n})`,
  pendingEmpty: {
    title: "Set up your first device",
    body: "No device has been set up on this account yet. This machine will " +
      "become the root of trust: every future device must be approved by one " +
      "you already own. Only do this on a machine you control.",
  },
  pending: {
    requesting: "Requesting approval…",
    title: "Waiting for approval",
    body: "Open PeakBot on another of your devices and approve this one within " +
      "15 minutes. Check that the words match.",
  },
  rejected: {
    title: "Not approved",
    body: "Another of your devices rejected this one. If that was a mistake, " +
      "try again and approve it there.",
  },
  active: {
    title: "Your devices",
    self: "(this device)",
    online: "Online",
    offline: "Offline",
    unknown: "Status unknown",
    waiting: (name: string, platform: string) => `Waiting: ${name} (${platform})`,
  },
  trust: {
    paneTitle: "Cloud is stopped",
    paneBody:
      "Sign out to reset cloud on this device. You'll need another device's " +
      "approval to rejoin.",
  },
  lastLoginError: (e: string) => `Sign-in failed: ${e}`,
  stale: "Reconnecting to PeakBot…",

  // D.2 approver modal
  approve: {
    title: (name: string) => `Add '${name}'?`,
    body: "Only approve if these words match the new device.",
    meta: (platform: string, m: number, ss: number) =>
      `${platform} · expires in ${m}:${String(ss).padStart(2, "0")}`,
  },

  // D.3 confirm dialogs
  confirm: {
    first: {
      title: "Make this the first device?",
      body:
        "Trust starts here. This machine becomes the root that approves all " +
        "your other devices. If someone else has already signed in to this " +
        "account, do not continue.",
      confirm: "Make first device",
    },
    revoke: {
      title: (name: string) => `Revoke '${name}'?`,
      body:
        "It will lose access to your devices. This cannot be undone; it must " +
        "be re-approved to rejoin.",
      confirm: "Revoke",
    },
    signOut: {
      title: "Sign out of cloud on this device?",
      body:
        "This device will be removed from your account. Another device must " +
        "approve it to rejoin.",
      confirm: "Sign out",
    },
  },

  // D.4 trust banner (per §A.4)
  banner: {
    genesis_mismatch:
      "Cloud trust error: this account's root device changed. Cloud is stopped.",
    fork:
      "Cloud trust error: conflicting device histories detected. Cloud is stopped.",
    revoked: "This device was revoked by another of your devices.",
    chain_invalid:
      "Cloud trust error: the device history is invalid. Cloud is stopped.",
  },

  // D.5 error code → inline message
  err: {
    settings_disabled: "Settings are locked on this device.",
    not_active: "Cloud isn't connected yet. Try again in a moment.",
    trust: "Cloud is stopped because of a trust error.",
    already_logged_in: "Already signed in.",
    not_pending_empty: "This account already has a device.",
    genesis_taken: "Another device just became first; waiting for its approval.",
    chain_busy: "Another change is in progress. Try again.",
    last_device: "You can't revoke the last device; sign out instead.",
    unknown_device: "That device is no longer on your account.",
    use_logout: "To remove this device, sign out.",
    bad: "Something went wrong. Reload and try again.",
    offline: "That device is offline.",
    refused: "That device refused the request.",
    relay: "The cloud service didn't respond. Try again.",
    network: "Can't reach PeakBot. Try again.",
  },
} as const;

export type Copy = typeof COPY;
