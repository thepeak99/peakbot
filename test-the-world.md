# Compaction & Summarization Test Plan

## Problem Statement

The compaction/summarization system has significant test gaps. The mock LLM
(`MockCompletionModel`) discards every `CompletionRequest` it receives (`_request`),
so no test ever verifies what was actually sent to the LLM for summarization.
Tests verify side effects (counts, events) but have zero visibility into the
content flowing through the system.

---

## Phase 0: Infrastructure — Make the Mock Observable

Before any new test can verify summarization content, the mock needs to record
what it receives.

### 0.1 Record CompletionRequests in MockCompletionModel

**File:** `src/mock/completion_model.rs`

Add `recorded_requests: Arc<Mutex<Vec<CompletionRequest>>>` to the struct.
In `completion()`, clone and push the request before popping the response.
Expose:

- `get_recorded_requests() -> Vec<CompletionRequest>` — all requests in order
- `last_request() -> Option<CompletionRequest>` — convenience
- `request_count() -> usize` — total calls made

The `_request` parameter becomes `request`.

### 0.2 Expose Recorded Requests Through TestHarness

**File:** `tests/harness/test_harness.rs`

Add:

- `get_recorded_requests()` — delegates to `mock_model`
- `get_summarization_requests()` — filters to requests whose last message
  contains `"Please summarize the following conversation"` (the prompt prefix
  from `ContextManager::summarize_messages`)
- `get_regular_requests()` — everything else

### 0.3 CompletionRequest Must Be Clone

Check if rig's `CompletionRequest` derives `Clone`. If not, record the
fields we care about into a local `RecordedRequest` struct:
```
struct RecordedRequest {
    preamble: Option<String>,
    chat_history: Vec<Message>,  // flattened from OneOrMany
    tool_count: usize,
}
```

---

## Phase 1: Verify Summarization Content

These tests verify that the LLM receives the correct data when asked to
summarize. They depend on Phase 0.

### 1.1 Summarization Request Contains the Right Old Messages

Build 6 messages (3 user + 3 assistant), trigger compaction with `keep_recent=2`.
After compaction, inspect the summarization request's `chat_history`. The last
message (the prompt) should contain the formatted text of messages 1-2 (the old
ones), NOT messages 3-6 (the kept-recent ones).

Assert:
- Summarization request exists (exactly 1)
- Prompt text contains `"Please summarize the following conversation"`
- Prompt text contains content from old messages (msg 1, msg 2)
- Prompt text does NOT contain content from kept-recent messages

### 1.2 Summarization Request Excludes Recent Messages

Same setup as 1.1 but with distinctive message content (`"OLD_MSG_1"`,
`"RECENT_MSG_1"`, etc.) to make substring assertions unambiguous.

Assert:
- `"OLD_MSG"` appears in the summarization prompt
- `"RECENT_MSG"` does NOT appear in the summarization prompt

### 1.3 Tool Calls Are Formatted in Summarization Input

Build history with tool call/result pairs in the old region. Trigger compaction.
Inspect the summarization prompt to verify tool interactions are included in the
formatted text (or explicitly excluded — either way, verify the behavior is
intentional).

Note: `format_messages_for_summary` skips non-User/Assistant variants with a
catch-all `_ => continue`. So tool messages are intentionally excluded from the
summary input. This test should verify that contract.

### 1.4 Number of LLM Calls Matches Expectations

For a conversation where compaction triggers once:
- Expected: N regular calls + 1 summarization call
- Assert `mock.request_count() == N + 1`
- Assert `get_summarization_requests().len() == 1`
- Assert `get_regular_requests().len() == N`

### 1.5 Post-Compaction Regular Request Contains Compacted History

After compaction, the next regular `prompt_with_history()` call should receive
the compacted history (summary + kept-recent), not the original full history.
Inspect the regular request's `chat_history` after the compaction turn.

Assert:
- The chat_history in the post-compaction request is shorter than pre-compaction
- The first message in chat_history contains `"[Previous conversation summary:"`

---

## Phase 2: Verify Summarization Output in State

