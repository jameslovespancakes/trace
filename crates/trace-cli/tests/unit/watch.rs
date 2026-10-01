use super::*;
use clap::Parser;

#[test]
fn rule_watch_follows_the_global_flags() {
    let cli = crate::cli::Cli::try_parse_from(["trace", "--json", "index", "--watch"]).unwrap();
    let o = options(&cli.global);
    assert!(o.json);
    assert_eq!(o.offline, crate::app::offline());
    let cli = crate::cli::Cli::try_parse_from(["trace", "index", "--watch"]).unwrap();
    assert!(!options(&cli.global).json);
}
