//! The `tax-radar-au` command line: ported from tests/radar/test_radar_cli.py
//! and the command-line cases of test_radar_paths.py and test_exposure.py.
mod common;

use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

use common::*;
use tempfile::tempdir;

fn stderr(output: &std::process::Output) -> String {
    text(&output.stderr)
}

fn stdout(output: &std::process::Output) -> String {
    text(&output.stdout)
}

fn review_argv(queue: &Path) -> Vec<String> {
    vec![
        "validate-review".into(),
        "--queue".into(),
        queue.to_string_lossy().into(),
        "--decision".into(),
        sample("decisions", "sample-technical-review.json")
            .to_string_lossy()
            .into(),
    ]
}

#[test]
fn cli_compare_and_validate_review() {
    let dir = tempdir().unwrap();
    let output = dir.path().join("cli-test");
    assert_eq!(run(&compare_argv(&output), None).status.code(), Some(0));
    let validation_out = dir.path().join("validation").join("result.json");
    let mut argv = review_argv(&output.join("impact-queue.json"));
    argv.extend(["--out".into(), validation_out.to_string_lossy().into()]);
    let done = run(&argv, None);
    assert_eq!(done.status.code(), Some(0));
    assert_eq!(
        stdout(&done),
        "tax-radar-au: DECISION_RECORDED; 1 decision(s)\n"
    );
    assert!(validation_out.is_file());
}

#[test]
fn compare_preserves_inputs_at_output_paths() {
    for (index, option) in ["--baseline", "--observation", "--map"]
        .into_iter()
        .enumerate()
    {
        for name in ["impact-queue.json", "impact-queue.md"] {
            let dir = tempdir().unwrap();
            let mut argv = compare_argv(dir.path());
            let position = argv.iter().position(|a| a == option).unwrap() + 1;
            let source = dir.path().join(name);
            let original = std::fs::read(&argv[position]).unwrap();
            std::fs::write(&source, &original).unwrap();
            argv[position] = source.to_string_lossy().into();
            let done = run(&argv, None);
            assert_eq!(done.status.code(), Some(2), "{index} {name}");
            assert!(stderr(&done).contains("would replace an input file"));
            assert_eq!(std::fs::read(&source).unwrap(), original);
            assert_eq!(names(dir.path()), [name]);
        }
    }
}

#[test]
fn validate_review_preserves_its_inputs() {
    for target in ["queue", "decision", "markdown"] {
        for alias in ["direct", "relative", "hardlink", "symlink"] {
            let dir = tempdir().unwrap();
            let root = dir.path();
            assert_eq!(
                run(&compare_argv(&root.join("queue")), Some(root))
                    .status
                    .code(),
                Some(0)
            );
            let queue_path = root.join("queue").join("impact-queue.json");
            let decision_path = root.join("decision.json");
            std::fs::copy(
                sample("decisions", "sample-technical-review.json"),
                &decision_path,
            )
            .unwrap();
            let inputs = [
                ("queue", queue_path.clone()),
                ("decision", decision_path.clone()),
                ("markdown", root.join("queue").join("impact-queue.md")),
            ];
            let original: Vec<Vec<u8>> = inputs
                .iter()
                .map(|(_, p)| std::fs::read(p).unwrap())
                .collect();
            let target_path = inputs
                .iter()
                .find(|(name, _)| *name == target)
                .unwrap()
                .1
                .clone();
            let output = match alias {
                "direct" => target_path.clone(),
                "relative" => target_path.strip_prefix(root).unwrap().to_path_buf(),
                _ => {
                    let link = root.join("alias.json");
                    let made = if alias == "hardlink" {
                        std::fs::hard_link(&target_path, &link)
                    } else {
                        symlink_file(&target_path, &link)
                    };
                    if made.is_err() {
                        eprintln!("skipped: {alias} unavailable");
                        continue;
                    }
                    link
                }
            };
            let done = cli(
                &[
                    "validate-review",
                    "--queue",
                    &queue_path.to_string_lossy(),
                    "--decision",
                    &decision_path.to_string_lossy(),
                    "--out",
                    &output.to_string_lossy(),
                ],
                Some(root),
            );
            assert_eq!(done.status.code(), Some(2), "{target} {alias}");
            assert!(
                stderr(&done).contains("would replace an input file"),
                "{target} {alias}: {}",
                stderr(&done)
            );
            let now: Vec<Vec<u8>> = inputs
                .iter()
                .map(|(_, p)| std::fs::read(p).unwrap())
                .collect();
            assert_eq!(now, original);
        }
    }
}

