---
name: git-assist
description: Help with git workflows - generate commit messages from diffs, explain conflicts, suggest commands.
triggers:
  - git help
  - commit message
  - merge conflict
  - git diff
  - rebase
dependencies: []
---

# Git Assist

Help the user with git workflows using the `bash` tool to run git commands.

## Generate commit messages

When the user asks for a commit message:

1. Run `bash` with `git diff --cached` to see staged changes (or `git diff` for unstaged).
2. Analyze what changed - files modified, lines added/removed, the nature of the change.
3. Write a commit message following conventional format:

```
<type>(<scope>): <short description>

<optional body explaining why>
```

Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`

Rules for the message:
- Imperative mood ("add feature" not "added feature")
- Under 72 characters for the subject line
- Explain *why*, not *what* (the diff shows what)

## Explain merge conflicts

When the user has a conflict:

1. Run `git status` to see conflicted files.
2. Use `file_read` on the conflicted file to see the conflict markers.
3. Explain what each side changed and why they conflict.
4. Suggest a resolution (or ask the user which version they prefer).

## Common workflows

### Interactive help
When the user asks "how do I...":
- Check the current state with `git status` and `git log --oneline -5`
- Suggest the specific commands for their situation
- Explain what each command will do before they run it

### Undo mistakes
| Situation | Command |
|-----------|---------|
| Undo last commit (keep changes) | `git reset --soft HEAD~1` |
| Discard unstaged changes in a file | `git checkout -- <file>` |
| Remove file from staging | `git reset HEAD <file>` |
| Undo a pushed commit | `git revert <sha>` |
| Find a lost commit | `git reflog` |

### Branch management
| Task | Command |
|------|---------|
| Create and switch to branch | `git checkout -b <name>` |
| See all branches | `git branch -a` |
| Delete merged branch | `git branch -d <name>` |
| Rebase onto main | `git rebase main` |
| Squash last N commits | `git reset --soft HEAD~N && git commit` |

## Rules

- Always check `git status` before suggesting destructive commands.
- Never suggest `--force` without explaining the risk and suggesting `--force-with-lease`.
- Never suggest `git reset --hard` without warning about data loss.
- If the repo is dirty, mention it before doing anything else.
- Prefer showing the user what will happen (`--dry-run`, `git diff`) before making changes.
