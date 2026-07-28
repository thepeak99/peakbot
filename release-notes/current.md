# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **Compaction in sub-agent conversations**: The compaction gate now reads only the orchestrator lane's last request size, so a sub-agent's large internal turn no longer triggers premature compaction. This also prevents the summary from being wedged between a tool call and its result after compacting, which could cause the provider to reject the conversation.
