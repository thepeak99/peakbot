# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Mobile app scaffold (M1).** New `mobile/` Tauri 2 app crate (`peakbot-mobile`, identifier `sh.shifu.mobile`) for Android + iOS: bundled Profiles page shell, `peakbot` linked `--no-default-features` (the Local edition), Android project committed with the locked toolchain pins (AGP 9.3.1 / Gradle 9.6.1 / Kotlin 2.2.10, SDK 37, minSdk 24) and a network-security config that trusts system + user CAs and allows cleartext only for the in-phone core on `127.0.0.1`. The crate joins the root Cargo workspace (one `Cargo.lock`), and `make release-bump` / `check-version.sh` now keep the mobile versions in lockstep with the root. No user-facing change yet — the app ships with the later mobile releases.

### Fixes

- Desktop: "Check for updates…" (macOS app menu and tray) now only checks — it no longer downloads and installs an already-known update immediately. When an update is found you get a dialog, and installing happens from the update banner.

- **Mandatory Discord release announcement.** Every release (stable and RC) now gets exactly one Discord embed-card post as the final step, only after the release is verified (all 10 Gitea assets, shifu `SHA256SUMS`, GitHub `latest` tag for stables). One format: an embed card that always pings @everyone via `content` + `allowed_mentions` (a mention inside an embed does not ping), posted to the `DISCORD_WEBHOOK_AI` webhook with `?wait=true`. Full format, template, and limits are defined in `agents.md` › "Release announcement (mandatory)".
