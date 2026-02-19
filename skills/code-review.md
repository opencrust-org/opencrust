---
name: code-review
description: Review code for bugs, security issues, performance problems, and style. Works with files or inline snippets.
triggers:
  - review
  - code review
  - audit
  - check this code
dependencies: []
---

# Code Review

When the user asks you to review code, follow this structured process.

## Get the code

- **File path**: Use `file_read` to read the file.
- **Directory**: Use `bash` to run `find` or `ls`, then read the relevant files.
- **Inline snippet**: Work directly with the code in the message.

## Review checklist

Analyze the code in this order:

### 1. Security
- Hardcoded secrets (API keys, passwords, tokens)
- SQL injection, XSS, command injection vectors
- Unsafe deserialization or eval usage
- Missing input validation on user-facing boundaries
- Overly permissive file/network access

### 2. Correctness
- Off-by-one errors, boundary conditions
- Null/undefined handling (missing checks, unwrap in non-test code)
- Race conditions in concurrent code
- Error handling gaps (swallowed errors, bare `except:`, missing `.catch()`)
- Logic errors in conditionals

### 3. Performance
- Unnecessary allocations in hot paths
- N+1 queries or unbounded iterations
- Missing pagination on list endpoints
- Blocking calls in async contexts

### 4. Style and maintainability
- Dead code or commented-out blocks
- Functions longer than 50 lines
- Magic numbers without named constants
- Missing or misleading names
- Debug print statements left in production code

## Output format

For each finding, report:

```
[SEVERITY] file:line — description
  Suggestion: how to fix it
```

Severity levels:
- **CRITICAL**: Security vulnerability or data loss risk. Fix immediately.
- **BUG**: Will cause incorrect behavior. Should fix before merge.
- **WARNING**: Potential problem or code smell. Should address.
- **NOTE**: Style or minor improvement. Nice to have.

## Rules

- Always read the code before reviewing. Never review code you haven't seen.
- Be specific — reference exact lines and variables, not vague advice.
- If the code looks good, say so. Don't invent issues.
- Limit to the top 5-10 most important findings. Don't overwhelm with nitpicks.
