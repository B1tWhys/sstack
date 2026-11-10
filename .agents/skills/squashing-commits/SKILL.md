---
name: squashing-commits
description: Squash a branch to one commit per feature. Use before landing a Session or merging any branch into main.
---

Squash a branch so that `main` gets one commit per feature. A feature is a whole piece of work as the person would name it, such as "add a section to the sidebar", and its commit carries every step of building it, including its tests, docs, ADR, refactors, fixes and review changes. A Session is usually one feature, so its branch usually becomes one commit.

## Steps

1. **Read the branch.** Record the tip with `git rev-parse HEAD`, then read every commit with `git log --reverse --format='%h %s%n%n%b' main..HEAD`. If `git branch -r --contains HEAD` lists a remote branch, ask the person before going on, because squashing rewrites history that someone else may have. The step is done when you have the tip and have read every commit on the branch.
2. **Group the commits.** Start from one group for the whole branch. Split it only where the branch holds unrelated features, each of which the person would ask for on its own, such as a sidebar section and an unrelated fix to the build. The step is done when every commit sits in exactly one group.
3. **Write one message per group.** Use a conventional commit typed by the feature, so a feature with its tests and docs is `feat`, not `test` or `docs`. The subject names the feature. The body says why, drawn from the squashed messages, as prose rather than a list of them. End it with every distinct trailer line of the squashed commits. Follow `.agents/skills/writing-prose/SKILL.md`. Save each message to a file outside the Workspace, such as in a directory made with `mktemp -d`.
4. **Squash.**
   - For one group, run `git reset --soft $(git merge-base main HEAD)`, then `git commit -F <message file>`.
   - For several groups, write a rebase todo that lists, for each group in order, `pick` for its first commit, `fixup` for each of the rest, and `exec git commit --amend -F <message file>`. Run it with `GIT_SEQUENCE_EDITOR="cp <todo file>" git rebase -i $(git merge-base main HEAD)`. If reordering conflicts, run `git rebase --abort`, merge the groups that clash into one, and start this step again.

   The hooks run on every new commit. The step is done when every new commit has passed them.
5. **Check.** `git diff <recorded tip> HEAD` prints nothing, and `git log --oneline main..HEAD` shows one commit per group, in order. The step is done when both hold.

The branch is now ready to land. In a Session with its own worktree, call the `land` tool. Otherwise, run `git merge --ff-only <branch>` from the main checkout.
