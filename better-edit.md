# Better File Edit Tool - Implementation Plan

## Overview

This document outlines improvements to PeakBot's `file_edit` tool to make it more robust, user-friendly, and resistant to models falling back to bash/sed commands.

## Current State

PeakBot currently has:
- **`file_read` tool**: Read files with optional line ranges (start_line, end_line)
- **`file_edit` tool**: Three commands (create, str_replace, insert) with strict exact matching

### Current Limitations
1. **Strict exact matching** - Fails on whitespace/indentation differences
2. **No global replace** - Can only replace first occurrence
3. **No fallback strategies** - Single attempt, then error
4. **Basic error messages** - Don't guide the model toward solutions

---

## 🔧 Flexible Matching Implementation

### Goal
Implement progressive fallback matching strategies inspired by Gemini CLI's smart-edit tool, while maintaining safety and precision.

### Matching Strategy Hierarchy

```
┌─────────────────────────────────────────────────────────────┐
│  Level 1: Exact Match (Current Behavior)                    │
│  - Direct literal string comparison                         │
│  - O(1) fast, zero false positives                          │
│  - ✅ Keep as default and first attempt                     │
└─────────────────────────────────────────────────────────────┘
                          ▼
                  (if no match or multiple matches)
                          ▼
┌─────────────────────────────────────────────────────────────┐
│  Level 2: Whitespace-Normalized Match                       │
│  - Trim leading/trailing whitespace per line                │
│  - Normalize multiple spaces to single space                │
│  - Preserve indentation structure                           │
│  - ✅ Catches most formatting differences                   │
└─────────────────────────────────────────────────────────────┘
                          ▼
                  (if still no unique match)
                          ▼
┌─────────────────────────────────────────────────────────────┐
│  Level 3: Flexible Whitespace Regex                         │
│  - Convert old_str to regex with \s+ between tokens         │
│  - Allow variable whitespace between words/tokens           │
│  - ⚠️  Use with caution, higher false positive risk         │
└─────────────────────────────────────────────────────────────┘
                          ▼
                  (if still fails)
                          ▼
┌─────────────────────────────────────────────────────────────┐
│  Level 4: Enhanced Error with Context                       │
│  - Show nearest matches (fuzzy)                             │
│  - Suggest reading file first                               │
│  - Provide line numbers of similar content                  │
└─────────────────────────────────────────────────────────────┘
```

### Implementation Details

#### 1. Enhanced Args Structure

```rust
pub struct FileEditArgs {
    pub command: Command,
    pub path: String,
    pub old_str: Option<String>,  // Required for str_replace
    pub new_str: Option<String>,  // Required for str_replace, insert
    pub insert_line: Option<usize>, // Required for insert
    pub file_text: Option<String>,  // Required for create
    
    // NEW FIELDS:
    pub replace_all: Option<bool>,  // Default: false (single match only)
    pub match_mode: Option<MatchMode>, // Default: None (auto-progress through levels)
}

pub enum MatchMode {
    Exact,           // Level 1 only (current behavior)
    Whitespace,      // Levels 1-2
    Flexible,        // Levels 1-3
    Auto,            // Levels 1-4 (default)
}
```

#### 2. Matching Functions

```rust
impl FileEditTool {
    /// Level 1: Exact match (current implementation)
    fn exact_match(&self, content: &str, old_str: &str) -> MatchResult {
        // Current implementation
    }
    
    /// Level 2: Whitespace-normalized match
    fn whitespace_normalized_match(&self, content: &str, old_str: &str) -> MatchResult {
        let normalize = |s: &str| -> String {
            s.lines()
                .map(|line| line.trim().trim_start_matches(|c| !c.is_whitespace()))
                .collect::<Vec<_>>()
                .join("\n")
        };
        
        let norm_content = normalize(content);
        let norm_old = normalize(old_str);
        
        // Find in normalized, map back to original positions
        // ...
    }
    
    /// Level 3: Flexible whitespace regex
    fn flexible_whitespace_match(&self, content: &str, old_str: &str) -> MatchResult {
        // Tokenize old_str on whitespace
        // Build regex with \s+ between tokens
        // Escape regex special characters in tokens
        // Search with regex
        // ...
    }
}

pub enum MatchResult {
    NoMatch,
    MultipleMatches { count: usize, positions: Vec<usize> },
    UniqueMatch { 
        position: usize, 
        end_position: usize,
        match_level: MatchLevel,  // Track which level succeeded
        confidence: f32,          // 1.0 = exact, <1.0 = fuzzy
    },
}

pub enum MatchLevel {
    Exact,
    WhitespaceNormalized,
    FlexibleWhitespace,
}
```

