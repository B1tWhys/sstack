---
name: squashing-commits
description: Squash a branch to one commit per user-facing change. Use before landing a Session or merging any branch into main.
---

Squash a branch so that `main` gets one commit per user-facing change: a feature, a fix, a new rule for agents, or anything else a person using or working on sstack would notice. Each change's commit carries everything that serves it, including its tests, docs, ADR, preparatory refactors and review fixes.

## Steps

1. **Read the branch.** Record the tip with `git rev-parse HEAD`, then read every commit with `git log --reverse --format='%h %s%n%n%b' main..HEAD`. If `git branch -r --contains HEAD` lists a remote branch, ask the person before going on, because squashing rewrites history that someone else may have. The step is done when you have the tip and have read every commit on the branch.
2. **Group the commits.** Give every commit to the change it serves. A branch is usually one change, and so one group. Keep two groups apart only when each would make sense landing without the other. The step is done when every commit sits in exactly one group.
3. **Write one message per group.** Use a conventional commit typed by the user-facing change, so a feature with its tests and docs is `feat`, not `test` or `docs`. The subject says what changed for a person. The body says why, drawn from the squashed messages, as prose rather than a list of them. Follow `.agents/skills/writing-prose/SKILL.md`. Save each message to a file outside the Workspace, such as in a directory made with `mktemp -d`.
4. **Squash.**
   - For one group, run `git reset --soft $(git merge-base main HEAD)`, then `git commit -F <message file>`.
   - For several groups, write a rebase todo that lists, for each group in order, `pick` for its first commit, `fixup` for each of the rest, and `exec git commit --amend -F <message file>`. Run it with `GIT_SEQUENCE_EDITOR="cp <todo file>" git rebase -i $(git merge-base main HEAD)`. If reordering conflicts, run `git rebase --abort`, merge the groups that clash into one, and start this step again.

   The hooks run on every new commit. The step is done when every new commit has passed them.
5. **Check.** `git diff <recorded tip> HEAD` prints nothing, and `git log --oneline main..HEAD` shows one commit per group, in order. The step is done when both hold.

The branch is now ready to land. In a Session with its own worktree, call the `land` tool. Otherwise, run `git merge --ff-only <branch>` from the main checkout.
