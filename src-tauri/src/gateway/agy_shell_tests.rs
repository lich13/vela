use super::*;

const BODY: &str = "agy() {\n  :\n}";

fn block_of(newline: &str) -> String {
    [OPEN, "agy() {", "  :", "}", CLOSE]
        .iter()
        .map(|line| format!("{line}{newline}"))
        .collect()
}

#[test]
fn the_block_goes_after_what_a_file_holds_and_comes_out_whole() {
    assert_eq!(with_block("", Some(BODY), false), block_of("\n"));
    let installed = with_block("export A=1\n", Some(BODY), false);
    assert_eq!(installed, format!("export A=1\n\n{}", block_of("\n")));
    assert_eq!(with_block(&installed, None, false), "export A=1\n");
    assert_eq!(with_block(&block_of("\n"), None, false), "");
    // A last line without an ending gets one before the block.
    assert_eq!(
        with_block("export A=1", Some(BODY), false),
        format!("export A=1\n\n{}", block_of("\n"))
    );
    assert_eq!(with_block("export A=1\n", None, false), "export A=1\n");
}

#[test]
fn a_block_is_replaced_where_it_stands_and_nothing_else_moves() {
    let around = format!(
        "before\n\n{}after\n",
        block_of("\n").replace("  :", "  old")
    );
    let replaced = with_block(&around, Some(BODY), false);
    assert_eq!(replaced, format!("before\n\n{}after\n", block_of("\n")));
    assert_eq!(with_block(&replaced, Some(BODY), false), replaced);
    assert_eq!(with_block(&replaced, None, false), "before\nafter\n");
    // The file ended at the closing marker: it still does.
    let unended = format!("before\n{}", block_of("\n").trim_end());
    assert_eq!(with_block(&unended, Some(BODY), false), unended);
}

#[test]
fn line_endings_follow_the_file() {
    let installed = with_block("$a = 1\r\n", Some(BODY), false);
    assert_eq!(installed, format!("$a = 1\r\n\r\n{}", block_of("\r\n")));
    assert_eq!(with_block(&installed, None, false), "$a = 1\r\n");
    // A new PowerShell profile is written the way Windows writes text.
    assert_eq!(with_block("", Some(BODY), true), block_of("\r\n"));
    assert_eq!(
        with_block("a\n", Some(BODY), true),
        format!("a\n\n{}", block_of("\n"))
    );
}

#[test]
fn an_opening_marker_cut_from_its_end_gives_up_only_its_own_line() {
    let cut = format!("before\n{OPEN}\nkept by hand\n");
    assert_eq!(
        with_block(&cut, Some(BODY), false),
        format!("before\n{}kept by hand\n", block_of("\n"))
    );
    assert_eq!(with_block(&cut, None, false), "before\nkept by hand\n");
    // A marker inside a longer line is somebody's text, not a block.
    let quoted = format!("echo '{OPEN}'\n");
    assert_eq!(with_block(&quoted, None, false), quoted);
}

#[test]
fn only_plain_addresses_and_tokens_reach_the_environment_file() {
    assert_eq!(
        environment("http://127.0.0.1:43721", "fixture-token_1.a").as_deref(),
        Some(
            "AGY_LLM_GATEWAY_URL=http://127.0.0.1:43721\n\
             AGY_LLM_GATEWAY_API_KEY=fixture-token_1.a\n\
             AGY_LLM_GATEWAY_WIRE_PROTOCOL=genai\n"
        )
    );
    for token in [
        "",
        "two words",
        "star*",
        "quote'",
        "dollar$x",
        "new\nline",
        "semi;colon",
    ] {
        assert_eq!(
            environment("http://127.0.0.1:43721", token),
            None,
            "{token:?}"
        );
    }
    assert_eq!(environment("http://127.0.0.1:43721/?a b", "fixture"), None);
}

#[test]
fn the_environment_path_is_quoted_for_each_shell() {
    let path = Path::new("/Users/it's me/.vela/antigravity/agy.env");
    let posix = function(Kind::Posix, path).unwrap();
    assert!(posix.contains(r"[ -r '/Users/it'\''s me/.vela/antigravity/agy.env' ]"));
    assert!(posix.starts_with("unalias agy 2>/dev/null || true\nagy() {\n"));
    let fish = function(Kind::Fish, path).unwrap();
    assert!(fish.contains(r"test -r '/Users/it\'s me/.vela/antigravity/agy.env'"));
    let powershell = function(
        Kind::PowerShell,
        Path::new(r"C:\Users\it's me\.vela\agy.env"),
    )
    .unwrap();
    assert!(powershell.contains(r"$file = 'C:\Users\it''s me\.vela\agy.env'"));
    assert_eq!(
        command(path).as_deref(),
        (!cfg!(windows)).then_some(r"env $(cat '/Users/it'\''s me/.vela/antigravity/agy.env') agy")
    );
    assert_eq!(function(Kind::Posix, Path::new("/tmp/a\nb")), None);
    assert_eq!(command(Path::new("/tmp/a\nb")), None);
}

