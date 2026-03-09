# Task 6: Debug ApiResponse JsonError - Detailed Plan

## Error Description

**Error Message:**
```
Error: CompletionError: JsonError: data did not match any variant of untagged enum ApiResponse
```

**Error Type:** `rig::completion::error::CompletionError::JsonError`

**Location:** Occurs during LLM completion calls through the rig-core library when parsing the API response from OpenRouter.

---

## Problem Analysis

### What This Error Means

The error occurs when deserializing the API response from OpenRouter. Rig-core uses an untagged enum `ApiResponse` to handle multiple response formats, but the actual response from OpenRouter doesn't match any of the expected variants.

### Potential Causes

1. **OpenRouter returns a new response format** that rig-core hasn't accounted for
2. **Streaming vs non-streaming response mismatch** - response format differs based on `stream` parameter
3. **Error responses** - OpenRouter returns error responses in a different format
4. **Model-specific responses** - Different models may return slightly different response structures
5. **Timeout/interruption** - Partial response that can't be parsed
6. **Rate limiting** - OpenRouter returns 429 with specific JSON format
7. **rig-core version mismatch** - Using an older version of rig-core

### Likely Scenarios Based on Research

Looking at similar issues in the rig-core repository and OpenRouter API:

1. **Tool call responses** - When Claude returns tool calls, the response format includes additional fields like `tool_calls` that might not be handled
2. **Content blocking** - When content is blocked, response format differs
3. **Streaming end markers** - SSE stream ending markers cause parsing issues
4. **Usage data** - Missing or malformed `usage` field in response

---

## Investigation Phase

### Step 1: Gather Environment Information

Collect data about when the error occurs:

```bash
# Get rig-core version
grep "rig-core" Cargo.lock

# Check OpenRouter API version being used
# (Need to look at rig-core source or docs)
```

**Checklist:**
- [ ] Record rig-core version (`0.31.0` currently)
- [ ] Record model being used (`anthropic/claude-3.7-sonnet` default)
- [ ] Record if error happens during:
  - [ ] First message
  - [ ] After tool calls
  - [ ] During streaming
  - [ ] Sporadically

### Step 2: Add Debug Logging

Add detailed request/response logging to capture the raw data causing the error:

**File: `src/lib.rs`**

```rust
// Add wrapper around client to log requests/responses
// Or use a middleware pattern

use rig::client::completion::CompletionClient;

// After building client, wrap it to add logging
// Log: request URL, headers (without sensitive data), body
// Log: response status, headers, body (first N chars)
```

**Checklist:**
- [ ] Log outgoing request: model, messages count, tools count
- [ ] Log incoming response status code
- [ ] Log raw response body (truncated to first 1000 chars on error)
- [ ] Save full error response to file for analysis
- [ ] Add `RUST_LOG=trace` support for verbose debugging

### Step 3: Reproduce the Error

Try to trigger the error consistently:

1. **Long conversations** - Let conversation grow, then trigger tool call
2. **Specific tools** - Try with different tools
3. **Force streaming vs non-streaming** - Test both modes
4. **Error scenarios** - Trigger rate limits or invalid requests

**Checklist:**
- [ ] Create test case that reproduces error (if possible)
- [ ] Document exact steps to reproduce
- [ ] Note what was different about this request vs successful ones

### Step 4: Examine Rig-Core Source

```bash
# Find rig-core in cargo cache
find ~/.cargo/registry/src -name "rig-core*" -type d 2>/dev/null | head -5

# Look at the ApiResponse enum definition
grep -r "ApiResponse" ~/.cargo/registry/src/*/rig-core-*/src/ 2>/dev/null | head -20
```

**Checklist:**
- [ ] Find `ApiResponse` enum definition in rig-core
- [ ] List all variants of the enum
- [ ] Understand the deserialization logic
- [ ] Identify which variant is failing to match

### Step 5: Compare with OpenRouter Response

Check what OpenRouter actually returns:

```bash
# Test API call directly with curl
curl -X POST https://openrouter.ai/api/v1/chat/completions \
  -H "Authorization: Bearer $OPENROUTER_API_KEY" \
  -H "Content-Type: application/json" \
  -H "HTTP-Referer: http://localhost" \
  -d '{
    "model": "anthropic/claude-3.7-sonnet",
    "messages": [{"role": "user", "content": "Hello"}],
    "stream": false
  }'
```

**Checklist:**
- [ ] Compare successful response format with rig-core expectations
- [ ] Check for differences in:
  - [ ] Field names (camelCase vs snake_case)
  - [ ] Optional fields presence
  - [ ] Nested structures
  - [ ] Tool call format variations

---

## Solution Phase

### Option 1: Update Rig-Core (Preferred)

Check if newer version has fix:

```bash
# Check latest version
cargo search rig-core

# Update to latest
cargo update rig-core

# Test if error persists
```

**Checklist:**
- [ ] Check latest rig-core version on crates.io
- [ ] Update and test
- [ ] If fixed, update Cargo.toml and document

### Option 2: Workaround in PeakBot

If rig-core can't be updated or fix isn't available:

#### 2a. Add Retry Logic

**File: `src/lib.rs`**

