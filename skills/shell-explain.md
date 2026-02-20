---
name: shell-explain
description: Explain shell commands in plain English. Flag dangerous flags, suggest safer alternatives.
triggers:
  - explain command
  - what does this do
  - what does this command do
  - is this safe
dependencies: []
---

# Shell Explain

When the user pastes a shell command and asks what it does, break it down clearly.

## How to explain

1. **One-line summary**: What the command does in plain English.
2. **Component breakdown**: Explain each part (command, flags, arguments, pipes, redirects).
3. **Risk assessment**: Flag anything dangerous.

## Example

User: `find / -name "*.log" -mtime +30 -exec rm {} \;`

Response:
> **Deletes all `.log` files older than 30 days from the entire filesystem.**
>
> - `find /` - search starting from root (entire filesystem)
> - `-name "*.log"` - match files ending in `.log`
> - `-mtime +30` - only files modified more than 30 days ago
> - `-exec rm {} \;` - delete each matched file
>
> **Warning**: Running from `/` as root will search every mounted filesystem. Consider scoping to a specific directory. Use `-exec rm -i {} \;` to confirm each deletion, or preview first with `find / -name "*.log" -mtime +30 -print`.

## Dangerous patterns to always flag

| Pattern | Risk | Safer alternative |
|---------|------|-------------------|
| `rm -rf /` or `rm -rf *` | Deletes everything | Scope the path, add `--interactive` |
| `chmod -R 777` | World-writable permissions | Use specific permissions (755, 644) |
| `> file` (redirect to existing file) | Overwrites without warning | Use `>>` to append, or backup first |
| `curl ... \| sh` | Executes unreviewed remote code | Download first, review, then execute |
| `dd if=... of=/dev/...` | Overwrites disk directly | Triple-check the `of=` device |
| `:(){:\|:&};:` | Fork bomb - crashes the system | Never run this |
| `git push --force` | Overwrites remote history | Use `--force-with-lease` |
| `kill -9` | No graceful shutdown | Try `kill` (SIGTERM) first |
| `sudo` anything | Elevated privileges | Verify the command is correct before adding sudo |

## Rules

- Always explain what the command does before flagging risks.
- If you don't recognize a command, say so. Don't guess.
- For complex pipelines, explain each stage of the pipe separately.
- If the user asks "is this safe?", lead with the risk assessment.
- Suggest the safer alternative, not just "don't do this."
