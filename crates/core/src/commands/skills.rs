//! The [`Skill`]s a project and a person keep in the `.agents/skills` directories that sstack
//! shares with other agent tools, such as Codex, and how they are found.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use yaml_rust2::{Yaml, YamlLoader};

/// The file that marks a directory as a Skill and describes it.
const SKILL_FILE: &str = "SKILL.md";

/// Where a directory keeps its Skills, relative to that directory.
const SKILLS_DIRECTORY: &str = ".agents/skills";

/// Where a Skill was found, which is what tells apart two Skills that share a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// The root of the project the Session works in.
    Project,
    /// A directory inside the project, given relative to the project's root.
    Nested(PathBuf),
    /// The person's own Skills, which every project sees.
    Personal,
}

impl Scope {
    /// Returns how the Scope is written in front of a Skill's name when the name clashes:
    /// `project`, `personal`, or a nested directory's path from the project's root.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Project => "project".to_owned(),
            Self::Nested(directory) => directory.to_string_lossy().into_owned(),
            Self::Personal => "personal".to_owned(),
        }
    }
}

/// A directory whose subdirectories may be Skills, all of one [`Scope`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// The directory to look in. It need not exist.
    pub directory: PathBuf,
    /// The Skill Scope of every Skill found there.
    pub scope: Scope,
}

/// A Skill that was found, described well enough to tell the person or the model about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// What the Skill is called: its declared name, or its directory's name if it declares none.
    /// When another Skill shares that name, it is written after the Skill's [`Scope::label`] and
    /// a colon, as in `personal:review`.
    pub name: String,
    /// When to use the Skill, as its author wrote it. Empty if it has none.
    pub description: String,
    /// The Skill's `SKILL.md`, as found, which may run through symlinks.
    pub path: PathBuf,
    /// Where the Skill was found.
    pub scope: Scope,
    /// Whether the model may load the Skill on its own. A Skill whose header sets
    /// `disable-model-invocation: true` runs only when a person asks for it by name.
    pub model_invocable: bool,
}

/// Returns where the Skills of a Session working in the project at `root` may be, most specific
/// first.
///
/// The project's locations run from `start` up to `root`, so a person working in a directory
/// inside the project also gets that directory's Skills. A `start` outside `root` counts as
/// `root`. `home` is the person's home directory, if known. Locations need not exist.
#[must_use]
pub fn skill_locations(root: &Path, start: &Path, home: Option<&Path>) -> Vec<Location> {
    let start = if start.starts_with(root) { start } else { root };
    let project = start
        .ancestors()
        .take_while(|directory| directory.starts_with(root))
        .map(|directory| Location {
            directory: directory.join(SKILLS_DIRECTORY),
            scope: scope_of(directory, root),
        });
    let personal = home.map(|home| Location {
        directory: home.join(SKILLS_DIRECTORY),
        scope: Scope::Personal,
    });
    project.chain(personal).collect()
}

/// Returns the Scope of Skills kept in `directory`, which lies inside the project at `root`.
fn scope_of(directory: &Path, root: &Path) -> Scope {
    match directory.strip_prefix(root) {
        Ok(relative) if !relative.as_os_str().is_empty() => Scope::Nested(relative.to_owned()),
        _ => Scope::Project,
    }
}

