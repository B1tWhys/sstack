# Skills come from the shared `.agents` locations and are listed in the system prompt

sstack finds Skills only in `.agents/skills` directories: one in each directory from the working directory up to the repository root, and the person's `~/.agents/skills`. It looks one level deep and follows symlinks. Codex reads the same places. Claude Code reads `.claude/skills` instead, so a person or a project bridges the two with one symlink, `.claude/skills -> ../.agents/skills`, as this repository does.

We rejected also reading Claude Code's and Codex's own directories. Claude Code writes the Skills of a person's claude.ai account into `~/.claude/skills`, and many of those describe tools sstack does not have, such as Claude's browser. Filtering them by name would break with each sync, and their `compatibility` field is free text that only some of them set. We also rejected reading Claude Code plugins, which only Claude Code installs. A person who wants a plugin's Skill copies it into `~/.agents/skills`.

The system prompt lists each Skill the model may use by name and description, and the `skill` tool loads one by name. The Agent Skills spec, Codex and Gemini CLI work the same way, and a listing fixed for the whole Session keeps the prompt cacheable. We considered listing Skills in the tool's description instead, which gains nothing over the system prompt, and retrieving the relevant Skills on each Turn, which no harness we looked at ships.

The tool's `name` accepts only listed names. It returns the body without its header, together with the Skill's directory and the files in it, and the `read` tool may read inside that directory, so a Skill can keep reference files beside its `SKILL.md`. Skills that set `disable-model-invocation` stay out of the listing. The listing is capped at about 1% of the Connection's context limit. Past the cap, sstack drops descriptions until only names remain and tells the person.

Claude Code runs the person's Skill when names clash and Codex lists both, so neither behaviour can be relied on. When two Skills share a name, sstack renames both with their Skill Scope: `project:`, `personal:`, or a nested directory's path from the repository root, as in `crates/cli:deploy`. Skills without a clash keep their bare names. We rejected keeping the bare name for the project's Skill, because the bare name would then quietly mean one of two Skills. We rejected the repository's directory name as the prefix, because personal Skills have none and it depends on where someone cloned the repository.

A person runs a Skill by typing `/` and its name, optionally followed by arguments, and sstack sends the Skill's body to the model with the arguments appended. This is not a Human Command, which never reaches the model, so Human Commands match first. A bare name that clashes is refused with a notice naming both full names.

## Consequences

A person whose Skills live only in `~/.claude/skills` gets none in sstack until they add the symlink.

A Skill's name changes when a clashing Skill appears, so a document that must reach one particular Skill points at its `SKILL.md` by path.

The listing is built when a Session starts, so a Skill added later appears in the next Session. If sstack later updates the listing within a Session, the update has to be appended to the Session, because ADR 0002 never rewrites entries.

When sstack gains compaction, the body of a loaded Skill has to survive it.