#### 3. Enhanced Error Messages

```rust
// Current error:
"String to replace not found in file"

// Enhanced error:
"String not found in file '/path/to/file.rs'

Searched for:
  fn old_function() {
      // code here
  }

Suggestions:
1. The text might have different whitespace or indentation
   Try: file_read with line ranges to see exact formatting
   
2. Similar content found at line 42 (85% match):
   fn  old_function  ( )  {
       //   code here
   }
   
3. If you need to replace all occurrences, use replace_all: true

Tip: Always read the file first for precise edits."
```

#### 4. Success Response with Match Level

```json
{
  "success": true,
  "path": "/path/to/file.rs",
  "command": "str_replace",
  "match_level": "whitespace_normalized",
  "confidence": 0.95,
  "lines_changed": [42, 43, 44, 45],
  "diff": "- old line\n+ new line",
  "warning": "Match required whitespace normalization. Consider reading file first for exact match."
}
```

### 3. Replace All Feature

```rust
// When replace_all: true
fn replace_all(&self, content: &str, old_str: &str, new_str: &str) -> Result<FileEditOutput> {
    let mut count = 0;
    let mut result = content.to_string();
    
    while let Some(pos) = result.find(old_str) {
        result.replace_range(pos..pos+old_str.len(), new_str);
        count += 1;
        if count > 100 {
            return Err(FileEditError::TooManyReplacements(count));
        }
    }
    
    if count == 0 {
        // Try fallback matching strategies
        // ...
    }
    
    Ok(FileEditOutput {
        count,
        warning: Some("Replaced all occurrences. Review changes carefully.".to_string()),
        // ...
    })
}
```

---

## 🏗️ Tool Architecture Analysis

### Current PeakBot Architecture (Separate Tools)

```
┌─────────────────┐     ┌──────────────────┐
│  file_read      │     │   file_edit      │
│  - read         │     │   - create       │
│  - line ranges  │     │   - str_replace  │
│  - pagination   │     │   - insert       │
└─────────────────┘     └──────────────────┘
```

### Anthropic API Architecture (Combined Tool)

```
┌─────────────────────────────────────────┐
│        text_editor (combined)           │
│  - view (with line ranges)              │
│  - str_replace                          │
│  - create                               │
│  - insert                               │
│  - undo_edit                            │
└─────────────────────────────────────────┘
```

### Industry Comparison

| System | Architecture | Commands |
|--------|-------------|----------|
| **Anthropic API** | Combined | view, str_replace, create, insert, undo_edit |
| **Claude Code** | Combined (internal) | Similar to API |
| **Q CLI** | Separate | fs_read, fs_write (create/str_replace/insert/append) |
| **Gemini CLI** | Separate | read-file, write-file, smart-edit |
| **PeakBot** | Separate | file_read, file_edit (create/str_replace/insert) |

### Trade-off Analysis

#### Separate Tools (PeakBot Current)
**Pros:**
- ✅ Single responsibility principle
- ✅ Independent optimization (file_read can have search, pagination)
- ✅ Clearer error boundaries
- ✅ Follows Q CLI and Gemini CLI patterns (proven successful)
- ✅ Easier to add features to individual tools
- ✅ Better for MCP (can expose read-only vs read-write servers)

**Cons:**
- ❌ More tool definitions in context (token overhead)
- ❌ Model must understand tool boundaries
- ❌ Slightly more complex workflow (two tool calls vs one)

#### Combined Tool (Anthropic)
**Pros:**
- ✅ Fewer tools to define (token efficient)
- ✅ Natural workflow (view → edit in same tool)
- ✅ Built-in undo_edit with view history
- ✅ Simpler schema for model to learn

