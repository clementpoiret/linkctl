use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};

use serde_json::Value;

fn command(arguments: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_linkctl"));
    command
        .args(arguments)
        .env_remove("LINKCTL_FORMAT")
        .env_remove("LINKCTL_CONFIG");
    command
}

fn run(arguments: &[&str]) -> Output {
    command(arguments)
        .env_remove("LINKCTL_UNSAFE_XU")
        .output()
        .expect("linkctl should execute")
}

fn run_with_environment(arguments: &[&str], name: &str, value: &str) -> Output {
    command(arguments)
        .env(name, value)
        .output()
        .expect("linkctl should execute")
}

fn run_with_xdg(arguments: &[&str], root: &Path) -> Output {
    command(arguments)
        .env_remove("LINKCTL_DEVICE")
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .output()
        .expect("linkctl should execute")
}

fn successful_help(arguments: &[&str]) -> String {
    let output = run(arguments);
    assert!(
        output.status.success(),
        "{arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 help")
}

#[test]
fn help_and_version_advertise_only_implemented_commands() {
    let stdout = successful_help(&["--help"]);
    assert!(stdout.contains("--unsafe-xu"));
    assert!(stdout.contains("Commands:"));
    assert!(stdout.contains("device"));
    assert!(stdout.contains("control"));
    assert!(stdout.contains("image"));
    assert!(stdout.contains("zoom"));
    assert!(stdout.contains("frame"));
    assert!(stdout.contains("auto-framing"));
    assert!(stdout.contains("gesture"));
    assert!(stdout.contains("portrait"));
    assert!(stdout.contains("privacy"));
    assert!(stdout.contains("firmware"));
    assert!(stdout.contains("video"));
    assert!(stdout.contains("audio"));
    assert!(stdout.contains("snapshot"));
    assert!(stdout.contains("capture"));
    assert!(stdout.contains("record"));
    assert!(stdout.contains("daemon"));
    assert!(stdout.contains("pipeline"));
    assert!(stdout.contains("vcam"));
    assert!(stdout.contains("preset"));
    assert!(stdout.contains("xu"));
    assert!(stdout.contains("doctor"));
    assert!(stdout.contains("completion"));

    let version = run(&["--version"]);
    assert!(version.status.success());

    let unsafe_help = run(&["--unsafe-xu", "--help"]);
    assert!(unsafe_help.status.success());
}

#[test]
fn firmware_help_exposes_only_safe_manual_maintenance_commands() {
    let stdout = successful_help(&["firmware", "--help"]);
    for command in ["info", "watch", "stage"] {
        assert!(stdout.contains(command));
    }
    for prohibited in ["bootloader", "flash", "factory-reset", "force"] {
        assert!(!stdout.contains(prohibited));
    }

    let stdout = successful_help(&["firmware", "stage", "--help"]);
    assert!(stdout.contains("OFFICIAL_FILE"));
    assert!(stdout.contains("--sha256"));
    assert!(stdout.contains("--transition-timeout"));
}

#[test]
fn firmware_watch_rejects_single_json_and_staging_uses_validation_exit_code() {
    let watch = run(&["--format", "json", "firmware", "watch"]);
    assert_eq!(watch.status.code(), Some(2));
    let value: Value = serde_json::from_slice(&watch.stdout).expect("JSON watch error");
    assert_eq!(value["command"], "firmware.watch");

    let directory = tempfile::tempdir().expect("temporary directory");
    let firmware = directory.path().join("wrong-name.bin");
    fs::write(&firmware, b"not firmware").expect("write invalid fixture");
    let staged = run(&[
        "--format",
        "json",
        "--dry-run",
        "firmware",
        "stage",
        &firmware.to_string_lossy(),
    ]);
    assert_eq!(staged.status.code(), Some(14));
    let value: Value = serde_json::from_slice(&staged.stdout).expect("JSON staging error");
    assert_eq!(value["command"], "firmware.stage");
    assert_eq!(value["error"]["code"], "firmware-validation-failure");
}

#[test]
fn daemon_commands_and_virtual_camera_contract_are_exposed_offline() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let unavailable = run_with_xdg(&["--format", "json", "daemon", "status"], directory.path());
    assert_eq!(unavailable.status.code(), Some(12));
    let value: Value = serde_json::from_slice(&unavailable.stdout).expect("JSON daemon error");
    assert_eq!(value["error"]["code"], "daemon-unavailable");

    let dry_run = run_with_xdg(
        &[
            "--format",
            "json",
            "--dry-run",
            "vcam",
            "start",
            "--name",
            "conference",
            "--output-device",
            "/dev/video20",
            "--profile",
            "mirrored",
            "--size",
            "1280x720",
            "--crop",
            "0.1,0.2,0.8,0.6",
        ],
        directory.path(),
    );
    assert!(
        dry_run.status.success(),
        "{}",
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let value: Value = serde_json::from_slice(&dry_run.stdout).expect("JSON vcam plan");
    assert_eq!(value["command"], "vcam.start");
    assert_eq!(value["result"]["specification"]["name"], "conference");
    assert_eq!(value["result"]["specification"]["horizontal_flip"], true);
    assert_eq!(value["result"]["specification"]["width"], 1280);
}