/// Returns the directory Claude Code keeps the person's files in, if it can be known.
///
/// `home` is the person's home directory, if known. `env` looks up an environment variable, which
/// lets `CLAUDE_CONFIG_DIR` move the directory as it does for Claude Code. The directory need not
/// exist.
#[must_use]
pub fn claude_directory(
    home: Option<&Path>,
    env: impl Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    env("CLAUDE_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|home| home.join(".claude")))
}

/// Finds every [`Skill`] in `locations`, searching earlier locations first.
///
/// Each directory directly inside a location that holds a `SKILL.md` is a Skill, and nothing
/// deeper is searched. Symlinks are followed, and a Skill reached through more than one link is
/// found once. Missing locations, broken links and unreadable files are skipped.
///
/// Skills of different Scopes that share a name are all kept, each named with its Scope, so that
/// no bare name quietly means one of them. Of two Skills with one name in the same Scope, only the
/// first found is kept.
#[must_use]
pub fn discover_skills(locations: &[Location]) -> Vec<Skill> {
    let mut search = Search::default();
    for location in locations {
        for child in subdirectories(&location.directory) {
            search.consider(&location.directory.join(child), &location.scope);
        }
    }
    name_clashes_by_scope(search.skills)
}

/// Writes the Scope in front of the name of every Skill whose name another Skill shares.
fn name_clashes_by_scope(mut skills: Vec<Skill>) -> Vec<Skill> {
    let mut uses: HashMap<String, usize> = HashMap::new();
    for skill in &skills {
        *uses.entry(skill.name.clone()).or_default() += 1;
    }
    for skill in &mut skills {
        if uses[&skill.name] > 1 {
            skill.name = format!("{}:{}", skill.scope.label(), skill.name);
        }
    }
    skills
}

/// What a search has seen so far, so that nothing is kept twice.
#[derive(Debug, Default)]
struct Search {
    /// Every Skill directory kept so far, with symlinks resolved.
    seen: HashSet<PathBuf>,
    /// The Scope label and name of each Skill kept so far.
    names: HashSet<(String, String)>,
    /// The [`Skill`]s kept so far, in the order they were found.
    skills: Vec<Skill>,
}

impl Search {
    /// Keeps the Skill in `directory`, found with `scope`, if it is one and is new.
    fn consider(&mut self, directory: &Path, scope: &Scope) {
        let Ok(real) = directory.canonicalize() else {
            return;
        };
        if !real.join(SKILL_FILE).is_file() || !self.seen.insert(real) {
            return;
        }
        let skill = read_skill(directory.join(SKILL_FILE), directory, scope);
        let new = |skill: &Skill| self.names.insert((skill.scope.label(), skill.name.clone()));
        if let Some(skill) = skill.filter(new) {
            self.skills.push(skill);
        }
    }
}

/// Returns the names of the directories in `directory`, including links to directories, in a
/// stable order. Returns none if it cannot be read.
fn subdirectories(directory: &Path) -> Vec<OsString> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut names: Vec<OsString> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name())
        .collect();
    names.sort();
    names
}

/// Reads the [`Skill`] described by `file`, which lives in `directory` and was found with
/// `scope`. Returns nothing if the file cannot be read as text.
///
/// A missing or malformed header still gives a Skill, named after its directory.
fn read_skill(file: PathBuf, directory: &Path, scope: &Scope) -> Option<Skill> {
    let text = fs::read_to_string(&file).ok()?;
    let header = header(&text).unwrap_or(Yaml::Null);
    let field = |key: &str| header[key].as_str().map(|value| value.trim().to_owned());
    let folder = directory.file_name().unwrap_or_default().to_string_lossy();
    Some(Skill {
        name: field("name").unwrap_or_else(|| folder.into_owned()),
        description: field("description").unwrap_or_default(),
        path: file,
        scope: scope.clone(),
        model_invocable: header["disable-model-invocation"].as_bool() != Some(true),
    })
}

/// Returns the instructions in the text of a `SKILL.md`, without the header that describes the
/// Skill.
#[must_use]
pub fn instructions(text: &str) -> &str {
    split_header(text).1.trim_start_matches('\n')
}

/// Parses the YAML header that opens `text`, if there is a valid one.
fn header(text: &str) -> Option<Yaml> {
    YamlLoader::load_from_str(split_header(text).0?).ok()?.pop()
}

/// Splits `text` into the header between its opening `---` lines, if it has one, and the rest.
/// A header that is never closed counts as part of the rest.
fn split_header(text: &str) -> (Option<&str>, &str) {
    let Some(first_end) = text.find('\n') else {
        return (None, text);
    };
    if text[..first_end].trim_end() != "---" {
        return (None, text);
    }
    let start = first_end + 1;
    let mut end = start;
    for line in text[start..].split_inclusive('\n') {
        if line.trim_end() == "---" {
            return (Some(&text[start..end]), &text[end + line.len()..]);
        }
        end += line.len();
    }
    (None, text)
}