**Cons:**
- ❌ Larger tool definition
- ❌ Mixed concerns (reading vs writing)
- ❌ Harder to optimize independently
- ❌ Less flexible for specialized features

### Recommendation: **Keep Separate Tools**

**Rationale:**
1. **Already working**: PeakBot's current architecture functions well
2. **Industry precedent**: Q CLI and Gemini CLI both use separate tools successfully
3. **Feature richness**: file_read has pagination hints and line ranges that benefit from specialization
4. **MCP flexibility**: Can create read-only MCP servers for safe browsing
5. **Incremental improvement**: Can add "view" command to file_edit later if needed for undo history

**When to Consider Combining:**
- If token overhead becomes a measured problem
- If models consistently confuse when to use which tool
- If undo_edit feature requires tight coupling

---

## 🚫 Preventing Bash/sed Fallback

### Problem
Models fall back to bash/sed when:
1. file_edit fails too often
2. They need global replacement
3. They're more familiar with bash patterns
4. Error messages don't guide them back to the tool

### Solutions

#### 1. Make Tool More Forgiving ✅
- Implement flexible matching (above)
- Add replace_all option
- Better error recovery

#### 2. Enhanced Tool Description

Update the tool's JSON schema description:

```json
{
  "name": "file_edit",
  "description": "Edit files safely with automatic formatting detection. \n\nPREFER THIS TOOL OVER BASH/SED for all file modifications because:\n- Provides clear diffs for review\n- Supports undo via file history\n- Automatically handles whitespace differences\n- Safer: won't accidentally modify wrong files\n- Works across all platforms (sed syntax varies)\n\nUse bash ONLY for: file operations (mv/cp/rm), permissions, bulk operations on many files.\n\nIf editing fails, read the file first to get exact content, then retry.",
  "input_schema": { ... }
}
```

#### 3. System Prompt Guidance

Add to PeakBot's system prompt:

```markdown
## File Editing Best Practices

### ALWAYS Use file_edit Tool For:
- Modifying file content
- Adding/changing/removing code
- Text replacements (single or global)
- Creating new files

### NEVER Use Bash/sed/awk For:
- File content modifications
- Text replacements
- Code edits

### When Bash IS Appropriate:
- File operations: mv, cp, rm, mkdir
- Permission changes: chmod, chown
- Bulk operations on many files (find + xargs)
- System configuration

### If file_edit Fails:
1. Read the file first: `file_read path="..."`
2. Copy the EXACT content you want to replace
3. Include surrounding context (2-3 lines before/after)
4. Retry with exact match
5. If still failing, the error message will suggest alternatives

Remember: file_edit is safer, provides diffs, and works across all platforms.
```

#### 4. Tool Output Reinforcement

After successful edits, include reminders:

```
✅ Successfully edited /path/to/file.rs

Changes:
- Line 42-45: Updated function signature

Tip: For global replacements, use replace_all: true
Tip: If you need to edit multiple files, file_edit is safer than sed
```

#### 5. Bash Tool Guardrails (Optional)

Add detection in bash tool:

```rust
impl BashTool {
    fn call(&self, args: BashArgs) -> Result<BashOutput, BashError> {
        let command = &args.command;
        
        // Warn on common file-editing bash patterns
        if command.contains("sed -i") || 
           command.contains("awk ") && command.contains(">") {
            
            return Ok(BashOutput {
                stdout: String::new(),
                warning: Some(
                    "Consider using file_edit tool instead of sed/awk for file modifications.\n"
                    "file_edit provides: safe diffs, undo support, cross-platform compatibility.\n"
                    "This command will execute, but file_edit is recommended."
                ).to_string(),
                // ...
            });
        }
        
        // Execute normally
        // ...
    }
}
```

---

## 📋 Implementation Priority

### Phase 1: Quick Wins (1-2 days)
- [ ] Add `replace_all` parameter to str_replace
- [ ] Enhance error messages with suggestions
- [ ] Update tool description to discourage bash
- [ ] Add system prompt guidance section

