use super::*;
use std::io::Read;

fn node() -> Option<PathBuf> {
    trace_env::os::find_executable(
        &["node"],
        &trace_env::lookup::path_dirs(),
        &trace_env::os::Platform::current(),
    )
}

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("trace-tests")
        .join("trace-semantic-tests")
        .join(format!("{name}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Rule: the signals only ever address the child's own tree: its process group (the
/// negative pid after `--`) or taskkill's `/T /PID <child>`.
#[test]
fn rule_tree_signals_address_only_the_child_tree() {
    assert_eq!(group_signal_args(4242, "TERM"), vec!["-TERM", "--", "-4242"]);
    assert_eq!(group_signal_args(4242, "KILL"), vec!["-KILL", "--", "-4242"]);
    assert_eq!(tree_kill_args(4242), vec!["/T", "/F", "/PID", "4242"]);
}

/// Rule: when trace is done with a process, the processes it started are stopped too:
/// a grandchild that would outlive its parent stops writing its heartbeat, and the pipe
/// the tree shared reaches its end (nothing keeps it open).
#[test]
fn rule_process_tree_is_stopped_when_the_session_ends() {
    let Some(node) = node() else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    let dir = test_dir("procs-tree");
    let beat = dir.join("beat.txt");
    let grandchild = dir.join("grandchild.js");
    std::fs::write(
        &grandchild,
        "const fs = require('fs'); let n = 0;\n\
             setInterval(() => { n += 1; fs.writeFileSync(process.argv[2], String(n)); }, 50);\n",
    )
    .unwrap();
    let parent = dir.join("parent.js");
    std::fs::write(
        &parent,
        "const { spawn } = require('child_process');\n\
             spawn(process.execPath, [process.argv[2], process.argv[3]], { stdio: 'inherit' });\n\
             process.stdout.write('started\\n');\n\
             setInterval(() => {}, 1000);\n",
    )
    .unwrap();
    let mut cmd = command(&node);
    cmd.arg(&parent).arg(&grandchild).arg(&beat).stdout(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    // Wait for the grandchild's heartbeat.
    let until = Instant::now() + Duration::from_secs(20);
    while !beat.is_file() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(beat.is_file(), "the grandchild started");
    stop_tree(&mut child, Duration::from_secs(2));
    // The pipe shared by the tree ends once every holder is gone.
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        out
    });
    let until = Instant::now() + Duration::from_secs(15);
    while !reader.is_finished() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(reader.is_finished(), "no process of the tree keeps the pipe open");
    let text = reader.join().unwrap();
    assert!(text.contains("started"), "{text}");
    std::thread::sleep(Duration::from_millis(300));
    let before = std::fs::read_to_string(&beat).unwrap_or_default();
    std::thread::sleep(Duration::from_millis(600));
    let after = std::fs::read_to_string(&beat).unwrap_or_default();
    assert_eq!(before, after, "the grandchild no longer runs");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Rule: a started program never gets trace's standard streams: stdin is closed (a read
/// ends at once) and output goes only where the caller pointed it.
#[test]
fn rule_server_children_never_inherit_trace_stdio() {
    let Some(node) = node() else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    let mut cmd = command(&node);
    cmd.arg("-e")
        .arg(
            "let n = 0; process.stdin.on('data', (d) => { n += d.length; });\n\
                 process.stdin.on('end', () => { process.stdout.write('eof:' + n); process.exit(0); });\n\
                 process.stderr.write('to nowhere');\n",
        )
        .stdout(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let status = wait_bounded(&mut child, Duration::from_secs(20)).unwrap();
    assert!(status.is_some_and(|s| s.success()), "stdin ended at once");
    let mut out = String::new();
    stdout.read_to_string(&mut out).unwrap();
    assert_eq!(out, "eof:0");
}

/// Rule: a bounded wait stops the tree when the timeout passes.
#[test]
fn rule_bounded_wait_stops_the_tree_on_timeout() {
    let Some(node) = node() else {
        eprintln!("skipped: node not found on PATH");
        return;
    };
    let mut cmd = command(&node);
    cmd.arg("-e").arg("setInterval(() => {}, 1000);");
    let mut child = cmd.spawn().unwrap();
    let started = Instant::now();
    let status = wait_bounded(&mut child, Duration::from_millis(300)).unwrap();
    assert!(status.is_none(), "timed out");
    assert!(started.elapsed() < Duration::from_secs(15));
    assert!(!running(&mut child));
}