#[cfg(unix)]
fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

#[test]
fn validate_review_preserves_an_existing_receipt() {
    let dir = tempdir().unwrap();
    assert_eq!(
        run(&compare_argv(&dir.path().join("queue")), None)
            .status
            .code(),
        Some(0)
    );
    let output = dir.path().join("validation.json");
    std::fs::write(&output, "previous receipt\n").unwrap();
    let mut argv = review_argv(&dir.path().join("queue").join("impact-queue.json"));
    argv.extend(["--out".into(), output.to_string_lossy().into()]);
    assert_eq!(run(&argv, None).status.code(), Some(2));
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        "previous receipt\n"
    );
}

#[test]
fn a_non_ascii_output_path_prints_verbatim() {
    // Python on a cp1252 stdout escaped this line and noted it on stderr; the
    // binary writes UTF-8, so the exact path reaches stdout and stderr is empty.
    let dir = tempdir().unwrap();
    let output = dir.path().join("M\u{0101}ori Trust").join("demo");
    let done = run(&compare_argv(&output), None);
    assert_eq!(done.status.code(), Some(0));
    assert!(output.join("impact-queue.json").is_file());
    assert!(stdout(&done).contains("M\u{0101}ori Trust"));
    assert_eq!(stderr(&done), "");
}

#[test]
fn a_malformed_command_line_is_argparse_not_a_blocked_line() {
    let done = cli(&["compare", "--baseline", "some-path.json"], None);
    assert_eq!(done.status.code(), Some(2));
    let error = stderr(&done);
    assert!(error.starts_with("usage:"));
    assert!(!error.contains("blocked:"));
}

#[test]
fn occupied_output_paths_are_blocked() {
    let dir = tempdir().unwrap();
    let occupied = dir.path().join("not-a-directory");
    std::fs::write(&occupied, "already here\n").unwrap();
    let done = run(&compare_argv(&occupied), None);
    assert_eq!(done.status.code(), Some(2));
    assert!(stderr(&done).contains("blocked: the output directory could not be written:"));

    assert_eq!(
        run(&compare_argv(&dir.path().join("queue")), None)
            .status
            .code(),
        Some(0)
    );
    let queue = dir.path().join("queue").join("impact-queue.json");
    let done = cli(
        &[
            "exposure",
            "--queue",
            &queue.to_string_lossy(),
            "--profiles",
            &sample("profiles", "sample-client-profiles.json").to_string_lossy(),
            "--out",
            &occupied.to_string_lossy(),
        ],
        None,
    );
    assert_eq!(done.status.code(), Some(2));
    assert!(stderr(&done).contains("blocked: the output directory could not be written:"));

    let directory = dir.path().join("validation-out");
    std::fs::create_dir(&directory).unwrap();
    let mut argv = review_argv(&queue);
    argv.extend(["--out".into(), directory.to_string_lossy().into()]);
    let done = run(&argv, None);
    assert_eq!(done.status.code(), Some(2));
    assert!(stderr(&done).contains("blocked: The validation output must not exist;"));

    let mut argv = review_argv(&queue);
    argv.extend([
        "--out".into(),
        occupied.join("receipt.json").to_string_lossy().into(),
    ]);
    let done = run(&argv, None);
    assert_eq!(done.status.code(), Some(2));
    assert!(stderr(&done).contains("blocked: the validation output file could not be written:"));
}

#[test]
fn validate_review_without_out_only_reports() {
    let dir = tempdir().unwrap();
    assert_eq!(
        run(&compare_argv(&dir.path().join("queue")), None)
            .status
            .code(),
        Some(0)
    );
    let done = run(
        &review_argv(&dir.path().join("queue").join("impact-queue.json")),
        None,
    );
    assert_eq!(done.status.code(), Some(0));
    assert_eq!(
        stdout(&done),
        "tax-radar-au: DECISION_RECORDED; 1 decision(s)\n"
    );
    assert_eq!(
        names(&dir.path().join("queue")),
        ["impact-queue.json", "impact-queue.md"]
    );
}

/// A file opened for reading fails every write, as a closed pipe does.
fn unwritable(dir: &Path, name: &str) -> Stdio {
    let path = dir.join(name);
    std::fs::write(&path, b"").unwrap();
    Stdio::from(File::open(path).unwrap())
}

