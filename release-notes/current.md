# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Outbound HTTP timeouts (new `http:` config block).** Every HTTP client PeakBot builds — LLM completions, embeddings, MCP auth, web tools — now has a connect timeout (default 30 s) and a read timeout (default 600 s); `0` disables either. Previously there were none, so an upstream that accepted a request and never sent response headers wedged the turn indefinitely: observed in the wild as a sub-agent delegation stuck for 56 minutes, freed only when the proxy's own timeout eventually returned a 502. Stop could not cancel it, because the stop flag is only checked after a completion returns. Note that completions are non-streaming, so for LLM calls the read timeout acts as a ceiling on a single generation — raise `read_timeout_secs` if you run models that think for longer than 10 minutes. Boot-only, like `web:` and `vector_db:`.
