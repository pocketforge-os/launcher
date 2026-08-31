use std::process::Command;

#[test]
fn complete_matrix_is_deterministic_in_fresh_processes() {
    let exe = env!("CARGO_BIN_EXE_taffy-spike");
    let run = || Command::new(exe).arg("--matrix-child").output().unwrap();
    let first = run();
    let second = run();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(first.stdout, second.stdout);
}
