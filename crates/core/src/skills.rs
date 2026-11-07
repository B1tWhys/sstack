//! Finding the [`Skill`]s that a project and a person keep in the `.agents/skills` directories
//! sstack shares with other agent tools, such as Codex.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
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
    pub name: String,
    /// When to use the Skill, as its author wrote it. Empty if it has none.
    pub description: String,
    /// The Skill's `SKILL.md`, as found, which may run through symlinks.
    pub path: PathBuf,
    /// Where the Skill was found.
    pub scope: Scope,
}

/// Returns where the Skills of a Session working in the project at `root` may be, most specific
/// first, so that the project's Skills win over the person's own when names clash.
///
/// The project's locations run from `start` up to `root`, so a person working in a directory
/// inside the project also gets that directory's Skills. A `start` outside `root` counts as
/// `root`. `home` is the person's home directory, if known. Locations need not exist.
#[must_use]
pub fn locations(root: &Path, start: &Path, home: Option<&Path>) -> Vec<Location> {
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
/// found once. Missing locations, broken links and unreadable files are skipped. When two Skills
/// share a name, only the first found is kept.
#[must_use]
pub fn discover(locations: &[Location]) -> Vec<Skill> {
    let mut search = Search::default();
    for location in locations {
        for child in subdirectories(&location.directory) {
            search.consider(&location.directory.join(child), &location.scope);
        }
    }
    search.skills
}

/// Remembers what a search has seen, so that nothing is kept twice.
#[derive(Debug, Default)]
struct Search {
    /// Every Skill directory kept so far, with symlinks resolved.
    seen: HashSet<PathBuf>,
    /// The names of the Skills kept so far.
    names: HashSet<String>,
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
        if let Some(skill) = skill.filter(|skill| self.names.insert(skill.name.clone())) {
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
    })
}

/// Parses the YAML header that opens `text` between `---` lines, if there is a valid one.
fn header(text: &str) -> Option<Yaml> {
    let mut lines = text.split_inclusive('\n');
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let start = text.find('\n')? + 1;
    let mut end = start;
    for line in lines {
        if line.trim_end() == "---" {
            return YamlLoader::load_from_str(&text[start..end]).ok()?.pop();
        }
        end += line.len();
    }
    None
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
        discover(locations)
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
            discover(&[project(&dir.path().join("missing")), project(&empty)]),
            []
        );
    }

    #[test]
    fn skills_are_described_by_their_header() {
        let dir = tempfile::tempdir().unwrap();
        let content = "---\nname: review\ndescription: >\n  Use when\n  reviewing.\nallowed-tools: [read]\n---\nBody\n";
        skill_at(&dir.path().join("folder"), content);

        let found = discover(&[project(dir.path())]);

        let expected = Skill {
            name: "review".into(),
            description: "Use when reviewing.".into(),
            path: dir.path().join("folder").join(SKILL_FILE),
            scope: Scope::Project,
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

        let found = discover(&[project(dir.path())]);

        let found: Vec<_> = found.iter().map(|s| (&*s.name, &*s.description)).collect();
        let bare = ["broken", "listed", "plain", "unended"].map(|name| (name, ""));
        assert_eq!(found, bare);
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

        let found = discover(&[project(&first), project(&second)]);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, first.join("alias").join(SKILL_FILE));
    }

    #[test]
    fn the_first_skill_found_with_a_name_wins() {
        let dir = tempfile::tempdir().unwrap();
        let (mine, theirs) = (dir.path().join("project"), dir.path().join("user"));
        skill_at(&mine.join("review"), &described("review", "project"));
        skill_at(&theirs.join("review"), &described("review", "user"));
        skill_at(&theirs.join("other"), &described("other", "user"));
        let personal = Location {
            directory: theirs,
            scope: Scope::Personal,
        };

        let found = discover(&[project(&mine), personal]);

        let found: Vec<_> = found.iter().map(|s| (&*s.name, &s.scope)).collect();
        assert_eq!(
            found,
            [("review", &Scope::Project), ("other", &Scope::Personal)]
        );
    }

    #[test]
    fn locations_run_from_the_start_up_to_the_root_then_the_person() {
        let root = Path::new("/code/p");
        let found = locations(root, &root.join("crates/cli"), Some(Path::new("/home/me")));
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
        let found = locations(Path::new("/code/p"), Path::new("/elsewhere"), None);
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
