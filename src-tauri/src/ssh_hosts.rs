//! The `Host` aliases of the user's SSH configuration, for the manage dialog
//! to offer. Reads the configuration files and what they include, and gives
//! back names alone: no host, user, port or key path leaves this file.
use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
};

const SYSTEM: &str = "/etc/ssh/ssh_config";
/// How far an `Include` may nest below the file that was opened first.
const DEPTH: usize = 8;
/// How many files all the `Include`s together may bring in.
const INCLUDED: usize = 256;
/// A larger file is passed over.
const SIZE: u64 = 1 << 20;

/// The aliases `ssh` would take as a server of this user, in the order the
/// configuration names them. A file that is absent or unreadable adds none.
pub fn list(home: &Path) -> Vec<String> {
    list_with(home, Some(Path::new(SYSTEM)))
}

fn list_with(home: &Path, system: Option<&Path>) -> Vec<String> {
    let mut scan = Scan {
        home,
        seen: HashSet::new(),
        included: 0,
        aliases: Vec::new(),
    };
    let user = home.join(".ssh");
    scan.file(&user.join("config"), &user, 0);
    if let Some(system) = system {
        scan.file(system, system.parent().unwrap_or(Path::new("/")), 0);
    }
    scan.aliases
}

struct Scan<'a> {
    home: &'a Path,
    /// Every file read so far, so that one including itself ends.
    seen: HashSet<PathBuf>,
    included: usize,
    aliases: Vec<String>,
}

impl Scan<'_> {
    /// `base` is where a relative `Include` starts: the directory of the
    /// file opened first, whichever file names it.
    fn file(&mut self, path: &Path, base: &Path, depth: usize) {
        let Ok(path) = fs::canonicalize(path) else {
            return;
        };
        if self.seen.contains(&path) || (depth > 0 && self.included == INCLUDED) {
            return;
        }
        // A pipe or a device is never opened.
        if !fs::metadata(&path).is_ok_and(|file| file.is_file() && file.len() <= SIZE) {
            return;
        }
        let Ok(bytes) = fs::read(&path) else {
            return;
        };
        self.seen.insert(path);
        if depth > 0 {
            self.included += 1;
        }
        for line in String::from_utf8_lossy(&bytes).lines() {
            let Some((keyword, arguments)) = statement(line) else {
                continue;
            };
            // A `Match` block names no alias, and an `Include` inside one
            // is read like any other.
            if keyword.eq_ignore_ascii_case("host") {
                for alias in arguments {
                    // A pattern or a negation never passes the rule.
                    if crate::remote::check_alias(&alias).is_ok() && !self.aliases.contains(&alias)
                    {
                        self.aliases.push(alias);
                    }
                }
            } else if keyword.eq_ignore_ascii_case("include") && depth < DEPTH {
                for argument in arguments {
                    for found in expand(&self.resolve(&argument, base)) {
                        self.file(&found, base, depth + 1);
                    }
                }
            }
        }
    }

    fn resolve(&self, argument: &str, base: &Path) -> PathBuf {
        match argument.strip_prefix("~/") {
            Some(rest) => self.home.join(rest),
            // Joining an absolute path gives that path.
            None => base.join(argument),
        }
    }
}

/// The keyword of a line and its arguments, unless it is blank or a comment.
/// The two are separated by blanks, by `=` or by both.
fn statement(line: &str) -> Option<(&str, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let (keyword, rest) = line.split_at(end);
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest);
    Some((keyword, arguments(rest)))
}

/// Splits on blanks outside quotes and drops the quotes. A `#` where an
/// argument would start ends the line.
fn arguments(rest: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut chars = rest.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        if matches!(chars.peek(), None | Some('#')) {
            return found;
        }
        let mut argument = String::new();
        let mut quote = None;
        while let Some(c) = chars.next_if(|c| quote.is_some() || !c.is_whitespace()) {
            if quote == Some(c) {
                quote = None;
            } else if quote.is_none() && matches!(c, '"' | '\'') {
                quote = Some(c);
            } else {
                argument.push(c);
            }
        }
        found.push(argument);
    }
}