#[test]
fn xu_help_exposes_read_research_write_and_recovery_commands() {
    let stdout = successful_help(&["xu", "--help"]);
    for command in [
        "inventory",
        "get",
        "snapshot",
        "diff",
        "watch",
        "set",
        "raw-set",
        "recover",
    ] {
        assert!(stdout.contains(command));
    }

    assert!(successful_help(&["doctor", "--help"]).contains("--bundle"));
}

#[test]
fn xu_diff_works_offline_with_sanitized_snapshots() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let before = root.join("fixtures/xu-snapshots/sanitized-before.json");
    let after = root.join("fixtures/xu-snapshots/sanitized-after.json");
    let output = run(&[
        "--format",
        "json",
        "xu",
        "diff",
        &before.to_string_lossy(),
        &after.to_string_lossy(),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).expect("JSON diff");
    assert_eq!(value["command"], "xu.diff");
    assert_eq!(value["result"]["selectors"][0]["bytes"][0]["offset"], 1);
    assert_eq!(
        value["result"]["selectors"][0]["bytes"][0]["changed_bits"],
        serde_json::json!([0])
    );
}

#[test]
fn doctor_writes_a_private_no_clobber_diagnostic_archive() {
    let root = tempfile::tempdir().expect("temporary directory");
    let bundle = root.path().join("report.tar.zst");
    let output = run_with_xdg(
        &["doctor", "--bundle", &bundle.to_string_lossy()],
        root.path(),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::metadata(&bundle).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let decoder = zstd::Decoder::new(fs::File::open(&bundle).unwrap()).unwrap();
    let mut archive = tar::Archive::new(decoder);
    let names = archive
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().into_owned())
        .collect::<Vec<_>>();
    assert!(names.iter().any(|path| path.ends_with("doctor.json")));
    assert!(names.iter().any(|path| path.ends_with("manifest.json")));

    let duplicate = run_with_xdg(
        &["doctor", "--bundle", &bundle.to_string_lossy()],
        root.path(),
    );
    assert_eq!(duplicate.status.code(), Some(2));
}

#[test]
fn preset_help_and_local_store_commands_are_hardware_free() {
    let stdout = successful_help(&["preset", "--help"]);
    for command in [
        "save", "apply", "list", "show", "delete", "export", "import",
    ] {
        assert!(stdout.contains(command));
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("input.toml");
    fs::write(
        &input,
        r#"schema_version = 2
name = "local-test"

[requirements]
model = "Insta360 Link 2C Pro"
fallback = "fail"

[standard_controls]
brightness = 50
"#,
    )
    .expect("write import fixture");
    let input = input.to_string_lossy();
    let imported = run_with_xdg(
        &["--format", "json", "preset", "import", &input],
        directory.path(),
    );
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let value: Value = serde_json::from_slice(&imported.stdout).expect("JSON import");
    assert_eq!(value["command"], "preset.import");
    assert_eq!(value["result"]["name"], "local-test");

    let listed = run_with_xdg(&["--format", "json", "preset", "list"], directory.path());
    let value: Value = serde_json::from_slice(&listed.stdout).expect("JSON list");
    let listed = value["result"].as_array().unwrap();
    assert_eq!(listed.len(), 2);
    assert!(
        listed
            .iter()
            .any(|preset| preset["id"] == "builtin:default")
    );

    let shown = run_with_xdg(
        &["--format", "json", "preset", "show", "builtin:default"],
        directory.path(),
    );
    assert!(shown.status.success());
    let value: Value = serde_json::from_slice(&shown.stdout).expect("JSON built-in");
    assert_eq!(value["result"]["schema_version"], 2);
    assert_eq!(value["result"]["name"], "default");

    let builtin_export = run_with_xdg(
        &["preset", "export", "builtin:default", "-"],
        directory.path(),
    );
    assert!(builtin_export.status.success());
    assert!(String::from_utf8_lossy(&builtin_export.stdout).contains("name = \"default\""));

    let builtin_delete = run_with_xdg(&["preset", "delete", "builtin:default"], directory.path());
    assert_eq!(builtin_delete.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&builtin_delete.stderr).contains("immutable"));

    let exported = run_with_xdg(&["preset", "export", "local-test", "-"], directory.path());
    assert!(exported.status.success());
    assert!(String::from_utf8_lossy(&exported.stdout).contains("brightness = 50"));
    assert!(exported.stderr.is_empty());

    let deleted = run_with_xdg(
        &["--format", "json", "preset", "delete", "local-test"],
        directory.path(),
    );
    assert!(deleted.status.success());
    let listed = run_with_xdg(&["--format", "json", "preset", "list"], directory.path());
    let value: Value = serde_json::from_slice(&listed.stdout).expect("JSON list");
    assert_eq!(value["result"].as_array().unwrap().len(), 1);
    assert_eq!(value["result"][0]["id"], "builtin:default");

    let legacy = directory.path().join("legacy.toml");
    fs::write(
        &legacy,
        r#"schema_version = 1
name = "legacy"

[requirements]
model = "Insta360 Link 2C Pro"
fallback = "fail"

[standard_controls]
brightness = 50
"#,
    )
    .expect("write legacy fixture");
    let rejected = run_with_xdg(
        &["preset", "import", &legacy.to_string_lossy()],
        directory.path(),
    );
    assert_eq!(rejected.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("unsupported preset schema"));
}

