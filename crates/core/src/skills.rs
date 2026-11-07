//! Finding the [`Skill`]s a person has installed for Claude Code or Codex, so the Harness can use
//! the same ones.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use yaml_rust2::{Yaml, YamlLoader};

/// The file that marks a directory as a Skill and describes it.
const SKILL_FILE: &str = "SKILL.md";

/// How many directories deep below a location the search goes. Visited directories are never
/// searched twice, so this only bounds very deep trees.
const MAX_DEPTH: usize = 8;

/// A Skill that was found, described well enough to tell the person or the model about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// What the Skill is called: its declared name, or its directory's name if it declares none.
    pub name: String,
    /// When to use the Skill, as its author wrote it. Empty if it has none.
    pub description: String,
    /// The Skill's `SKILL.md`, as found, which may run through symlinks.
    pub path: PathBuf,
}

/// Returns where Claude Code and Codex keep Skills, most specific first, so that a project's
/// Skills win over the person's own when names clash.
///
/// `home` is the person's home directory, if known. `env` looks up an environment variable, which
/// lets `CLAUDE_CONFIG_DIR` and `CODEX_HOME` move those tools' directories as they do for the
/// tools themselves. Locations need not exist.
#[must_use]
pub fn locations(
    project: &Path,
    home: Option<&Path>,
    env: impl Fn(&str) -> Option<OsString>,
) -> Vec<PathBuf> {
    let in_home = |name: &str| home.map(|home| home.join(name));
    let config = |variable: &str, default: &str| {
        env(variable)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| in_home(default))
    };
    let user = [
        config("CLAUDE_CONFIG_DIR", ".claude"),
        config("CODEX_HOME", ".codex"),
        in_home(".agents"),
    ];
    [Some(project.join(".claude")), Some(project.join(".agents"))]
        .into_iter()
        .chain(user)
        .flatten()
        .map(|directory| directory.join("skills"))
        .collect()
}

/// Finds every [`Skill`] in `locations`, searching earlier locations first.
///
/// Symlinks are followed, and links that loop back on themselves are harmless. Missing
/// locations, broken links and unreadable files are skipped. A Skill's own subdirectories are not
/// searched for further Skills. When two Skills share a name, only the first found is kept.
#[must_use]
pub fn discover(locations: &[PathBuf]) -> Vec<Skill> {
    let mut search = Search::default();
    for location in locations {
        search.directory(location, 0);
    }
    search.skills
}

/// Remembers what a search has seen, so that nothing is searched or kept twice.
#[derive(Debug, Default)]
struct Search {
    /// Every directory searched so far, with symlinks resolved.
    visited: HashSet<PathBuf>,
    /// The names of the Skills kept so far.
    names: HashSet<String>,
    /// The [`Skill`]s kept so far, in the order they were found.
    skills: Vec<Skill>,
}

impl Search {
    /// Searches `directory`, which lies `depth` directories below a location.
    fn directory(&mut self, directory: &Path, depth: usize) {
        let Ok(real) = directory.canonicalize() else {
            return;
        };
        if depth > MAX_DEPTH || !self.visited.insert(real.clone()) {
            return;
        }
        let file = directory.join(SKILL_FILE);
        if real.join(SKILL_FILE).is_file() {
            self.keep(read_skill(file, directory));
            return;
        }
        for child in subdirectories(&real) {
            self.directory(&directory.join(child), depth + 1);
        }
    }