#[cfg(unix)]
mod files {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn shells(home: &Path, login: &str) -> Shells {
        Shells {
            home: home.to_owned(),
            login: Some(login.into()),
            ..Default::default()
        }
    }
    fn environment_file(home: &Path) -> PathBuf {
        home.join(".vela/antigravity/agy.env")
    }
    fn bash_profile(home: &Path) -> PathBuf {
        home.join(if cfg!(target_os = "macos") {
            ".bash_profile"
        } else {
            ".bashrc"
        })
    }

    #[test]
    fn startup_files_follow_the_login_shell_and_stay_inside_the_home() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let found = shells(home, "/bin/zsh").profiles();
        assert_eq!(
            found,
            [
                Profile {
                    kind: Kind::Posix,
                    path: home.join(".zshrc"),
                    create: true
                },
                Profile {
                    kind: Kind::Posix,
                    path: bash_profile(home),
                    create: false
                },
                Profile {
                    kind: Kind::Fish,
                    path: home.join(".config/fish/config.fish"),
                    create: false
                },
            ]
        );
        // fish that has run once reads its file, whatever the login shell.
        std::fs::create_dir_all(home.join(".config/fish")).unwrap();
        assert!(shells(home, "/bin/zsh").profiles()[2].create);
        assert!(shells(home, "/opt/homebrew/bin/bash").profiles()[1].create);
        let moved = Shells {
            zdotdir: Some(home.join("zsh")),
            xdg_config: Some(home.join("xdg")),
            ..shells(home, "/usr/bin/fish")
        };
        let found = moved.profiles();
        assert_eq!(found[0].path, home.join("zsh/.zshrc"));
        assert_eq!(found[2].path, home.join("xdg/fish/config.fish"));
        assert!(found[2].create && !found[0].create);
        // A relative directory names nothing; one outside the home is not ours.
        let relative = Shells {
            zdotdir: Some("zsh".into()),
            ..shells(home, "/bin/zsh")
        };
        assert_eq!(relative.profiles()[0].path, home.join(".zshrc"));
        let outside = Shells {
            zdotdir: Some("/etc".into()),
            ..shells(home, "/bin/zsh")
        };
        assert!(outside
            .profiles()
            .iter()
            .all(|profile| profile.path.starts_with(home)));
        assert_eq!(outside.profiles().len(), 2);
        assert_eq!(moved.display(&home.join("zsh/.zshrc")), "~/zsh/.zshrc");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_mac_login_bash_reads_the_first_profile_it_finds() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(home.join(".profile"), "").unwrap();
        assert_eq!(
            shells(home, "/bin/bash").profiles()[1].path,
            home.join(".profile")
        );
        std::fs::write(home.join(".bash_login"), "").unwrap();
        assert_eq!(
            shells(home, "/bin/bash").profiles()[1].path,
            home.join(".bash_login")
        );
    }

    #[test]
    fn installing_twice_changes_nothing_and_removing_gives_the_file_back() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let rc = home.join(".zshrc");
        std::fs::write(&rc, "export A=1\n").unwrap();
        std::fs::set_permissions(&rc, std::fs::Permissions::from_mode(0o644)).unwrap();
        let shells = shells(home, "/bin/zsh");
        let installed = install(&shells, &environment_file(home));
        assert_eq!(installed.profiles, ["~/.zshrc"]);
        assert!(installed.failures.is_empty());
        let written = std::fs::read_to_string(&rc).unwrap();
        assert!(written.starts_with(&format!("export A=1\n\n{OPEN}\nunalias agy")));
        assert!(written.ends_with(&format!("}}\n{CLOSE}\n")));
        // The replacement is made private; the startup file keeps its own mode.
        assert_eq!(rc.metadata().unwrap().permissions().mode() & 0o777, 0o644);
        // Only the login shell's file is made; bash was never used here.
        assert!(!bash_profile(home).exists());
        assert!(!home.join(".config").exists());
        assert_eq!(install(&shells, &environment_file(home)), installed);
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), written);
        // The data directory moved: the block follows it in place.
        let elsewhere = home.join("elsewhere/agy.env");
        install(&shells, &elsewhere);
        let moved = std::fs::read_to_string(&rc).unwrap();
        assert!(moved.contains("elsewhere/agy.env") && !moved.contains(".vela/antigravity"));
        assert_eq!(moved.matches(OPEN).count(), 1);
        assert!(remove(&shells).is_empty());
        assert_eq!(std::fs::read_to_string(&rc).unwrap(), "export A=1\n");
        assert!(remove(&shells).is_empty());
    }

    #[test]
    fn a_missing_login_file_is_made_and_a_linked_one_is_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let dotfiles = dir.path().join("home/dotfiles");
        std::fs::create_dir_all(&dotfiles).unwrap();
        let shells = shells(&home, "/bin/zsh");
        assert_eq!(
            install(&shells, &environment_file(&home)).profiles,
            ["~/.zshrc"]
        );
        assert!(remove(&shells).is_empty());
        // Vela made the file; emptied, it is still the user's to keep.
        assert_eq!(std::fs::read_to_string(home.join(".zshrc")).unwrap(), "");
        std::fs::remove_file(home.join(".zshrc")).unwrap();
        std::fs::write(dotfiles.join("zshrc"), "# kept\n").unwrap();
        std::os::unix::fs::symlink(dotfiles.join("zshrc"), home.join(".zshrc")).unwrap();
        assert_eq!(
            install(&shells, &environment_file(&home)).profiles,
            ["~/.zshrc"]
        );
        assert!(home
            .join(".zshrc")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(std::fs::read_to_string(dotfiles.join("zshrc"))
            .unwrap()
            .contains(OPEN));
        assert!(remove(&shells).is_empty());
        assert_eq!(
            std::fs::read_to_string(dotfiles.join("zshrc")).unwrap(),
            "# kept\n"
        );
    }

    #[test]
    fn a_file_that_cannot_be_read_as_text_is_left_alone_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(home.join(".zshrc"), [0xff, 0xfe, b'\n']).unwrap();
        let installed = install(&shells(home, "/bin/zsh"), &environment_file(home));
        assert!(installed.profiles.is_empty());
        assert_eq!(installed.failures.len(), 1);
        assert!(installed.failures[0].starts_with("~/.zshrc："));
        assert_eq!(
            std::fs::read(home.join(".zshrc")).unwrap(),
            [0xff, 0xfe, b'\n']
        );
    }

    /// A stand-in `agy` that prints what it was given, on a PATH of its own.
    fn fake_agy(dir: &Path) -> PathBuf {
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let agy = bin.join("agy");
        std::fs::write(
            &agy,
            "#!/bin/sh\nprintf 'url=%s key=%s wire=%s args=%s' \"$AGY_LLM_GATEWAY_URL\" \"$AGY_LLM_GATEWAY_API_KEY\" \"$AGY_LLM_GATEWAY_WIRE_PROTOCOL\" \"$#\"\nfor a in \"$@\"; do printf ' [%s]' \"$a\"; done\necho\n",
        )
        .unwrap();
        std::fs::set_permissions(&agy, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    fn run(shell: &str, bin: &Path, script: &str) -> Option<String> {
        let path = format!("{}:/usr/bin:/bin", bin.display());
        let output = std::process::Command::new(shell)
            .arg("-c")
            .arg(script)
            .env_clear()
            .env("PATH", path)
            .output()
            .ok()?;
        assert!(
            output.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Some(String::from_utf8(output.stdout).unwrap())
    }

    #[test]
    fn the_function_runs_in_the_shells_that_read_it() {
        let dir = tempfile::tempdir().unwrap();
        // A space and a quote in the path, as a home folder may have.
        let data = dir.path().join("it's here");
        std::fs::create_dir_all(&data).unwrap();
        let file = data.join("agy.env");
        let bin = fake_agy(dir.path());
        let define = function(Kind::Posix, &file).unwrap();
        let on = environment("http://127.0.0.1:43721", "fixture-token").unwrap();
        let mut ran = 0;
        for shell in ["sh", "bash", "zsh", "dash"] {
            std::fs::write(&file, &on).unwrap();
            let script = format!(
                "alias agy='echo aliased'\n{define}\nagy 'a b' --flag\nagy remote-control start\nagy --remote-control=1\necho \"left=${{AGY_LLM_GATEWAY_URL:-}}${{vela_agy_arg:-}}\"\n"
            );
            let Some(output) = run(shell, &bin, &script) else {
                continue;
            };
            ran += 1;
            assert_eq!(
                output.lines().collect::<Vec<_>>(),
                [
                    "url=http://127.0.0.1:43721 key=fixture-token wire=genai args=2 [a b] [--flag]",
                    // Remote control signs in with Google: never on the gateway.
                    "url= key= wire= args=2 [remote-control] [start]",
                    "url= key= wire= args=1 [--remote-control=1]",
                    // Nothing of the gateway stays behind in the shell itself.
                    "left=",
                ],
                "{shell}"
            );
            // Without the file, which is whenever the gateway is off.
            std::fs::remove_file(&file).unwrap();
            assert_eq!(
                run(shell, &bin, &format!("{define}\nagy plain\n")).unwrap(),
                "url= key= wire= args=1 [plain]\n",
                "{shell}"
            );
        }
        assert!(ran > 0, "no POSIX shell to run the function in");
        std::fs::write(&file, &on).unwrap();
        let by_hand = command(&file).unwrap();
        assert_eq!(
            run("sh", &bin, &format!("{by_hand} one\n")).unwrap(),
            "url=http://127.0.0.1:43721 key=fixture-token wire=genai args=1 [one]\n"
        );
        // fish is rarely installed; where it is, its function is held to the same.
        let define = function(Kind::Fish, &file).unwrap();
        if let Some(output) = run(
            "fish",
            &bin,
            &format!("{define}\nagy 'a b' --flag\nagy remote-control\n"),
        ) {
            assert_eq!(
                output.lines().collect::<Vec<_>>(),
                [
                    "url=http://127.0.0.1:43721 key=fixture-token wire=genai args=2 [a b] [--flag]",
                    "url= key= wire= args=1 [remote-control]",
                ]
            );
        }
    }
}
