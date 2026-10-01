use super::*;

#[test]
fn rule_command_line_tokens_name_repository_files() {
    let t = Tpl {
        parts: vec![
            TplPart::Lit("python scripts/build.py --port ".into()),
            TplPart::Hole(String::new()),
        ],
        dynamic: true,
    };
    let tokens = command_tokens(&t);
    assert_eq!(tokens, vec!["python", "scripts/build.py", "--port"]);
    assert!(names_a_file("scripts/build.py"));
    assert!(names_a_file("helper.sh"));
    assert!(!names_a_file("ls"));
    assert!(!names_a_file("--port"));
    assert!(!names_a_file("8080"));
    assert!(!names_a_file("1.5"));
}