#[test]
fn published_json_schemas_are_valid_documents() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for relative in [
        "docs/schemas/envelope-v1.json",
        "docs/schemas/preset-v2.json",
        "docs/schemas/transaction-v2.json",
        "docs/schemas/vendor-profile-v1.json",
        "docs/schemas/xu-snapshot-v1.json",
    ] {
        let path = root.join(relative);
        let bytes = fs::read(&path).unwrap_or_else(|error| {
            panic!("failed to read {}: {error}", path.display());
        });
        serde_json::from_slice::<Value>(&bytes).unwrap_or_else(|error| {
            panic!("invalid JSON schema {}: {error}", path.display());
        });
    }
}

#[test]
fn media_help_exposes_exact_tuple_and_output_options() {
    let stdout = successful_help(&["video", "--help"]);
    for command in ["formats", "status", "set", "stats"] {
        assert!(stdout.contains(command));
    }

    let stdout = successful_help(&["snapshot", "--help"]);
    assert!(stdout.contains("--image-format"));
    assert!(stdout.contains("--raw-frame"));
    assert!(!stdout.contains("--format <IMAGE"));

    let stdout = successful_help(&["record", "start", "--help"]);
    assert!(stdout.contains("--segment-duration"));
    assert!(stdout.contains("--rolling"));
    assert!(stdout.contains("--disk-reserve"));
    assert!(stdout.contains("--audio"));
    assert!(stdout.contains("--audio-delay"));
    assert!(stdout.contains("--gate"));
}

#[test]
fn audio_help_exposes_discovery_control_and_streaming_commands() {
    let stdout = successful_help(&["audio", "--help"]);
    for command in [
        "devices", "status", "gain", "mute", "unmute", "mode", "meter", "capture", "monitor",
    ] {
        assert!(stdout.contains(command));
    }

    let stdout = successful_help(&["audio", "capture", "--help"]);
    for option in ["--stdout", "--audio-format", "--sample-rate", "--channels"] {
        assert!(stdout.contains(option));
    }
}