#[test]
fn an_unwritable_stdout_keeps_the_exit_status() {
    let dir = tempdir().unwrap();
    let output = dir.path().join("unwritable-stdout");
    let done = Command::new(env!("CARGO_BIN_EXE_tax-radar-au"))
        .args(compare_argv(&output))
        .stdout(unwritable(dir.path(), "read-only-stdout.txt"))
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(done.status.code(), Some(0), "{}", stderr(&done));
    assert!(output.join("impact-queue.json").is_file());
    let error = stderr(&done);
    assert!(error.contains("note: the run summary could not be written to stdout"));
    assert!(!error.contains("blocked:"));
}

#[test]
fn a_run_with_both_streams_gone_keeps_its_status() {
    let dir = tempdir().unwrap();
    let output = dir.path().join("queue");
    let status = Command::new(env!("CARGO_BIN_EXE_tax-radar-au"))
        .args(compare_argv(&output))
        .stdout(unwritable(dir.path(), "stdout.txt"))
        .stderr(unwritable(dir.path(), "stderr.txt"))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    assert!(output.join("impact-queue.json").is_file());
}

#[test]
fn a_blocked_run_exits_2_with_the_queue_still_written() {
    let dir = tempdir().unwrap();
    let mut observation = payload("observations", "sample-register-observation.json");
    observation["complete"] = json!(false);
    let observation_path = write(
        &dir.path().join("incomplete-observation.json"),
        &observation,
    );
    let mut argv = compare_argv(&dir.path().join("blocked-run"));
    argv[4] = observation_path.to_string_lossy().into();
    assert_eq!(run(&argv, None).status.code(), Some(2));
    let queue = load(&dir.path().join("blocked-run").join("impact-queue.json"));
    assert_eq!(queue["run_status"], "BLOCKED");
}

#[test]
fn compare_blocks_unhashable_observation_schema_version() {
    for version in [json!([]), json!({})] {
        let dir = tempdir().unwrap();
        let output = dir.path().join("output");
        let mut observation = payload("observations", "sample-register-observation.json");
        observation["schema_version"] = version;
        let mut argv = compare_argv(&output);
        argv[4] = write(&dir.path().join("observation.json"), &observation)
            .to_string_lossy()
            .into();
        let done = run(&argv, None);
        assert_eq!(done.status.code(), Some(2));
        assert_eq!(stdout(&done), "");
        assert_eq!(
            stderr(&done),
            "tax-radar-au: blocked: Only au-tax-register-observation.v1, \
             au-tax-register-observation.v2 and \
             au-tax-register-observation.v3 in synthetic mode are supported.\n"
        );
        assert!(!output.exists());
    }
}

#[test]
fn text_output_uses_the_platform_line_separator() {
    let dir = tempdir().unwrap();
    let done = run(&compare_argv(&dir.path().join("q")), None);
    let newline: &[u8] = if cfg!(windows) { b"\r\n" } else { b"\n" };
    assert!(done.stdout.ends_with(newline));
    let written = std::fs::read(dir.path().join("q").join("impact-queue.json")).unwrap();
    assert!(written.ends_with(&[b"}", newline].concat()));
    assert_eq!(written.windows(2).any(|w| w == b"\r\n"), cfg!(windows));
}

#[test]
fn help_lists_every_command() {
    let done = cli(&["--help"], None);
    assert_eq!(done.status.code(), Some(0));
    let help = stdout(&done);
    assert!(help.starts_with("usage: tax-radar-au [-h] {compare,validate-review,exposure} ...\n"));
    for command in ["compare", "validate-review", "exposure"] {
        assert!(help.contains(&format!("    {command}")));
    }
}

