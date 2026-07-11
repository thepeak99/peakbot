# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Web UI — slash `/` command palette restored under `make dev`.** The Vite dev server (`:5173`) only proxied `/ws`, so the composer's `GET /commands` fetch hit the SPA fallback and got `index.html` back. `r.ok` was `true` (a 200), so `r.json()` threw and the error was silently swallowed — the command list stayed empty and the palette never rendered. Added `/commands` to the Vite proxy so dev mirrors prod. (The embedded `peakbot --web` binary on `:7823` was never affected.)

- **Conversation listing is now instant via a summary index.** `FileStorage::list()` no longer deserializes every conversation file (the `/conversations` picker, auto-resume, and max-conversations pruning all went through it). It reads a single derived `index.json` of summaries instead. On a real 418 MB / 1612-conversation history this drops listing from ~470 ms to ~2 ms (~220×), and it stays flat as history grows. The index is a cache — the per-conversation JSON files remain authoritative; it is kept in sync on save/delete and self-heals via a full rebuild when missing, corrupt, or when the file count drifts (e.g. after an upgrade or files copied in out-of-band). Index writes are atomic (temp + rename) and serialised by a mutex since one `FileStorage` is shared across web sessions.

- **Web UI — mobile drawer**: full-viewport aside (was a slide-in with `max-w-sm`), left edge meets the screen edge. Replaced the "Sidebar" header with PeakBot branding.
- **Web UI — composer**: dropped the long "Send a message… (tap Send, 📎/paste/drag to attach an image)" placeholder for a single short "Type a message…" on every viewport. The hint row under the composer (Enter to send, / for commands, Shift+Enter newline, attach shortcuts) is now desktop-only — on touch screens it just ate vertical real estate.
- **Web UI — sessions bar**: moved the conversations picker out of the sidebar and into the mobile bottom bar next to the model switcher and cwd picker, AND added it to the desktop top bar next to model + cwd. The three session-affecting controls (conversations, model, cwd) now live together on every viewport — top bar on lg+, bottom bar on mobile. Menus open upward (`dropUp`) on mobile to avoid clipping.
- **Web UI — sidebar (desktop + drawer)**: now shows stats/context, todos, and background processes only — conversations are gone.
- Web UI: fixed the top and bottom bars being hidden behind mobile browser chrome (address bar / tab toolbar). The root container now uses `h-dvh` (dynamic viewport height) instead of `h-screen` (`100vh`), so the app fills the *currently visible* viewport instead of the chrome-collapsed maximum.
- Web UI: the on-screen keyboard now resizes the layout instead of pushing the top bar off-screen — added `interactive-widget=resizes-content` to the viewport meta, so `h-dvh` shrinks to the space above the keyboard and the app compresses in place.