#[test]
fn binary_audio_stdout_requires_an_explicit_encoding_before_discovery() {
    let output = run(&[
        "--device",
        "missing-preflight-device",
        "audio",
        "capture",
        "--stdout",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires --audio-format"));
}

#[test]
fn recording_dry_run_validates_limits_before_opening_hardware() {
    let output = run(&[
        "--device",
        "missing-preflight-device",
        "--dry-run",
        "record",
        "start",
        "output.mkv",
        "--max-size",
        "invalid",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid byte-size value"));
}

#[test]
fn device_help_describes_read_only_inventory_commands() {
    let stdout = successful_help(&["device", "--help"]);
    assert!(stdout.contains("list"));
    assert!(stdout.contains("info"));
    assert!(stdout.contains("watch"));
    assert!(stdout.contains("probe"));

    let stdout = successful_help(&["device", "probe", "--help"]);
    assert!(stdout.contains("--bundle"));
    assert!(stdout.contains("--include-serial"));
}

#[test]
fn control_and_image_help_expose_expected_commands() {
    let stdout = successful_help(&["control", "--help"]);
    for command in ["list", "get", "set", "reset", "watch"] {
        assert!(stdout.contains(command));
    }

    let stdout = successful_help(&["image", "--help"]);
    assert!(stdout.contains("white-balance"));
    assert!(stdout.contains("anti-flicker"));
}

#[test]
fn camera_native_help_exposes_only_fixed_mount_semantics() {
    for (command, expected) in [
        ("zoom", &["get", "set", "step", "ramp", "reset"][..]),
        ("frame", &["status", "set", "move", "center"]),
        ("auto-framing", &["on", "off", "status", "style"]),
        ("gesture", &["status", "enable", "disable", "set"]),
        ("portrait", &["status", "native"]),
    ] {
        let stdout = successful_help(&[command, "--help"]);
        for item in expected {
            assert!(stdout.contains(item), "{command} help omitted {item}");
        }
    }

    let style = run(&["auto-framing", "style", "--help"]);
    let stdout = String::from_utf8(style.stdout).expect("UTF-8 help");
    assert!(stdout.contains("head"));
    assert!(stdout.contains("half-body"));
    assert!(!stdout.contains("full-body"));

    let stdout = successful_help(&["mode", "compatibility", "set", "--help"]);
    assert!(stdout.contains("standard"));
    assert!(stdout.contains("low-resolution"));
}

#[test]
fn completion_generates_raw_shell_source() {
    let output = run(&["completion", "bash"]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 completion");
    assert!(stdout.contains("linkctl"));

    let output = run(&["--format", "json", "completion", "bash"]);
    assert_eq!(output.status.code(), Some(2));
    let value: Value = serde_json::from_slice(&output.stdout).expect("JSON error");
    assert_eq!(value["command"], "completion");
}

#[test]
fn watch_rejects_single_json_output_before_opening_hardware() {
    let output = run(&["--format", "json", "device", "watch"]);
    assert_eq!(output.status.code(), Some(2));
    let value: Value = serde_json::from_slice(&output.stdout).expect("JSON error");
    assert_eq!(value["command"], "device.watch");
}

#[test]
fn forced_nonstandard_control_backend_is_reported_as_unsupported() {
    let output = run(&[
        "--device",
        "missing-preflight-device",
        "--format",
        "json",
        "--backend",
        "vendor",
        "control",
        "list",
    ]);
    assert_eq!(output.status.code(), Some(4));
    let value: Value = serde_json::from_slice(&output.stdout).expect("JSON error");
    assert_eq!(value["command"], "control.list");
    assert_eq!(value["error"]["details"]["requested_backend"], "vendor");
}

#[test]
fn unsafe_xu_from_the_environment_reaches_the_safety_gate() {
    let output = run_with_environment(
        &[
            "--device",
            "missing-preflight-device",
            "xu",
            "raw-set",
            "--guid",
            "11111111-1111-1111-1111-111111111111",
            "--selector",
            "1",
            "--hex",
            "00",
        ],
        "LINKCTL_UNSAFE_XU",
        "true",
    );

    assert_eq!(output.status.code(), Some(9));
    assert!(
        String::from_utf8(output.stderr)
            .expect("UTF-8 error")
            .contains("raw XU")
    );
}

#[test]
fn unsafe_xu_is_rejected_in_human_output() {
    let output = run(&[
        "--device",
        "missing-preflight-device",
        "--unsafe-xu",
        "xu",
        "raw-set",
        "--guid",
        "11111111-1111-1111-1111-111111111111",
        "--selector",
        "1",
        "--hex",
        "00",
    ]);

    assert_eq!(output.status.code(), Some(9));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .expect("UTF-8 error")
            .contains("raw XU")
    );
}

#[test]
fn unsafe_xu_is_rejected_with_a_schema_conforming_json_error() {
    let output = run(&[
        "--device",
        "missing-preflight-device",
        "--format",
        "json",
        "--unsafe-xu",
        "xu",
        "raw-set",
        "--guid",
        "11111111-1111-1111-1111-111111111111",
        "--selector",
        "1",
        "--hex",
        "00",
    ]);

    assert_eq!(output.status.code(), Some(9));
    let value: Value = serde_json::from_slice(&output.stdout).expect("JSON error");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["ok"], false);
    assert_eq!(value["command"], "xu.raw-set");
    assert!(value["device"].is_null());
    assert!(value["result"].is_null());
    assert_eq!(value["error"]["code"], "unsafe-operation-denied");
    assert_eq!(value["error"]["exit_code"], 9);
    assert!(output.stderr.is_empty());
}

#[test]
fn unsafe_xu_is_scoped_to_raw_set() {
    let output = run(&[
        "--device",
        "missing-preflight-device",
        "--unsafe-xu",
        "xu",
        "inventory",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("only with xu raw-set"));
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
