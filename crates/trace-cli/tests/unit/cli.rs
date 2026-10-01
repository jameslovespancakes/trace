use super::*;
use clap::CommandFactory;

fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(std::iter::once("trace").chain(args.iter().copied()))
}

fn os(args: &[&str]) -> Vec<std::ffi::OsString> {
    args.iter().map(Into::into).collect()
}

#[test]
fn definition_is_consistent() {
    Cli::command().debug_assert();
}

#[test]
fn query_commands_include_discovery() {
    let cmd = Cli::command();
    let mut names: Vec<&str> = cmd
        .get_subcommands()
        .filter(|c| !c.is_hide_set())
        .map(|c| c.get_name())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["context", "deps", "index", "path", "search", "show", "source", "status", "symbols", "uses"]
    );
    let mut sorted = NAMES;
    sorted.sort_unstable();
    assert_eq!(names, sorted);
}

#[test]
fn top_level_help_is_the_help_text() {
    let help = Cli::command().render_help().to_string();
    assert_eq!(help.trim_end(), HELP.trim_end());
    let err = parse(&["--help"]).unwrap_err();
    assert_eq!(err.exit_code(), 0);
    assert_eq!(err.to_string().trim_end(), HELP.trim_end());
    assert_eq!(parse(&["--version"]).unwrap_err().exit_code(), 0);
}

