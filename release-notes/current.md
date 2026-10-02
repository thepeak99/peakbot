# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- File sandbox v1: `sandbox:` (master config or a profile, never per-repo) gates `file_create`/`file_str_replace`/`file_insert` with three modes — `off` (default), `read-only`, and `workspace-write` (session cwd + configured `writable_roots`). Reads remain unrestricted and the shell is never sandboxed.