#[test]
fn cli_exposure_exit_status_follows_the_queue() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let mut argv = compare_argv(&root.join("queue"));
    argv[6] = sample("mappings", "sample-workpaper-map.json")
        .to_string_lossy()
        .into();
    assert_eq!(run(&argv, None).status.code(), Some(0));
    let queue = root.join("queue").join("impact-queue.json");
    let profiles = sample("profiles", "sample-client-profiles.json");
    let done = cli(
        &[
            "exposure",
            "--queue",
            &queue.to_string_lossy(),
            "--profiles",
            &profiles.to_string_lossy(),
            "--out",
            "report",
        ],
        Some(root),
    );
    assert_eq!(done.status.code(), Some(0));
    let written = load(&root.join("report").join("client-exposure.json"));
    assert!(
        written["exposure_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    let markdown = text(&std::fs::read(root.join("report").join("client-exposure.md")).unwrap());
    assert_eq!(
        markdown,
        tax_radar_au::radar::exposure::render_exposure_markdown(&radar_json(&written))
    );

    let mut observation = payload("observations", "sample-register-observation.json");
    observation["complete"] = json!(false);
    let mut argv = compare_argv(&root.join("blocked"));
    argv[4] = write(&root.join("blocked-observation.json"), &observation)
        .to_string_lossy()
        .into();
    assert_eq!(run(&argv, None).status.code(), Some(2));
    let blocked = root.join("blocked").join("impact-queue.json");
    let done = cli(
        &[
            "exposure",
            "--queue",
            &blocked.to_string_lossy(),
            "--profiles",
            &profiles.to_string_lossy(),
            "--out",
            "blocked-report",
        ],
        Some(root),
    );
    assert_eq!(done.status.code(), Some(2));
}

#[test]
fn cli_exposure_reports_bad_inputs_as_blocked() {
    let dir = tempdir().unwrap();
    let profiles = sample("profiles", "sample-client-profiles.json");
    let missing = dir.path().join("missing.json");
    let done = cli(
        &[
            "exposure",
            "--queue",
            &missing.to_string_lossy(),
            "--profiles",
            &profiles.to_string_lossy(),
            "--out",
            &dir.path().join("x").to_string_lossy(),
        ],
        None,
    );
    assert_eq!(done.status.code(), Some(2));
    assert!(stderr(&done).contains("tax-radar-au: blocked:"));

    assert_eq!(
        run(&compare_argv(&dir.path().join("queue")), None)
            .status
            .code(),
        Some(0)
    );
    let queue = dir.path().join("queue").join("impact-queue.json");
    let base = load(&profiles);
    let mut cases: Vec<(String, &str)> = Vec::new();
    for control in ["\\u0000", "\\t", "\\u001b", "\\u007f"] {
        let mut payload = base.clone();
        payload["profiles_version"] = json!("fabricated-privateCTRLversion");
        cases.push((
            payload.to_string().replace("CTRL", control),
            "profiles_version must be a non-empty string without control characters",
        ));
    }
    let mut payload = base.clone();
    payload["profiles_version"] = json!("vSURROGATE");
    cases.push((
        payload.to_string().replace("SURROGATE", "\\ud800"),
        "tax-radar-au: blocked:",
    ));
    let mut payload = base.clone();
    payload["profiles"][0]["skill_refs"] = json!(["bas-SURROGATE"]);
    cases.push((
        payload.to_string().replace("SURROGATE", "\\ud800"),
        "tax-radar-au: blocked:",
    ));
    for (document, expected) in cases {
        let path = dir.path().join("profiles.json");
        std::fs::write(&path, &document).unwrap();
        let out = dir.path().join("report");
        let done = cli(
            &[
                "exposure",
                "--queue",
                &queue.to_string_lossy(),
                "--profiles",
                &path.to_string_lossy(),
                "--out",
                &out.to_string_lossy(),
            ],
            None,
        );
        assert_eq!(done.status.code(), Some(2));
        let error = stderr(&done);
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("fabricated-private"));
        assert!(!out.exists());
    }
}

#[cfg(unix)]
#[test]
fn cli_refuses_a_fifo_markdown_companion_without_blocking() {
    for command in ["exposure", "validate-review"] {
        let dir = tempdir().unwrap();
        assert_eq!(
            run(&compare_argv(&dir.path().join("queue")), None)
                .status
                .code(),
            Some(0)
        );
        let queue = dir.path().join("queue").join("impact-queue.json");
        let markdown = dir.path().join("queue").join("impact-queue.md");
        std::fs::remove_file(&markdown).unwrap();
        let fifo = std::ffi::CString::new(markdown.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: the path is a valid NUL-terminated string.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let out = dir.path().join("report");
        let mut args = vec![
            command.to_string(),
            "--queue".into(),
            queue.to_string_lossy().into(),
            "--out".into(),
            out.to_string_lossy().into(),
        ];
        if command == "exposure" {
            args.extend([
                "--profiles".into(),
                sample("profiles", "sample-client-profiles.json")
                    .to_string_lossy()
                    .into(),
            ]);
        } else {
            args.extend([
                "--decision".into(),
                sample("decisions", "sample-technical-review.json")
                    .to_string_lossy()
                    .into(),
            ]);
        }
        let mut child = Command::new(env!("CARGO_BIN_EXE_tax-radar-au"))
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        while child.try_wait().unwrap().is_none() {
            if started.elapsed() > std::time::Duration::from_secs(10) {
                let _ = child.kill();
                panic!("opening the Markdown FIFO blocked");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let done = child.wait_with_output().unwrap();
        assert_eq!(done.status.code(), Some(2));
        assert!(stderr(&done).contains("Queue Markdown could not be read; regenerate the pair."));
        assert!(!out.exists());
    }
}