#[test]
fn defaults() {
    let cli = parse(&["status"]).unwrap();
    assert!(!cli.global.json);
    assert!(cli.global.root.is_none());
    assert!(matches!(
        cli.command,
        Command::Status {
            install: None,
            yes: false
        }
    ));
    match parse(&["context", "src/a.py:f"]).unwrap().command {
        Command::Context { symbol, deep } => {
            assert_eq!(symbol, "src/a.py:f");
            assert!(!deep);
        }
        other => panic!("unexpected {other:?}"),
    }
    match parse(&["show", "a", "b.c"]).unwrap().command {
        Command::Show { symbols } => assert_eq!(symbols, ["a", "b.c"]),
        other => panic!("unexpected {other:?}"),
    }
    match parse(&["uses", "src/a.py:f"]).unwrap().command {
        Command::Uses { symbol, deep } => {
            assert_eq!(symbol, "src/a.py:f");
            assert!(!deep);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(
        parse(&["index"]).unwrap().command,
        Command::Index { watch: false, allow_build: false, ref env } if env.is_empty()
    ));
}

#[test]
fn deep_and_global_options_before_or_after_the_command() {
    let cli = parse(&["deps", "src/a.py:f", "--json", "--root", "C:/repo", "--deep"]).unwrap();
    assert!(matches!(cli.command, Command::Deps { ref symbol, deep: true } if symbol == "src/a.py:f"));
    assert_eq!(cli.command.name(), "deps");
    assert!(cli.command.deep());
    assert!(cli.global.json);
    assert_eq!(cli.global.root.as_deref(), Some(std::path::Path::new("C:/repo")));
    let cli = parse(&["--json", "uses", "x", "--deep"]).unwrap();
    assert!(cli.global.json);
    assert!(matches!(cli.command, Command::Uses { deep: true, .. }));
    assert!(parse(&["path", "a", "b", "--deep"]).unwrap().command.deep());
    assert!(parse(&["context", "a", "--deep"]).unwrap().command.deep());
    assert!(!parse(&["show", "a"]).unwrap().command.deep());
    let cli = parse(&["status", "--install", "go"]).unwrap();
    assert!(matches!(cli.command, Command::Status { install: Some(ref l), yes: false } if l == "go"));
    let cli = parse(&["status", "--install", "php", "--yes"]).unwrap();
    assert!(matches!(cli.command, Command::Status { install: Some(ref l), yes: true } if l == "php"));
    let cli = parse(&["index", "--allow-build", "--env", "venv", "--env", "node_modules"]).unwrap();
    assert!(matches!(cli.command, Command::Index { allow_build: true, ref env, .. } if env.len() == 2));
    assert!(parse(&["index", "--watch"]).is_ok());
}

#[test]
fn flags_a_command_would_ignore_are_rejected() {
    for args in [
        &["show", "x", "--deep"][..],
        &["status", "--deep"],
        &["index", "--deep"],
        &["status", "--watch"],
        &["uses", "x", "--install", "go"],
        &["status", "--install"],
        &["status", "--yes"],
        &["uses", "x", "--allow-build"],
    ] {
        let err = parse(args).unwrap_err();
        assert_eq!(err.exit_code(), 2, "{args:?}");
    }
}

#[test]
fn missing_arguments() {
    assert!(parse(&["show"]).is_err());
    assert!(parse(&["context"]).is_err());
    assert!(parse(&["uses"]).is_err());
    assert!(parse(&["deps"]).is_err());
    assert_eq!(parse(&["path", "only-one"]).unwrap_err().exit_code(), 2);
    // `context` takes one symbol, not a sentence.
    assert_eq!(
        parse(&["context", "how", "does", "login", "work"])
            .unwrap_err()
            .exit_code(),
        2
    );
}

#[test]
fn removed_commands_and_flags_are_rejected() {
    for args in [
        &["find", "x"][..],
        &["query", "x"],
        &["references", "x"],
        &["impact", "x"],
        &["evidence", "x"],
        &["dependencies", "x"],
        &["doctor"],
        &["setup", "go"],
        &["serve"],
        &["--format", "json", "status"],
        &["--offline", "status"],
        &["--no-jev", "status"],
        &["--no-bridges", "deps", "x"],
        &["--include", "possible", "deps", "x"],
        &["--depth", "3", "deps", "x"],
        &["deps", "x", "--all"],
        &["uses", "x", "--all"],
        &["path", "a", "b", "--all"],
        &["context", "x", "--budget", "500"],
        &["uses", "x", "--kind", "call"],
        &["uses", "x", "--no-overrides"],
        &["uses", "x", "--live"],
        &["uses", "x", "--callers-only"],
        &["uses", "--diff", "HEAD"],
        &["index", "--rebuild"],
        &["index", "--clean"],
        &["status", "--languages"],
    ] {
        let err = parse(args).unwrap_err();
        assert_eq!(err.exit_code(), 2, "{args:?}");
    }
}

#[test]
fn audit_alias_and_discovery_options_parse() {
    assert!(!parse(&["show", "x"]).unwrap().global.audit);
    assert!(parse(&["show", "x", "--audit"]).unwrap().global.audit);
    assert!(parse(&["--audit", "--json", "context", "x"]).unwrap().global.audit);
    assert!(matches!(
        parse(&["symbols", "cookie", "--file", "test", "--tests", "--limit", "5", "--offset", "10"])
            .unwrap()
            .command,
        Command::Symbols {
            query: Some(_),
            file: Some(_),
            tests: true,
            limit: 5,
            offset: 10,
            ..
        }
    ));
    assert!(parse(&["symbols", "--limit", "0"]).is_err());
    assert!(parse(&["symbols", "--limit", "101"]).is_err());
    assert!(parse(&["symbols", "--offset", "-1"]).is_err());
    assert!(parse(&["symbols", "hook", "--mode", "body"]).is_ok());
    assert!(parse(&["symbols", "--mode", "typo"]).is_err());
    assert!(parse(&["show", "x", "--mode", "body"]).is_err());
}

#[test]
fn sniffing_raw_arguments() {
    assert_eq!(sniff_args(&os(&["trace", "--json", "deps"])), ("deps", true));
    assert_eq!(sniff_args(&os(&["trace", "path", "--json"])), ("path", true));
    assert_eq!(sniff_args(&os(&["trace", "uses", "x"])), ("uses", false));
    assert_eq!(sniff_args(&os(&["trace", "show", "x"])), ("show", false));
    assert_eq!(sniff_args(&os(&["trace", "--root", "show", "status"])), ("status", false));
    assert_eq!(sniff_args(&os(&["trace", "references", "x", "--json"])), ("trace", true));
    assert_eq!(sniff_args(&os(&["trace", "bogus"])), ("trace", false));
}
