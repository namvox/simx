use std::process::Command;

#[test]
fn real_simulator_smoke_is_gated() {
    if std::env::var("SIMX_REAL_SIM_TESTS").as_deref() != Ok("1") {
        return;
    }

    let binary = env!("CARGO_BIN_EXE_simx");
    let _ = Command::new(binary).arg("clean").output();
    run(binary, &["init", "--size", "1"]);
    let lease = run(
        binary,
        &[
            "lease",
            "--slug",
            "real-smoke",
            "--ttl",
            "2m",
            "--json",
            "--wait-timeout",
            "5s",
        ],
    );
    assert!(lease.contains(r#""slug": "real-smoke""#));
    assert!(lease.contains(r#""udid":"#) || lease.contains(r#""udid": "#));
    let renewed = run(
        binary,
        &["renew", "--slug", "real-smoke", "--ttl", "2m", "--json"],
    );
    assert!(renewed.contains(r#""slug": "real-smoke""#));
    let status = run(binary, &["status", "--json"]);
    assert!(status.contains(r#""slug": "real-smoke""#));
    run(binary, &["release", "--slug", "real-smoke"]);
    run(binary, &["clean"]);
}

fn run(binary: &str, args: &[&str]) -> String {
    let output = Command::new(binary)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("failed to run simx {args:?}: {error}"));
    assert!(
        output.status.success(),
        "simx {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}