```rust
// Wrap completion calls with retry
async fn completion_with_retry(
    agent: &Agent<M>,
    prompt: &str,
    max_retries: u32,
) -> Result<String> {
    let mut last_error = None;
    
    for attempt in 0..max_retries {
        match agent.prompt(prompt).await {
            Ok(response) => return Ok(response),
            Err(e) => {
                last_error = Some(e);
                // Check if it's the JsonError
                if is_retryable_error(&e) {
                    let delay = Duration::from_secs(2u64.pow(attempt));
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(e);
            }
        }
    }
    Err(last_error.unwrap())
}

fn is_retryable_error(e: &anyhow::Error) -> bool {
    let error_str = e.to_string();
    error_str.contains("JsonError") && 
    error_str.contains("ApiResponse")
}
```

**Checklist:**
- [ ] Implement retry function
- [ ] Add exponential backoff
- [ ] Limit max retries
- [ ] Log retry attempts

#### 2b. Add Error Recovery

```rust
// If we detect this specific error, try alternative parsing
fn handle_api_response_error(raw_response: &str) -> Result<Value> {
    // Try to parse as different formats
    // Log which format worked
}
```

**Checklist:**
- [ ] Create error handling that tries alternative parsing
- [ ] Log which format finally worked
- [ ] Report upstream to rig-core

#### 2c. Disable Streaming

If error is streaming-related, try non-streaming:

**File: `src/lib.rs`**

```rust
// In build_agent, try to disable streaming if available
// Some rig-core versions support this

client.agent(model_name)
    .stream(false)  // If supported
    // ...
```

**Checklist:**
- [ ] Check if rig-core supports forcing non-streaming
- [ ] Test if error disappears with streaming disabled

### Option 3: Fork and Patch

If the fix is simple but not merged:

```bash
# Fork rig-core on GitHub
# Apply fix to ApiResponse enum
# Use git revision in Cargo.toml
```

**Checklist:**
- [ ] Fork rig-core
- [ ] Identify minimal fix
- [ ] Apply patch
- [ ] Use patched version in Cargo.toml
- [ ] Submit PR to upstream

---

## Implementation Steps

### Phase 1: Investigation (Priority 1)

| Step | Task | Owner | Status |
|------|------|-------|--------|
| 1.1 | Add detailed error logging to capture raw response | Agent | ⬜ |
| 1.2 | Update rig-core to latest version and test | Agent | ⬜ |
| 1.3 | Check for related GitHub issues in rig-core | Agent | ⬜ |
| 1.4 | Document exact conditions when error occurs | Agent | ⬜ |

### Phase 2: Short-term Fix (Priority 2)

| Step | Task | Owner | Status |
|------|------|-------|--------|
| 2.1 | Implement retry logic with exponential backoff | Agent | ⬜ |
| 2.2 | Add specific error detection for ApiResponse error | Agent | ⬜ |
| 2.3 | Add user-facing error message improvement | Agent | ⬜ |
| 2.4 | Log error occurrence for monitoring | Agent | ⬜ |

### Phase 3: Long-term Fix (Priority 3)

| Step | Task | Owner | Status |
|------|------|-------|--------|
| 3.1 | If fix available in rig-core, update dependency | Agent | ⬜ |
| 3.2 | If no fix available, patch locally or fork | Agent | ⬜ |
| 3.3 | Add integration test to prevent regression | Agent | ⬜ |
| 3.4 | Document workaround in agents.md | Agent | ⬜ |

---

## Testing Plan

### Unit Tests

```rust
#[test]
fn test_retry_on_json_error() {
    // Mock completion that fails with JsonError then succeeds
}

#[test]
fn test_error_message_is_helpful() {
    // Verify error message includes actionable info
}
```

### Integration Tests

1. **Successful completion test** - Verify normal operation still works
2. **Error reproduction test** - If reproducible, test the fix
3. **Retry behavior test** - Verify retry logic works correctly
4. **Logging test** - Verify error details are logged

---

## Monitoring and Metrics

Add tracking for this error:

```rust
// In error handling
metrics::increment_counter!("peakbot_completion_errors_total", 
    "error_type" => "api_response_json");
```

**Metrics to track:**
- Total occurrences of this error
- Frequency over time
- Correlation with other factors (model, message count, tool use)

---

## Rollout Plan

1. **Stage 1**: Deploy with enhanced logging (no behavior change)
2. **Stage 2**: After identifying root cause, deploy fix
3. **Stage 3**: Monitor error rate, iterate if needed

---

## Success Criteria

- [ ] Error rate drops to 0 (or near 0)
- [ ] User experience improves (retries work transparently)
- [ ] We can identify root cause and potentially upstream fix
- [ ] Documentation updated for future reference

---

## References

- [Rig-core GitHub](https://github.com/0xPlaygrounds/rig)
- [Rig-core Issues](https://github.com/0xPlaygrounds/rig/issues)
- [OpenRouter API Docs](https://openrouter.ai/docs/api-overview)
- [OpenRouter Status Page](https://status.openrouter.ai/)

---

## Notes

- Current rig-core version: `0.31.0`
- Error appears to be intermittent, suggesting timing or specific response condition
- Tool call responses are a likely culprit based on similar issues in other SDKs