    /// Keeps `skill`, unless it could not be read or one with its name is already kept.
    fn keep(&mut self, skill: Option<Skill>) {
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

/// Reads the [`Skill`] described by `file`, which lives in `directory`. Returns nothing if the file
/// cannot be read as text.
///
/// A missing or malformed header still gives a Skill, named after its directory.
fn read_skill(file: PathBuf, directory: &Path) -> Option<Skill> {
    let text = fs::read_to_string(&file).ok()?;
    let header = header(&text).unwrap_or(Yaml::Null);
    let field = |key: &str| header[key].as_str().map(|value| value.trim().to_owned());
    let folder = directory.file_name().unwrap_or_default().to_string_lossy();
    Some(Skill {
        name: field("name").unwrap_or_else(|| folder.into_owned()),
        description: field("description").unwrap_or_default(),
        path: file,
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

    /// Returns the names of the Skills found in `locations`, in order.
    fn names(locations: &[PathBuf]) -> Vec<String> {
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
        assert_eq!(discover(&[dir.path().join("missing"), empty]), []);
    }

    #[test]
    fn skills_are_described_by_their_header() {
        let dir = tempfile::tempdir().unwrap();
        let content = "---\nname: review\ndescription: >\n  Use when\n  reviewing.\nallowed-tools: [read]\n---\nBody\n";
        skill_at(&dir.path().join("folder"), content);

        let found = discover(&[dir.path().to_path_buf()]);

        let path = dir.path().join("folder").join(SKILL_FILE);
        let expected = Skill {
            name: "review".into(),
            description: "Use when reviewing.".into(),
            path,
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

        let found = discover(&[dir.path().to_path_buf()]);

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
        assert_eq!(names(&[dir.path().to_path_buf()]), ["fine"]);
    }

    #[test]
    fn nested_skills_are_found_but_not_inside_another_skill() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("synced").join("id").join("outer");
        skill_at(&outer, &described("outer", "o"));
        skill_at(&outer.join("assets"), &described("inner", "i"));
        assert_eq!(names(&[dir.path().to_path_buf()]), ["outer"]);
    }

    #[test]
    fn symlinked_skills_and_locations_are_followed() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere");
        skill_at(&elsewhere.join("linked"), &described("linked", "l"));
        let location = dir.path().join("location");
        fs::create_dir(&location).unwrap();
        symlink(elsewhere.join("linked"), location.join("alias")).unwrap();
        symlink(&location, dir.path().join("location-link")).unwrap();

        let found = discover(&[dir.path().join("location-link")]);

        assert_eq!(found.len(), 1);
        let path = dir.path().join("location-link/alias").join(SKILL_FILE);
        assert_eq!(found[0].path, path);
    }

    #[test]
    fn symlink_loops_end_and_each_skill_is_found_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        skill_at(&root.join("a").join("one"), &described("one", "1"));
        symlink(&root, root.join("a").join("back-to-root")).unwrap();
        symlink(root.join("a"), root.join("a").join("itself")).unwrap();
        symlink(root.join("loop"), root.join("loop")).unwrap();
        symlink(root.join("missing"), root.join("dangling")).unwrap();

        let twice = [root.clone(), root.join("a")];
        assert_eq!(names(&twice), ["one"]);
    }

    /// Returns the directory `depth` levels below `root`.
    fn nested(root: &Path, depth: usize) -> PathBuf {
        (0..depth).fold(root.to_path_buf(), |path, _| path.join("d"))
    }

    #[test]
    fn the_search_stops_at_a_depth_limit() {
        let dir = tempfile::tempdir().unwrap();
        let (kept, skipped) = (dir.path().join("kept"), dir.path().join("skipped"));
        skill_at(&nested(&kept, MAX_DEPTH), &described("deepest", "d"));
        skill_at(
            &nested(&skipped, MAX_DEPTH + 1),
            &described("too-deep", "d"),
        );
        assert_eq!(names(&[kept, skipped]), ["deepest"]);
    }

    #[test]
    fn the_first_skill_found_with_a_name_wins() {
        let dir = tempfile::tempdir().unwrap();
        let (project, user) = (dir.path().join("project"), dir.path().join("user"));
        skill_at(&project.join("review"), &described("review", "project"));
        skill_at(&user.join("review"), &described("review", "user"));
        skill_at(&user.join("other"), &described("other", "user"));

        let found = discover(&[project, user]);

        let found: Vec<_> = found.iter().map(|s| (&*s.name, &*s.description)).collect();
        assert_eq!(found, [("review", "project"), ("other", "user")]);
    }

    #[test]
    fn locations_cover_both_tools_with_the_project_first() {
        let home = Path::new("/home/me");
        let found = locations(Path::new("/code/p"), Some(home), |_| None);
        let expected = [
            "/code/p/.claude/skills",
            "/code/p/.agents/skills",
            "/home/me/.claude/skills",
            "/home/me/.codex/skills",
            "/home/me/.agents/skills",
        ];
        assert_eq!(found, expected.map(PathBuf::from));
    }

    #[test]
    fn the_environment_can_move_each_tools_directory() {
        let env = |name: &str| Some(OsString::from(format!("/env/{name}")));
        let found = locations(Path::new("/p"), None, env);
        let expected = [
            "/p/.claude/skills",
            "/p/.agents/skills",
            "/env/CLAUDE_CONFIG_DIR/skills",
            "/env/CODEX_HOME/skills",
        ];
        assert_eq!(found, expected.map(PathBuf::from));
    }

    #[test]
    fn an_empty_variable_counts_as_unset() {
        let home = Path::new("/h");
        let found = locations(Path::new("/p"), Some(home), |_| Some(OsString::new()));
        assert_eq!(found[2], Path::new("/h/.claude/skills"));
    }
}
