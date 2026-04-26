# PeakBot release notes

This directory holds per-release Markdown files used as the body of
both the Gitea release page and the annotated git tag.

## Convention

- One file per shipped version: `release-notes/<v>.md` (e.g. `0.3.0.md`).
- File version `<v>` must match the `make release VERSION=<v>` value.
- Plain Markdown — Gitea renders it on the release page and `git show <v>`
  prints it from the tag object.
- Commit the notes file *before* running `make release`, ideally in the
  same PR / branch that contains the changes being shipped.

## Resolution order in `make release`

1. `NOTES=<path>` on the make CLI overrides everything.
2. Otherwise: `release-notes/<v>.md` if present.
3. Otherwise: literal fallback `Release <v>` (pipeline never blocks).

## Example

```bash
$EDITOR release-notes/0.3.0.md
git add release-notes/0.3.0.md
git commit -m "docs: 0.3.0 release notes"
make release VERSION=0.3.0
```

See `agents.md` § *Release pipeline → Release notes* for the full
walkthrough.
