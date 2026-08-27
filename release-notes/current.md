# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- Two-phase conversation titles: a provisional title (first ~60 chars of your opening message, no LLM) appears the moment you send it, and upgrades to an LLM-generated title after the first reply — plus an escape hatch that generates the definitive title the moment you message a previously interrupted conversation. Also fixes a stale-slot race that could write a title onto the wrong conversation and a UTF-8 panic in title truncation.