### Phase 2: Flexible Matching (3-5 days)
- [ ] Implement MatchResult enum
- [ ] Add whitespace_normalized_match()
- [ ] Add flexible_whitespace_match()
- [ ] Implement progressive fallback logic
- [ ] Add match_level to success responses
- [ ] Add warnings for non-exact matches

### Phase 3: Advanced Features (Optional)
- [ ] Fuzzy matching with line number suggestions
- [ ] Bash tool guardrails (warnings)
- [ ] Undo history in file_edit
- [ ] "view" command in file_edit for undo support
- [ ] Evaluation suite to measure improvement

### Phase 4: Optimization
- [ ] Performance benchmarking
- [ ] Token usage analysis
- [ ] User feedback collection
- [ ] Consider combining tools if needed

---

## 📊 Success Metrics

### Quantitative
- **Reduction in bash/sed usage**: Track via tool call logs
- **file_edit success rate**: Target >95% (from current ~85%)
- **Retry rate**: Target <10% of edits require multiple attempts
- **Average edits per task**: Should decrease as tool becomes more reliable

### Qualitative
- User reports fewer "string not found" errors
- Models prefer file_edit over bash in observed sessions
- Cleaner diffs and fewer formatting issues
- Better cross-platform compatibility reports

---

## 🔍 Testing Strategy

### Unit Tests
```rust
#[test]
fn test_exact_match() { ... }

#[test]
fn test_whitespace_normalized_match() {
    // Different indentation
    let content = "  fn test() {\n      println!(\"hello\");\n  }";
    let old_str = "fn test() {\n    println!(\"hello\");\n}";
    let result = whitespace_normalized_match(content, old_str);
    assert!(matches!(result, MatchResult::UniqueMatch { .. }));
}

#[test]
fn test_flexible_whitespace_match() {
    // Extra spaces between tokens
    let content = "fn   test  ( )  { }";
    let old_str = "fn test() {}";
    let result = flexible_whitespace_match(content, old_str);
    assert!(matches!(result, MatchResult::UniqueMatch { .. }));
}

#[test]
fn test_replace_all() {
    let content = "fn a() {}\nfn b() {}\nfn a() {}";
    let old_str = "fn a() {}";
    let new_str = "fn a_new() {}";
    let result = replace_all(content, old_str, new_str, true);
    assert_eq!(result.count, 2);
}
```

### Integration Tests
- Real-world code editing scenarios
- Multi-file refactoring tasks
- Comparison: before/after bash usage

### Evaluation Suite
Create benchmark tasks:
1. Simple single-line edits
2. Multi-line function edits
3. Whitespace-sensitive edits (Python, YAML)
4. Global replacements
5. Edits in large files

---

## 📚 References

- [Gemini CLI smart-edit implementation](https://github.com/google-gemini/gemini-cli/blob/main/packages/core/src/tools/smart-edit.ts)
- [Anthropic Text Editor Tool](https://docs.anthropic.com/en/docs/build-with-claude/tool-use/text-editor-tool)
- [Q CLI fs_write](https://github.com/aws/amazon-q-developer-cli/blob/main/crates/chat-cli/src/cli/chat/tools/fs_write.rs)
- [Anthropic: Writing Tools for Agents](https://www.anthropic.com/engineering/writing-tools-for-agents)
- [File Editing for LLMs - Sumit Gouthaman](https://sumitgouthaman.com/posts/file-editing-for-llms/)

---

## 🤔 Open Questions

1. **Match confidence threshold**: At what confidence level should we warn the user?
2. **Regex safety**: How to prevent overly broad flexible matches?
3. **Performance**: What's the acceptable overhead for fallback strategies?
4. **Undo implementation**: Should undo be in file_edit or a separate tool?
5. **MCP considerations**: How does this affect MCP server design?

---

## 📅 Timeline

- **Week 1**: Phase 1 (quick wins) - Immediate bash reduction
- **Week 2-3**: Phase 2 (flexible matching) - Major reliability improvement  
- **Week 4**: Testing and evaluation
- **Week 5+**: Phase 3 (advanced features) - Optional enhancements

---

*Created: 2026-03-18*
*Author: PeakBot (with research from industry best practices)*
*Status: Draft - Ready for Implementation*
