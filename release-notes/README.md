# PeakBot release notes

This directory holds per-release Markdown files used as the body of
both the Gitea release page and the annotated git tag.

## Convention

- One file per shipped version: `release-notes/<v>.md` (e.g. `0.3.0.md`).
- File version `<v>` must match the tag name: `make release-bump VERSION=<v>`
  bumps the crates; `make release-tag` (or `make release TAG=<v>`) tags it.
- Plain Markdown — Gitea renders it on the release page and `git show <v>`
  prints it from the tag object.
- Notes content lives in `release-notes/current.md` until `make release-bump`
  promotes it to `release-notes/<v>.md` on the `release/<v>` branch; commit
  notes changes with that release branch (PR), before the merge.

## Release body

There is no resolution order: `release-notes/<v>.md` is **required**
(`release-check` exits non-zero with the missing notes file named). The `NOTES=`
override and the `Release <v>` fallback have been removed — without the notes
file, no release can be tagged.

## Example

```bash
$EDITOR release-notes/current.md      # entry for 0.3.0 under "## Changes"
make release-bump VERSION=0.3.0       # branch + bump + promote + push
# open a PR; wait for ci.yml green; merge
make release-tag                      # tag 0.3.0, message = release-notes/0.3.0.md
                                      # the push triggers release.yml, which builds
                                      # and publishes all 8 assets
```

See `agents.md` § *Release notes workflow* and *Release pipeline* for the full
walkthrough.
