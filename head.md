# Plan: Add head/tail Parameters to Bash Tool

## TL;DR

Add `head` and `tail` parameters to bash tool for controlling output visibility; guide model to use these instead of `| head`/`| tail` in commands.

## What's Working

- Full output already saved to `/tmp/peakbot/` — this is the foundation we build on
- Exit code always shown
- Stderr separated from stdout
- Truncation already keeps end (good for logs)

## What Changes

### 1. `src/tools/bash.rs` — Add parameters

**BashArgs:**
```rust
#[derive(Deserialize)]
pub struct BashArgs {
    command: String,
    timeout_seconds: Option<u64>,
    head: Option<usize>,  // NEW: show first N lines
    tail: Option<usize>,  // NEW: show last N lines (default: 100)
}
```

**Logic in `call()`:**
- Split stdout/stderr into lines
- If `head` specified: keep first N lines
- If `tail` specified: keep last N lines (default 100)
- If both specified: head at top, tail at bottom with `... X lines in between ...` separator
- Full output always saved to file (already implemented)
- Always show "Full output: /path/to/file" when output is modified

**Truncation function:**
- Rename `truncate_from_beginning` → handle head/tail on lines
- Keep char-limit safety net for excessively long lines

### 2. Update Tool Definition

**Description:**
```
Run a shell command and return stdout and stderr.
Use `head` to show first N lines, `tail` to show last N lines (default: 100).
Full output is always saved to /tmp/peakbot/ and accessible via file_read.
Commands run in /bin/sh. Default timeout is 30 seconds.
```

**JSON Schema parameters:**
```json
{
    "command": { "type": "string", "description": "The shell command to execute" },
    "timeout_seconds": { "type": "integer", "description": "Optional timeout in seconds (default: 30, max: 120)" },
    "head": { "type": "integer", "description": "Show first N lines of output (optional)" },
    "tail": { "type": "integer", "description": "Show last N lines of output (default: 100, use 0 for all)" }
}
```

### 3. System Prompt — Model Guidance

Add to system prompt building in `src/lib.rs`:

```
BASH TOOL USAGE:
- Do NOT use `| head` or `| tail` in commands — use the head/tail parameters instead
- The full output is always saved to /tmp/peakbot/ regardless of head/tail settings
- Default tail is 100 lines; set tail: 0 to see all output (or use file_read on the saved file)
```

## Behavior Examples

| Parameters | Output Shown |
|------------|--------------|
| `tail: 50` (default) | Last 50 lines |
| `head: 20` | First 20 lines |
| `head: 10, tail: 10` | First 10, ellipsis, last 10 |
| `tail: 0` | All output (no truncation) |
| No params | Last 100 lines (default) |

Full file always at: `/tmp/peakbot/bash_TIMESTAMP_COUNTER.stdout.txt`

## Files to Modify

1. `src/tools/bash.rs` — Add head/tail to args, update logic
2. `src/lib.rs` — Add bash usage guidance to system prompt

## What NOT to Do

- Don't add char-based head/tail (lines are more useful for shell output)
- Don't change the file saving behavior (already works)
- Don't add complexity like regex filtering or line numbers
- Don't make head/tail affect the saved file (always save full output)
