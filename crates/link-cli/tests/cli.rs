use std::process::{Command, Output};

use serde_json::Value;

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_linkctl"))
        .args(arguments)
        .env_remove("LINKCTL_FORMAT")
        .env_remove("LINKCTL_CONFIG")
        .output()
        .expect("linkctl should execute")
}

fn run_with_environment(arguments: &[&str], name: &str, value: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_linkctl"))
        .args(arguments)
        .env_remove("LINKCTL_FORMAT")
        .env_remove("LINKCTL_CONFIG")
        .env(name, value)
        .output()
        .expect("linkctl should execute")
}

#[test]
fn help_and_version_advertise_only_implemented_commands() {
    let help = run(&["--help"]);
    assert!(help.status.success());
    let stdout = String::from_utf8(help.stdout).expect("UTF-8 help");
    assert!(stdout.contains("--unsafe-xu"));
    assert!(stdout.contains("Commands:"));
    assert!(stdout.contains("device"));
    assert!(!stdout.contains("control set"));

    let version = run(&["--version"]);
    assert!(version.status.success());

    let unsafe_help = run(&["--unsafe-xu", "--help"]);
    assert!(unsafe_help.status.success());
}

#[test]
fn device_help_describes_read_only_inventory_commands() {
    let help = run(&["device", "--help"]);
    assert!(help.status.success());
    let stdout = String::from_utf8(help.stdout).expect("UTF-8 help");
    assert!(stdout.contains("list"));
    assert!(stdout.contains("probe"));

    let probe_help = run(&["device", "probe", "--help"]);
    assert!(probe_help.status.success());
    let stdout = String::from_utf8(probe_help.stdout).expect("UTF-8 help");
    assert!(stdout.contains("--bundle"));
    assert!(stdout.contains("--include-serial"));
}

#[test]
fn unsafe_xu_from_the_environment_reaches_the_safety_gate() {
    let output = run_with_environment(&[], "LINKCTL_UNSAFE_XU", "true");

    assert_eq!(output.status.code(), Some(9));
    assert!(
        String::from_utf8(output.stderr)
            .expect("UTF-8 error")
            .contains("raw XU support is not available")
    );
}

#[test]
fn unsafe_xu_is_rejected_in_human_output() {
    let output = run(&["--unsafe-xu"]);

    assert_eq!(output.status.code(), Some(9));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .expect("UTF-8 error")
            .contains("raw XU support is not available")
    );
}

#[test]
fn unsafe_xu_is_rejected_with_a_schema_conforming_json_error() {
    let output = run(&["--format", "json", "--unsafe-xu"]);

    assert_eq!(output.status.code(), Some(9));
    let value: Value = serde_json::from_slice(&output.stdout).expect("JSON error");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["ok"], false);
    assert_eq!(value["command"], "linkctl");
    assert!(value["device"].is_null());
    assert!(value["result"].is_null());
    assert_eq!(value["error"]["code"], "unsafe-operation-denied");
    assert_eq!(value["error"]["exit_code"], 9);
    assert!(output.stderr.is_empty());
}

#[test]
fn parser_errors_honor_an_early_json_format_hint() {
    let output = run(&["--format=json", "--not-a-real-option"]);

    assert_eq!(output.status.code(), Some(2));
    let value: Value = serde_json::from_slice(&output.stdout).expect("JSON error");
    assert_eq!(value["error"]["code"], "invalid-invocation");
}

#[test]
fn unsupported_schema_is_rejected() {
    let output = run(&["--format", "jsonl", "--schema-version", "2"]);

    assert_eq!(output.status.code(), Some(2));
    let value: Value = serde_json::from_slice(&output.stdout).expect("JSON Lines error");
    assert_eq!(value["error"]["details"]["requested"], 2);
    assert_eq!(value["error"]["details"]["supported"], 1);
}
