# PeakBot Mobile

Tauri 2 shell for Android + iOS. Its only native UI is the bundled **Profiles**
page; every profile is just an origin the webview loads:

- **Remote** — one of your PeakBot servers (`https://host:port`), added by pasting
  the pairing link (or scanning its QR). The server's own SPA takes over, same-origin.
- **On this phone** — a PeakBot core compiled without hands, in-process, on
  `http://127.0.0.1:47823` with a per-launch token.

## Server prerequisites (Remote profiles)

Any PeakBot `>= 0.27.0` (the release that ships the `peakbotShell` seam and
`boot.edition`) with:

- `general.web` bound to `0.0.0.0:7823` (or your LAN/Tailscale address),
- `tls: true` (HTTPS is the only way a phone trusts you),
- a web token (pairing links carry it).

The phone trusts the server's self-signed CA only after you install it
(Settings → Security → Install a certificate → CA on Android; Safari profile
install + full trust on iOS). The app offers an [Install certificate…] shortcut
that opens `https://host/peakbot-ca.crt` in the system browser. Off-LAN, put the
server on Tailscale (`*.ts.net` with a real certificate needs no CA install).

Build docs and pairing/CA walkthroughs land with the later phases.
