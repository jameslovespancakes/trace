use super::*;
use clap::CommandFactory;

#[test]
fn every_cli_command_is_documented_and_vice_versa() {
    let cli = crate::cli::Cli::command();
    let mut names: Vec<&str> = cli
        .get_subcommands()
        .filter(|c| !c.is_hide_set())
        .map(|c| c.get_name())
        .collect();
    names.sort_unstable();
    let mut documented: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
    documented.sort_unstable();
    assert_eq!(names, documented);
    for c in cli.get_subcommands().filter(|c| !c.is_hide_set()) {
        let about = c.get_about().map(|s| s.to_string()).unwrap_or_default();
        assert_eq!(about, summary(c.get_name()), "{} about", c.get_name());
        // Every documented argument exists on the clap command and vice versa.
        let mut args: Vec<&str> = c
            .get_arguments()
            .filter(|a| !a.is_global_set() && a.get_id() != "help" && a.get_id() != "version")
            .map(|a| a.get_id().as_str())
            .collect();
        args.sort_unstable();
        let mut documented: Vec<&str> = doc(c.get_name()).args.iter().map(|a| a.name).collect();
        documented.sort_unstable();
        assert_eq!(args, documented, "{} arguments", c.get_name());
    }
}

#[test]
fn help_lists_every_command_once() {
    for c in COMMANDS {
        let needle = format!("  trace {} ", c.name);
        assert_eq!(HELP.matches(&needle).count(), 1, "{needle:?}");
    }
    for flag in [
        "--deep",
        "--watch",
        "--install",
        "--allow-build",
        "--env",
        "--yes",
        "TRACE_OFFLINE",
    ] {
        assert!(HELP.contains(flag), "{flag}");
    }
    // Hidden script options and removed flags are not advertised.
    for flag in ["--json", "--root", "--offline", "--all ", "--budget", "trace find"] {
        assert!(!HELP.contains(flag), "{flag}");
    }
    // No syntax-only mode is described anywhere (PLAN decision 3).
    for c in COMMANDS {
        assert!(!c.when_to_use.contains("deferred"), "{}", c.name);
    }
    assert!(doc("status").when_to_use.contains("install automatically"));
}