/// The paths a pattern stands for, in name order. `*` and `?` are expanded
/// within each part of the path, and a part without them is kept as it is.
fn expand(pattern: &Path) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::new()];
    for part in pattern.components() {
        let name = match part {
            Component::Normal(name) => name.to_string_lossy(),
            other => {
                paths.iter_mut().for_each(|path| path.push(other));
                continue;
            }
        };
        if !name.contains(['*', '?']) {
            paths.iter_mut().for_each(|path| path.push(&*name));
            continue;
        }
        let wanted: Vec<char> = name.chars().collect();
        let mut next = Vec::new();
        for path in &paths {
            let Ok(entries) = fs::read_dir(path) else {
                continue;
            };
            let mut names: Vec<_> = entries
                .flatten()
                .map(|entry| entry.file_name())
                .filter(|name| {
                    let name: Vec<char> = name.to_string_lossy().chars().collect();
                    // As in a shell, a wildcard does not reach a hidden name.
                    (name.first() != Some(&'.') || wanted[0] == '.') && matches(&wanted, &name)
                })
                .collect();
            names.sort();
            next.extend(names.into_iter().map(|name| path.join(name)));
        }
        // More than can be read is not carried further.
        next.truncate(INCLUDED);
        paths = next;
    }
    paths
}

/// Whether a name is what a pattern of `*` (any run) and `?` (any one
/// character) describes.
fn matches(pattern: &[char], name: &[char]) -> bool {
    let (mut p, mut n) = (0, 0);
    // Where the last `*` was, and how much of the name it has taken.
    let mut star = None;
    while n < name.len() {
        if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, n));
            p += 1;
        } else if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if let Some((at, taken)) = star {
            star = Some((at, taken + 1));
            p = at + 1;
            n = taken + 1;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A home whose `.ssh` holds the given files, by path below it.
    fn home(files: &[(&str, &str)]) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        for (name, text) in files {
            write(&home.path().join(".ssh").join(name), text);
        }
        home
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn user(config: &str) -> Vec<String> {
        list_with(home(&[("config", config)]).path(), None)
    }

    #[test]
    fn one_line_names_several_aliases_and_only_the_names_are_kept() {
        assert_eq!(
            user(
                "# servers\n\nHost tokyo osaka\n  HostName example.invalid\n  User root\n  Port 2222\n  IdentityFile ~/.ssh/id_example\nHost local\n  HostName 127.0.0.1\n"
            ),
            ["tokyo", "osaka", "local"]
        );
    }

    #[test]
    fn the_keyword_is_read_in_any_case_and_with_an_equals_sign() {
        assert_eq!(
            user("host a\nHOST b\n\tHoSt   c\nHost=d\nHost = e f\nHost =g\nHostName example.invalid\nHostKeyAlias h\n"),
            ["a", "b", "c", "d", "e", "f", "g"]
        );
    }

    #[test]
    fn quotes_are_dropped_and_a_trailing_comment_ends_the_line() {
        assert_eq!(
            user("Host \"tokyo\" 'osaka' \"ky\"oto # nara\nHost \"two words\"\n#Host hidden\n  # Host indented\n"),
            ["tokyo", "osaka", "kyoto"]
        );
    }

    #[test]
    fn patterns_and_negations_are_left_out() {
        assert_eq!(
            user("Host *\nHost *.example.invalid web-? !bastion plain\nHost 10.0.0.*\n"),
            ["plain"]
        );
    }

    #[test]
    fn an_alias_the_channel_would_refuse_is_left_out() {
        let long = "a".repeat(256);
        assert_eq!(
            user(&format!(
                "Host -oProxyCommand=x user@host a/b 东京 {long} fine_1.example-2\n"
            )),
            ["fine_1.example-2"]
        );
    }

    #[test]
    fn aliases_keep_the_order_they_first_appear_in_and_appear_once() {
        assert_eq!(
            user("Host zulu alpha\nHost mike zulu\nHost alpha Alpha\n"),
            ["zulu", "alpha", "mike", "Alpha"]
        );
    }

    #[test]
    fn a_match_block_names_no_alias_and_its_include_is_still_read() {
        let home = home(&[
            (
                "config",
                "Host before\nMatch host inside.example.invalid user root\n  Include matched\nMatch all\nHost after\n",
            ),
            ("matched", "Host included\n"),
        ]);
        assert_eq!(
            list_with(home.path(), None),
            ["before", "included", "after"]
        );
    }

    #[test]
    fn an_include_is_read_where_it_stands_from_home_and_from_the_ssh_directory() {
        let home = home(&[
            (
                "config",
                "Host first\nInclude ~/elsewhere/hosts conf.d/one \"conf.d/with space\" missing\nInclude=conf.d/two\nHost last\n",
            ),
            ("conf.d/one", "Host one\n"),
            ("conf.d/with space", "Host spaced\n"),
            ("conf.d/two", "Host two\n"),
        ]);
        write(&home.path().join("elsewhere/hosts"), "Host elsewhere\n");
        assert_eq!(
            list_with(home.path(), None),
            ["first", "elsewhere", "one", "spaced", "two", "last"]
        );
    }

    #[test]
    fn an_include_expands_wildcards_in_every_part_of_the_path() {
        let home = home(&[
            (
                "config",
                "Include conf.d/*.conf team-?/hosts ~/.ssh/n*/*/c\n",
            ),
            ("conf.d/b.conf", "Host b\n"),
            ("conf.d/a.conf", "Host a\n"),
            ("conf.d/.hidden.conf", "Host hidden\n"),
            ("conf.d/c.conf.bak", "Host backup\n"),
            ("team-1/hosts", "Host one\n"),
            ("team-2/hosts", "Host two\n"),
            ("team-10/hosts", "Host ten\n"),
            ("nested/x/c", "Host deep\n"),
            ("nested/x/d", "Host other\n"),
        ]);
        assert_eq!(
            list_with(home.path(), None),
            ["a", "b", "one", "two", "deep"]
        );
    }

    #[test]
    fn wildcards_match_as_a_shell_would() {
        let is = |pattern: &str, name: &str| {
            matches(
                &pattern.chars().collect::<Vec<_>>(),
                &name.chars().collect::<Vec<_>>(),
            )
        };
        assert!(is("*", ""));
        assert!(is("*", "anything"));
        assert!(is("a*c", "ac"));
        assert!(is("a*c", "abbc"));
        assert!(is("a*b*c", "a-b-b-c"));
        assert!(is("?.conf", "a.conf"));
        assert!(is("*.conf", "东京.conf"));
        assert!(!is("?.conf", ".conf"));
        assert!(!is("a*c", "abcd"));
        assert!(!is("a?c", "ac"));
        assert!(!is("", "a"));
    }

    #[test]
    fn files_that_include_each_other_are_each_read_once() {
        let home = home(&[
            ("config", "Host a\nInclude config loop\nHost c\n"),
            ("loop", "Host b\nInclude config loop ~/.ssh/./loop\n"),
        ]);
        assert_eq!(list_with(home.path(), None), ["a", "b", "c"]);
    }

    #[test]
    fn an_include_nests_only_so_deep() {
        let mut files: Vec<(String, String)> = (1..=DEPTH + 1)
            .map(|level| {
                (
                    format!("level{level}"),
                    format!("Host host{level}\nInclude level{}\n", level + 1),
                )
            })
            .collect();
        files.push(("config".into(), "Include level1\n".into()));
        let files: Vec<_> = files
            .iter()
            .map(|(name, text)| (name.as_str(), text.as_str()))
            .collect();
        let found = list_with(home(&files).path(), None);
        assert_eq!(found.len(), DEPTH);
        assert_eq!(found.last().unwrap(), &format!("host{DEPTH}"));
    }

    #[test]
    fn includes_bring_in_only_so_many_files() {
        let home = home(&[("config", "Include first/* second/*\nHost own\n")]);
        for index in 0..INCLUDED - 6 {
            for directory in ["first", "second"] {
                write(
                    &home.path().join(format!(".ssh/{directory}/{index:04}")),
                    &format!("Host {directory}{index}\n"),
                );
            }
        }
        let found = list_with(home.path(), None);
        assert_eq!(found.len(), INCLUDED + 1);
        assert_eq!(found[INCLUDED - 1], "second5");
        assert_eq!(found[INCLUDED], "own");
    }

    #[test]
    fn a_file_over_the_size_limit_is_passed_over() {
        let mut large = "Host large\n".to_string();
        large.push_str(&"#".repeat(SIZE as usize));
        let home = home(&[
            ("config", "Include large small\n"),
            ("large", &large),
            ("small", "Host small\n"),
        ]);
        assert_eq!(list_with(home.path(), None), ["small"]);
    }

    #[test]
    fn the_system_file_follows_the_users_and_resolves_beside_itself() {
        let home = home(&[("config", "Host mine shared\n")]);
        let system = tempfile::tempdir().unwrap();
        write(
            &system.path().join("ssh_config"),
            "Host shared everyone\nInclude ssh_config.d/*.conf\n",
        );
        write(&system.path().join("ssh_config.d/site.conf"), "Host site\n");
        // A file of the same name beside the user's is not what it means.
        write(
            &home.path().join(".ssh/ssh_config.d/user.conf"),
            "Host wrong\n",
        );
        assert_eq!(
            list_with(home.path(), Some(&system.path().join("ssh_config"))),
            ["mine", "shared", "everyone", "site"]
        );
    }

    #[test]
    fn a_home_without_a_configuration_has_no_aliases() {
        let home = tempfile::tempdir().unwrap();
        assert!(list_with(home.path(), None).is_empty());
        assert!(list_with(home.path(), Some(&home.path().join("absent"))).is_empty());
    }
}