/// The most files beside a `SKILL.md` that [`load_skill`] names, so that a Skill with a large tree of
/// resources does not flood the model.
const MAX_LISTED_FILES: usize = 50;

/// Returns what the model is given when `skill` is loaded: its instructions, the directory they
/// live in, and the files beside them, which the model can then read.
///
/// # Errors
/// Returns an error if the Skill's `SKILL.md` can no longer be read.
pub fn load_skill(skill: &Skill) -> io::Result<String> {
    let text = fs::read_to_string(&skill.path)?;
    let directory = skill.path.parent().unwrap_or(Path::new(""));
    Ok(loaded(skill, directory, &text))
}

/// What a person's request for a Skill by name found.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup<'a> {
    /// The one Skill with that name.
    Found(&'a Skill),
    /// The full names of the Skills of different Scopes that share the bare name asked for.
    Ambiguous(Vec<&'a str>),
    /// No Skill has the name.
    Missing,
}

/// Finds the Skill in `skills` that `name` asks for. A bare name that several Skills share, which
/// [`discover_skills`] then names with their Scopes, finds none of them.
#[must_use]
pub fn find_skill<'a>(skills: &'a [Skill], name: &str) -> Lookup<'a> {
    if let Some(skill) = skills.iter().find(|skill| skill.name == name) {
        return Lookup::Found(skill);
    }
    let sharing: Vec<&str> = skills
        .iter()
        .filter(|skill| skill.name == format!("{}:{name}", skill.scope.label()))
        .map(|skill| skill.name.as_str())
        .collect();
    if sharing.is_empty() {
        Lookup::Missing
    } else {
        Lookup::Ambiguous(sharing)
    }
}

/// Describes `skill` to the model once loaded: its instructions from `text`, then the files in
/// its `directory` other than `SKILL.md`, if there are any.
fn loaded(skill: &Skill, directory: &Path, text: &str) -> String {
    let mut out = format!(
        "<skill name=\"{}\" directory=\"{}\">\n{}\n</skill>",
        skill.name,
        directory.display(),
        instructions(text).trim_end()
    );
    let files = files_beside(directory, &skill.path);
    if files.is_empty() {
        return out;
    }
    let mut lines: Vec<String> = files
        .iter()
        .take(MAX_LISTED_FILES)
        .map(|file| format!("- {}", file.display()))
        .collect();
    if files.len() > MAX_LISTED_FILES {
        lines.push(format!("- and {} more", files.len() - MAX_LISTED_FILES));
    }
    out.push_str("\n\nFiles beside the instructions:\n");
    out.push_str(&lines.join("\n"));
    out
}

