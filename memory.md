# PeakBot Memory

> Worktree copy for `feat/image-lightbox` (`peakbot-image-lightbox`), seeded 2026-10-29.

## Episodic
- **2026-10-29 — Full-screen lightbox for all chat images** (branch `feat/image-lightbox`, off `1cc2a9f`; RED tests in `9f4b9dc`).
  - `CachedImage` (Message.tsx) no longer wraps markdown images in `<a target=_blank>`; the `<img>` itself is clickable (`cursor-zoom-in`) and opens the existing `Lightbox` via a local `useState`. The 🖼 fallback stays non-clickable.
  - `AttachmentChip` composer image thumbnails are now clickable too (blob preview when present, else `/api/uploads/{convo}/{id}`); the × remove button does not open the lightbox.
  - `Lightbox`'s `images` prop narrowed from `AttachmentView[]` to `{id,name,url}[]` — the three fields it reads.
  - Gate (`lint && typecheck && test && build`) green locally before the PR.

## Semantic
- **Lightbox consumers:** `Lightbox` (web/src/components/Lightbox.tsx) is used in three places — `Attachments` (user-message thumbnails, Message.tsx), `CachedImage` (markdown images in agent replies, Message.tsx), and `AttachmentChip` (composer pending attachments). It portals to `<body>`, closes on ✕/Esc/backdrop, and cycles arrows only when `images.length > 1`.