These tests verify that the summary produced by the LLM actually lands in
the right place and has the right shape. These don't need Phase 0 (they
inspect state, not requests).

### 2.1 Summary Text Appears in History

Trigger compaction. Check that `get_agent_history()` contains a User message
starting with `"[Previous conversation summary:"` and that it contains the
mock's summary text (`"Summary of previous conversation."`).

### 2.2 Post-Compaction History Has Correct Structure

With `keep_recent=2`, after compaction + one new turn, history should be:
```
[summary_user_msg, kept_user, kept_assistant, new_user, new_assistant]
```
Assert exact message count and roles in order.

### 2.3 Summary Persists to StateManager Chat Messages

After compaction, `get_state().chat.messages` should contain a message with
the summary text. Verify it's there and in the right position (first).

### 2.4 Summarization Failure Falls Back to Truncation

Queue NO summarization response (or an error response) so `agent.prompt()`
fails. Verify:
- Compaction still completes (doesn't panic/propagate error)
- History still shrinks (truncation fallback)
- No summary message in history (just the kept-recent messages)

---

## Phase 3: Verify Compaction Timing

These tests verify compaction fires at exactly the right moment — not one
turn early, not one turn late.

### 3.1 Compaction Triggers at Exact Turn

Document the threshold math explicitly in the test:
```
context_window=500, threshold=0.5 -> 250 tokens
keep_recent=2
Response input_tokens=300 (above threshold)
```

Sequence:
- Turn 1: 2 messages total (== keep_recent), 300 tokens but skip (msgs <= keep_recent) -> NO compaction
- Turn 2: 4 messages (> keep_recent), compaction check sees turn 1's 300 tokens > 250 -> **but wait**: compaction runs BEFORE `prompt_with_history`, so it sees stats from turn 1. Does it? Verify this.
- Turn 3: compaction check sees turn 2's tokens

Assert compaction event count after each turn: `[0, 0, 1]` or `[0, 1, 1]`
depending on the actual timing. The point is to nail it down precisely.

### 3.2 Compaction Does NOT Trigger Below Threshold

`context_window=1000, threshold=0.8` (800 tokens), responses with 100
input_tokens. Send 10 messages. Assert zero compaction events AND all 20
messages present.

(This test exists but should be strengthened with request-count assertions
from Phase 0 to prove no summarization call was made.)

### 3.3 Compaction Does NOT Trigger When msgs <= keep_recent

Even with tokens above threshold, if message count <= keep_recent, compaction
should not fire. Test with `keep_recent=10`, send 3 messages with 500
input_tokens each on a 400-token threshold. Assert no compaction.

### 3.4 Compaction Timing With the Token Stats Pipeline

Verify the exact flow:
1. Turn 1 produces response with usage
2. Turn 2's `process_session_hook_events()` syncs turn 1's stats
3. Turn 2's `compact_if_needed()` reads `last_input_tokens` from turn 1
4. If above threshold AND msgs > keep_recent, compact fires

This is the subtle ordering dependency. The test should assert that
`get_current_tokens()` returns the expected value at the right moment.

---

## Phase 4: Verify Queue Consumption

These tests verify the fragile response-queue interleaving is correct and
catch misalignment early.

### 4.1 Compaction Turn Consumes Exactly 2 Responses

```
remaining_before = harness.remaining_responses()
harness.run_message("trigger").await
remaining_after = harness.remaining_responses()
assert_eq!(remaining_before - remaining_after, 2)  // 1 summarization + 1 regular
```

### 4.2 Non-Compaction Turn Consumes Exactly 1 Response

Same pattern but for a turn where compaction doesn't fire.
```
assert_eq!(remaining_before - remaining_after, 1)  // 1 regular only
```

### 4.3 Queue Misalignment Causes Detectable Failure

Intentionally queue responses in the wrong order (summarization response where
agent response should be, and vice versa). Assert that the test produces a
detectably wrong result — this validates that the other tests would catch a
real ordering bug rather than silently passing.

---

## Phase 5: Multi-Compaction & Edge Cases

### 5.1 Multiple Compactions Produce Stacked Summaries

Run enough messages for 2+ compaction events. After the second compaction,
the summarization request (Phase 0) should contain the FIRST summary as part
of the old messages being summarized. Verify this chain.

### 5.2 Compaction With Tool Calls Crossing the Boundary

Build history where a ToolCall is in the old region but its ToolResult is in
the kept-recent region. After compaction:
- The ToolCall message must be preserved (not summarized away)
- The summary should NOT include that tool call
- History should contain: `[summary, preserved_tool_call, tool_result, ...]`

This exercises `find_needed_tool_calls` through the full E2E path.

### 5.3 Message-Count Fallback (No Token Data)

Use `MockResponse::text()` (no usage) so `get_current_tokens()` returns 0.
Fallback threshold: `(keep_recent * 3).max(10)`.

With `keep_recent=3`, threshold = 10 messages:
- Send 9 messages (18 chat entries) -> should NOT trigger (msg count, not chat entry count... or is it? Verify.)
- Send 11 messages -> should trigger

This tests the dead fallback path that no E2E test currently covers.

### 5.4 Compaction With Empty/Whitespace Messages

Build history where some messages are empty strings or whitespace.
Verify compaction handles them without panicking and the summary
input doesn't contain empty `"User: \n\n"` blocks.

### 5.5 keep_recent=0 Edge Case

What happens when ALL messages should be discarded and replaced with a
summary? Verify this doesn't panic and produces `[summary_only]`.

---

## Phase 6: Cleanup

### 6.1 Remove Dead Code

- `TestRunner::run_message_with_history()` — never called by any test
- `TestHarness::run_message_with_history()` — same

### 6.2 Strengthen Existing Tests

- `compaction_reduces_history`: replace `final_count < count_after_1 + 4`
  with exact expected count based on keep_recent + summary + new turn
- `compaction_preserves_recent_messages`: remove the `if has_compaction_occurred`
  guard — compaction MUST occur in this test, make it an assert
- `multiple_compaction_events`: verify each event's `num_discarded` matches
  expected values, not just > 0

---

## Implementation Order

```
Phase 0  (infrastructure)     — required by Phase 1 and parts of Phase 3-4
Phase 2  (output in state)    — no dependencies, can parallelize with Phase 0
Phase 1  (content)            — depends on Phase 0
Phase 3  (timing)             — partially depends on Phase 0
Phase 4  (queue)              — depends on Phase 0
Phase 5  (edge cases)         — depends on Phase 0
Phase 6  (cleanup)            — anytime
```

Phases 0 and 2 can be done in parallel as a starting point.

---

## Tracking

| Phase | Test | Status |
|-------|------|--------|
| 0.1 | Record requests in mock | done |
| 0.2 | Expose through harness | done |
| 0.3 | Clone or local struct | done (CompletionRequest is Clone; used RecordedRequest) |
| 1.1 | Summarization has right old msgs | done |
| 1.2 | Summarization excludes recent msgs | done |
| 1.3 | Tool calls in summarization input | done |
| 1.4 | LLM call count matches | done |
| 1.5 | Post-compaction request has compacted history | done |
| 2.1 | Summary text in history | done |
| 2.2 | Post-compaction history structure | done |
| 2.3 | Summary persists to StateManager | done |
| 2.4 | Summarization failure fallback | done |
| 3.1 | Exact trigger turn | done |
| 3.2 | No trigger below threshold | done |
| 3.3 | No trigger when msgs <= keep_recent | done |
| 3.4 | Token stats pipeline ordering | done |
| 4.1 | Compaction turn consumes 2 responses | done |
| 4.2 | Non-compaction turn consumes 1 | done |
| 4.3 | Queue misalignment detection | done (as queue_consumption_pattern_across_turns) |
| 5.1 | Stacked summaries | done |
| 5.2 | Tool calls crossing boundary E2E | done |
| 5.3 | Message-count fallback | done |
| 5.4 | Empty/whitespace messages | done |
| 5.5 | keep_recent=0 edge case | done |
| 6.1 | Remove dead code | done |
| 6.2 | Strengthen existing tests | done |