/// Returns the files under `directory` other than `instructions`, in a stable order, leaving out
/// hidden and ignored files.
fn files_beside(directory: &Path, instructions: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = ignore::WalkBuilder::new(directory)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(ignore::DirEntry::into_path)
        .filter(|path| path != instructions)
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// Creates a Skill directory at `directory` whose `SKILL.md` holds `content`.
    fn skill_at(directory: &Path, content: &str) {
        fs::create_dir_all(directory).unwrap();
        fs::write(directory.join(SKILL_FILE), content).unwrap();
    }

    /// Returns a `SKILL.md` declaring `name` and `description`.
    fn described(name: &str, description: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n---\n\n# Body\n")
    }

    /// Returns a project-wide location at `directory`.
    fn project(directory: &Path) -> Location {
        Location {
            directory: directory.to_path_buf(),
            scope: Scope::Project,
        }
    }

    /// Returns the names of the Skills found in `locations`, in order.
    fn names(locations: &[Location]) -> Vec<String> {
        discover_skills(locations)
            .into_iter()
            .map(|skill| skill.name)
            .collect()
    }

    #[test]
    fn nothing_is_found_where_nothing_is_installed() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        fs::create_dir(&empty).unwrap();
        assert_eq!(
            discover_skills(&[project(&dir.path().join("missing")), project(&empty)]),
            []
        );
    }

    #[test]
    fn skills_are_described_by_their_header() {
        let dir = tempfile::tempdir().unwrap();
        let content = "---\nname: review\ndescription: >\n  Use when\n  reviewing.\nallowed-tools: [read]\n---\nBody\n";
        skill_at(&dir.path().join("folder"), content);

        let found = discover_skills(&[project(dir.path())]);

        let expected = Skill {
            name: "review".into(),
            description: "Use when reviewing.".into(),
            path: dir.path().join("folder").join(SKILL_FILE),
            scope: Scope::Project,
            model_invocable: true,
        };
        assert_eq!(found, [expected]);
    }

    #[test]
    fn a_skill_without_a_usable_header_is_named_after_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        skill_at(&dir.path().join("plain"), "# No header\n");
        skill_at(&dir.path().join("broken"), "---\nname: [unclosed\n---\n");
        skill_at(&dir.path().join("unended"), "---\nname: never\n");
        skill_at(&dir.path().join("listed"), "---\n- a list\n---\n");

        let found = discover_skills(&[project(dir.path())]);

        let found: Vec<_> = found.iter().map(|s| (&*s.name, &*s.description)).collect();
        let bare = ["broken", "listed", "plain", "unended"].map(|name| (name, ""));
        assert_eq!(found, bare);
    }

    #[test]
    fn a_skill_can_be_kept_from_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let header = "---\nname: by-hand\ndescription: d\ndisable-model-invocation: true\n---\n";
        skill_at(&dir.path().join("by-hand"), header);
        skill_at(&dir.path().join("open"), &described("open", "o"));

        let found = discover_skills(&[project(dir.path())]);

        let flags: Vec<_> = found
            .iter()
            .map(|s| (&*s.name, s.model_invocable))
            .collect();
        assert_eq!(flags, [("by-hand", false), ("open", true)]);
    }

    #[test]
    fn instructions_leave_out_the_header() {
        assert_eq!(instructions(&described("a", "b")), "# Body\n");
        assert_eq!(instructions("# No header\n"), "# No header\n");
        assert_eq!(instructions("---\nname: never\n"), "---\nname: never\n");
    }

    #[test]
    fn unreadable_skills_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("binary");
        fs::create_dir(&binary).unwrap();
        fs::write(binary.join(SKILL_FILE), [0xff, 0xfe, 0x00]).unwrap();
        skill_at(&dir.path().join("fine"), &described("fine", "ok"));
        assert_eq!(names(&[project(dir.path())]), ["fine"]);
    }

    #[test]
    fn only_directories_directly_inside_a_location_are_skills() {
        let dir = tempfile::tempdir().unwrap();
        skill_at(&dir.path().join("synced/id/deep"), &described("deep", "d"));
        let outer = dir.path().join("outer");
        skill_at(&outer, &described("outer", "o"));
        skill_at(&outer.join("assets"), &described("inner", "i"));
        assert_eq!(names(&[project(dir.path())]), ["outer"]);
    }

    #[test]
    fn symlinked_skills_are_followed_and_found_once() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere/linked");
        skill_at(&target, &described("linked", "l"));
        let (first, second) = (dir.path().join("first"), dir.path().join("second"));
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        symlink(&target, first.join("alias")).unwrap();
        symlink(&target, second.join("alias")).unwrap();
        symlink(first.join("loop"), first.join("loop")).unwrap();
        symlink(first.join("missing"), first.join("dangling")).unwrap();

        let found = discover_skills(&[project(&first), project(&second)]);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, first.join("alias").join(SKILL_FILE));
    }

    #[test]
    fn skills_that_share_a_name_are_each_named_with_their_scope() {
        let dir = tempfile::tempdir().unwrap();
        let (mine, inner, theirs) = (
            dir.path().join("project"),
            dir.path().join("nested"),
            dir.path().join("user"),
        );
        skill_at(&inner.join("review"), &described("review", "nested"));
        skill_at(&mine.join("review"), &described("review", "project"));
        skill_at(&theirs.join("other"), &described("other", "user"));
        skill_at(&theirs.join("review"), &described("review", "user"));
        let nested = Location {
            directory: inner,
            scope: Scope::Nested("crates/cli".into()),
        };
        let personal = Location {
            directory: theirs,
            scope: Scope::Personal,
        };

        let found = names(&[nested, project(&mine), personal]);

        let expected = [
            "crates/cli:review",
            "project:review",
            "other",
            "personal:review",
        ];
        assert_eq!(found, expected);
    }

    #[test]
    fn of_two_skills_with_one_name_in_a_scope_the_first_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        skill_at(&dir.path().join("a"), &described("review", "first"));
        skill_at(&dir.path().join("b"), &described("review", "second"));

        let found = discover_skills(&[project(dir.path())]);

        let found: Vec<_> = found.iter().map(|s| (&*s.name, &*s.description)).collect();
        assert_eq!(found, [("review", "first")]);
    }

    #[test]
    fn a_long_list_of_files_is_cut_short() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("SKILL.md");
        for index in 0..=MAX_LISTED_FILES {
            fs::write(dir.path().join(format!("{index:03}.md")), "").unwrap();
        }

        let many = Skill {
            name: "many".into(),
            description: String::new(),
            path,
            scope: Scope::Project,
            model_invocable: true,
        };
        let text = loaded(&many, dir.path(), "Many.");

        assert!(text.ends_with("\n- and 1 more"), "{text}");
    }

    #[test]
    fn a_name_finds_its_skill_and_a_shared_bare_name_finds_none() {
        let skill = |name: &str, scope: Scope| Skill {
            name: name.into(),
            description: String::new(),
            path: "SKILL.md".into(),
            scope,
            model_invocable: true,
        };
        let skills = [
            skill("project:review", Scope::Project),
            skill("personal:review", Scope::Personal),
            skill("quiz", Scope::Personal),
        ];

        assert_eq!(find_skill(&skills, "quiz"), Lookup::Found(&skills[2]));
        assert_eq!(
            find_skill(&skills, "personal:review"),
            Lookup::Found(&skills[1])
        );
        let both = vec!["project:review", "personal:review"];
        assert_eq!(find_skill(&skills, "review"), Lookup::Ambiguous(both));
        assert_eq!(find_skill(&skills, "nope"), Lookup::Missing);
    }

    #[test]
    fn locations_run_from_the_start_up_to_the_root_then_the_person() {
        let root = Path::new("/code/p");
        let found = skill_locations(root, &root.join("crates/cli"), Some(Path::new("/home/me")));
        let expected = [
            (
                "/code/p/crates/cli/.agents/skills",
                Scope::Nested("crates/cli".into()),
            ),
            (
                "/code/p/crates/.agents/skills",
                Scope::Nested("crates".into()),
            ),
            ("/code/p/.agents/skills", Scope::Project),
            ("/home/me/.agents/skills", Scope::Personal),
        ];
        let expected = expected.map(|(directory, scope)| Location {
            directory: directory.into(),
            scope,
        });
        assert_eq!(found, expected);
    }

    #[test]
    fn a_start_outside_the_project_counts_as_its_root() {
        let found = skill_locations(Path::new("/code/p"), Path::new("/elsewhere"), None);
        assert_eq!(found, [project(Path::new("/code/p/.agents/skills"))]);
    }

    #[test]
    fn claude_directory_follows_the_environment_then_the_home_directory() {
        let moved = |_: &str| Some(OsString::from("/env"));
        let home = Some(Path::new("/h"));
        assert_eq!(claude_directory(home, moved), Some("/env".into()));
        assert_eq!(claude_directory(home, |_| None), Some("/h/.claude".into()));
        assert_eq!(claude_directory(None, |_| None), None);
        let empty = |_: &str| Some(OsString::new());
        assert_eq!(claude_directory(home, empty), Some("/h/.claude".into()));
    }
}
