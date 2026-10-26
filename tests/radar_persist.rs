//! Staged writes, receipts and bounded loading: ported from
//! tests/radar/test_security_remediation.py and test_radar_paths.py.
mod common;

use std::cell::{Cell, RefCell};
use std::io::ErrorKind;
use std::path::Path;
use std::rc::Rc;

use common::*;
use tax_radar_au::pypath::PyPath;
use tax_radar_au::radar::persist::{self, Fault, Step};
use tax_radar_au::radar::util::{self, SourceSnapshot};
use tax_radar_au::radar::{Error, monitor};
use tempfile::tempdir;

fn pair(dir: &Path, json: &str, markdown: &str) -> tax_radar_au::radar::Result<(PyPath, PyPath)> {
    persist::write_queue_files(json, markdown, &py(dir), "impact-queue", false)
}

fn read(path: &Path) -> String {
    text(&std::fs::read(path).unwrap())
}

/// Run `body` with a filesystem hook installed, clearing it afterwards.
fn hooked<T>(hook: impl FnMut(Step, &str, &str) -> Fault + 'static, body: impl FnOnce() -> T) -> T {
    persist::set_hook(hook);
    let out = body();
    persist::clear_hook();
    out
}

fn is_os(result: &tax_radar_au::radar::Result<impl std::fmt::Debug>, needle: &str) -> bool {
    matches!(result, Err(Error::Os(e)) if e.text.contains(needle))
}

#[test]
fn interrupt_after_completed_queue_promotion_restores_old_bytes() {
    let dir = tempdir().unwrap();
    let output = dir.path().join("queue");
    pair(&output, "old json", "old markdown").unwrap();
    let done = Rc::new(Cell::new(false));
    let flag = done.clone();
    let result = hooked(
        move |step, _src, dst| {
            if step == Step::Replace && !flag.get() && dst.ends_with("impact-queue.json") {
                flag.set(true);
                return Fault::FailAfter(ErrorKind::Interrupted);
            }
            Fault::Proceed
        },
        || pair(&output, "new json", "new markdown"),
    );
    assert!(result.is_err() && done.get());
    assert_eq!(read(&output.join("impact-queue.json")), "old json");
    assert_eq!(read(&output.join("impact-queue.md")), "old markdown");
    assert_eq!(partials(&output), Vec::<String>::new());
}

#[test]
fn interrupt_between_backup_and_promotion_restores_the_pair() {
    let dir = tempdir().unwrap();
    let output = dir.path().join("pair");
    pair(&output, "old json", "old markdown").unwrap();
    let done = Rc::new(Cell::new(false));
    let flag = done.clone();
    let result = hooked(
        move |step, src, dst| {
            if step == Step::Replace
                && !flag.get()
                && src.ends_with(".partial")
                && dst.ends_with("impact-queue.json")
            {
                flag.set(true);
                return Fault::Fail(ErrorKind::Interrupted);
            }
            Fault::Proceed
        },
        || pair(&output, "new json", "new markdown"),
    );
    assert!(result.is_err());
    assert_eq!(read(&output.join("impact-queue.json")), "old json");
    assert_eq!(read(&output.join("impact-queue.md")), "old markdown");
    assert_eq!(partials(&output), Vec::<String>::new());
}

#[test]
fn backup_link_failure_keeps_the_original_path_and_bytes() {
    let dir = tempdir().unwrap();
    let output = dir.path().join("queue");
    pair(&output, "old json", "old markdown").unwrap();
    let json_path = output.join("impact-queue.json");
    let checked = json_path.clone();
    let result = hooked(
        move |step, _src, _dst| {
            if step == Step::Link {
                assert_eq!(read(&checked), "old json");
                return Fault::Fail(ErrorKind::Unsupported);
            }
            Fault::Proceed
        },
        || pair(&output, "new json", "new markdown"),
    );
    assert!(is_os(&result, "simulated Link failure"));
    assert_eq!(read(&json_path), "old json");
    assert_eq!(read(&output.join("impact-queue.md")), "old markdown");
    assert_eq!(partials(&output), Vec::<String>::new());
}

#[test]
fn all_backups_are_prepared_before_any_promotion() {
    for json_exists in [false, true] {
        let dir = tempdir().unwrap();
        let output = dir.path().join("queue");
        std::fs::create_dir(&output).unwrap();
        let json_path = output.join("impact-queue.json");
        let markdown_path = output.join("impact-queue.md");
        if json_exists {
            std::fs::write(&json_path, b"old json").unwrap();
        }
        std::fs::write(&markdown_path, b"old markdown").unwrap();
        let replaced = Rc::new(Cell::new(0));
        let count = replaced.clone();
        let result = hooked(
            move |step, src, _dst| match step {
                Step::Link if src.ends_with("impact-queue.md") => {
                    Fault::Fail(ErrorKind::Unsupported)
                }
                Step::Replace => {
                    count.set(count.get() + 1);
                    Fault::Proceed
                }
                _ => Fault::Proceed,
            },
            || pair(&output, "new json", "new markdown"),
        );
        assert!(is_os(&result, "simulated Link failure"));
        assert_eq!(replaced.get(), 0);
        assert_eq!(json_path.exists(), json_exists);
        if json_exists {
            assert_eq!(std::fs::read(&json_path).unwrap(), b"old json");
        }
        assert_eq!(std::fs::read(&markdown_path).unwrap(), b"old markdown");
        assert_eq!(partials(&output), Vec::<String>::new());
    }
}

#[test]
fn backup_retains_the_original_inode_and_access_controls() {
    let dir = tempdir().unwrap();
    let destination = dir.path().join("old.json");
    std::fs::write(&destination, b"old private bytes").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o640)).unwrap();
    }
    let original = tax_radar_au::pyos::file_id(&destination.to_string_lossy()).unwrap();
    let original_meta = std::fs::metadata(&destination).unwrap();
    let backup = persist::backup_existing(&py(&destination))
        .unwrap()
        .unwrap();
    assert_eq!(
        tax_radar_au::pyos::file_id(&backup.display()).unwrap(),
        original
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let retained = std::fs::metadata(backup.fs()).unwrap();
        assert_eq!(
            (retained.uid(), retained.gid(), retained.mode()),
            (
                original_meta.uid(),
                original_meta.gid(),
                original_meta.mode()
            )
        );
    }
    let _ = original_meta;
    persist::restore_quietly(&backup, &py(&destination));
    assert_eq!(std::fs::read(&destination).unwrap(), b"old private bytes");
    assert!(!backup.fs().exists());
}

#[test]
fn a_failed_rollback_still_raises_the_original_commit_failure() {
    let dir = tempdir().unwrap();
    let output = dir.path().join("pair");
    pair(&output, "old json\n", "old markdown\n").unwrap();
    let failing = Rc::new(Cell::new(false));
    let flag = failing.clone();
    let result = hooked(
        move |step, _src, dst| {
            if step == Step::Replace {
                if dst.ends_with(".md") {
                    flag.set(true);
                }
                if flag.get() {
                    return Fault::Fail(ErrorKind::PermissionDenied);
                }
            }
            Fault::Proceed
        },
        || pair(&output, "new json\n", "new markdown\n"),
    );
    assert!(is_os(&result, "simulated Replace failure"));
    // The restore could not run, so both parked originals stay beside the pair.
    assert_eq!(read(&output.join("impact-queue.json")), "new json\n");
    assert_eq!(partials(&output).len(), 2);
}

#[test]
fn a_directory_at_a_queue_destination_is_refused() {
    let dir = tempdir().unwrap();
    let output = dir.path().join("pair");
    std::fs::create_dir_all(output.join("impact-queue.json")).unwrap();
    refused(
        pair(&output, "json\n", "markdown\n"),
        "^queue destinations must be ordinary files.$",
    );
    assert_eq!(partials(&output), Vec::<String>::new());
}

#[test]
fn a_failed_first_commit_removes_the_new_file() {
    let dir = tempdir().unwrap();
    let output = dir.path().join("pair");
    let result = hooked(
        |step, _src, dst| {
            if step == Step::Replace && dst.ends_with(".md") {
                return Fault::Fail(ErrorKind::PermissionDenied);
            }
            Fault::Proceed
        },
        || pair(&output, "json\n", "markdown\n"),
    );
    assert!(is_os(&result, "simulated Replace failure"));
    assert_eq!(names(&output), Vec::<String>::new());
}

#[test]
fn a_staging_write_that_fails_leaves_no_partial() {
    let dir = tempdir().unwrap();
    let output = dir.path().join("pair");
    let result = hooked(
        |step, _src, _dst| {
            if step == Step::Write {
                Fault::Fail(ErrorKind::OutOfMemory)
            } else {
                Fault::Proceed
            }
        },
        || pair(&output, "json\n", "markdown\n"),
    );
    assert!(is_os(&result, "simulated Write failure"));
    assert_eq!(names(&output), Vec::<String>::new());
}

#[test]
fn a_second_write_removes_the_parked_originals() {
    let dir = tempdir().unwrap();
    let output = dir.path().join("pair");
    pair(&output, "old json\n", "old markdown\n").unwrap();
    pair(&output, "new json\n", "new markdown\n").unwrap();
    assert_eq!(names(&output), ["impact-queue.json", "impact-queue.md"]);
    assert_eq!(read(&output.join("impact-queue.md")), "new markdown\n");
}

#[test]
fn staged_writes_and_receipts_publish_their_text() {
    let dir = tempdir().unwrap();
    let (json, markdown) = pair(&dir.path().join("pair"), "{}\n", "Literal Markdown\n").unwrap();
    let receipt =
        persist::write_receipt("{}\n", &py(&dir.path().join("receipt.json")), &[]).unwrap();
    assert_eq!(read(&json.fs()), "{}\n");
    assert_eq!(read(&markdown.fs()), "Literal Markdown\n");
    assert_eq!(read(&receipt.fs()), "{}\n");
    assert_eq!(partials(dir.path()), Vec::<String>::new());
}

#[test]
fn a_receipt_stream_failure_keeps_the_original_error() {
    for cleanup in [ErrorKind::NotFound, ErrorKind::PermissionDenied] {
        let dir = tempdir().unwrap();
        let receipt = dir.path().join("receipt.json");
        let result = hooked(
            move |step, _src, _dst| match step {
                Step::Write => Fault::Fail(ErrorKind::OutOfMemory),
                Step::Unlink => Fault::Fail(cleanup),
                _ => Fault::Proceed,
            },
            || persist::write_receipt("{}\n", &py(&receipt), &[]),
        );
        assert!(is_os(&result, "simulated Write failure"), "{result:?}");
        assert!(!receipt.exists());
    }
}

#[test]
fn a_published_receipt_tolerates_an_already_removed_staging_file() {
    let dir = tempdir().unwrap();
    let receipt = dir.path().join("receipt.json");
    let result = hooked(
        |step, _src, _dst| {
            if step == Step::Unlink {
                Fault::Fail(ErrorKind::NotFound)
            } else {
                Fault::Proceed
            }
        },
        || persist::write_receipt("{}\n", &py(&receipt), &[]),
    );
    assert_eq!(result.unwrap().fs(), receipt);
    assert_eq!(read(&receipt), "{}\n");
}

#[test]
fn a_published_receipt_reports_a_staging_file_it_cannot_remove() {
    let dir = tempdir().unwrap();
    let receipt = dir.path().join("receipt.json");
    let result = hooked(
        |step, _src, _dst| {
            if step == Step::Unlink {
                Fault::Fail(ErrorKind::PermissionDenied)
            } else {
                Fault::Proceed
            }
        },
        || persist::write_receipt("{}\n", &py(&receipt), &[]),
    );
    refused(
        result,
        "^Validation receipt was published, but its staging file could not be removed.$",
    );
    assert_eq!(read(&receipt), "{}\n");
}

#[test]
fn a_receipt_sync_failure_does_not_publish_partial_bytes() {
    let dir = tempdir().unwrap();
    let result = hooked(
        |step, _src, _dst| {
            if step == Step::Write {
                Fault::FailAfter(ErrorKind::Other)
            } else {
                Fault::Proceed
            }
        },
        || persist::write_receipt("{}\n", &py(&dir.path().join("receipt.json")), &[]),
    );
    assert!(result.is_err());
    assert_eq!(names(dir.path()), Vec::<String>::new());
}

#[test]
fn a_receipt_destination_race_cannot_overwrite_another_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("receipt.json");
    let competitor = path.clone();
    let publish = if cfg!(windows) {
        Step::Rename
    } else {
        Step::Link
    };
    let result = hooked(
        move |step, _src, dst| {
            if step == publish && dst.ends_with("receipt.json") {
                std::fs::write(&competitor, "another writer\n").unwrap();
            }
            Fault::Proceed
        },
        || persist::write_receipt("{}\n", &py(&path), &[]),
    );
    match result {
        Err(Error::Os(error)) => assert_eq!(error.kind, ErrorKind::AlreadyExists),
        other => panic!("expected FileExistsError, got {other:?}"),
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "another writer\n");
    assert_eq!(names(dir.path()), ["receipt.json"]);
}

#[test]
fn receipt_relative_escape_is_refused_before_parents() {
    let dir = tempdir().unwrap();
    let working = dir.path().join("working");
    std::fs::create_dir(&working).unwrap();
    // The working directory is per process, so the CLI exercises the relative form.
    assert_eq!(
        run(&compare_argv(&working.join("queue")), Some(&working))
            .status
            .code(),
        Some(0)
    );
    let done = cli(
        &[
            "validate-review",
            "--queue",
            &working
                .join("queue")
                .join("impact-queue.json")
                .to_string_lossy(),
            "--decision",
            &sample("decisions", "sample-technical-review.json").to_string_lossy(),
            "--out",
            "../outside/receipt.json",
        ],
        Some(&working),
    );
    assert_eq!(done.status.code(), Some(2));
    assert!(text(&done.stderr).contains("output path must stay within"));
    assert!(!dir.path().join("outside").exists());
    let absolute = dir.path().join("explicit").join("receipt.json");
    assert_eq!(
        persist::write_receipt("{}\n", &py(&absolute), &[])
            .unwrap()
            .fs(),
        absolute
    );
    assert_eq!(read(&absolute), "{}\n");
}

#[test]
fn a_dangling_receipt_link_is_preserved() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("receipt.json");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(dir.path().join("absent.json"), &path);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(dir.path().join("absent.json"), &path);
    if made.is_err() {
        eprintln!("skipped: symbolic links unavailable");
        return;
    }
    refused(
        persist::write_receipt("{}\n", &py(&path), &[]),
        "must not exist",
    );
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!dir.path().join("absent.json").exists());
}

#[test]
fn relative_symlink_and_parent_components_use_one_interpretation() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("work");
    let deep = root.join("safe").join("deep");
    std::fs::create_dir_all(&deep).unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&deep, root.join("jump"))
        .and_then(|()| std::os::unix::fs::symlink(&outside, root.join("out")));
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(&deep, root.join("jump"))
        .and_then(|()| std::os::windows::fs::symlink_dir(&outside, root.join("out")));
    if made.is_err() {
        eprintln!("skipped: directory symlinks unavailable");
        return;
    }
    assert_eq!(
        run(&compare_argv(&root.join("q")), Some(&root))
            .status
            .code(),
        Some(0)
    );
    let done = cli(
        &[
            "validate-review",
            "--queue",
            &root.join("q").join("impact-queue.json").to_string_lossy(),
            "--decision",
            &sample("decisions", "sample-technical-review.json").to_string_lossy(),
            "--out",
            "jump/../out/receipt.json",
        ],
        Some(&root),
    );
    match done.status.code() {
        // POSIX resolves "jump" before "..", landing in safe/out.
        Some(0) => assert!(!read(&root.join("safe").join("out").join("receipt.json")).is_empty()),
        // Windows normalises ".." first, so "out" escapes to the outside directory.
        Some(2) => assert!(text(&done.stderr).contains("stay within")),
        other => panic!("unexpected exit {other:?}"),
    }
    assert_eq!(names(&outside), Vec::<String>::new());
}

#[test]
fn an_input_that_cannot_be_resolved_blocks_the_write() {
    let dir = tempdir().unwrap();
    let result = hooked(
        |step, _src, _dst| {
            if step == Step::Resolve {
                Fault::Fail(ErrorKind::Other)
            } else {
                Fault::Proceed
            }
        },
        || {
            persist::refuse_input_overwrite(
                &[py(&dir.path().join("out.json"))],
                &[py(&dir.path().join("in.json"))],
            )
        },
    );
    refused(
        result,
        "^Output paths could not be checked against input files.$",
    );
}

fn oversized(path: &Path) -> std::path::PathBuf {
    let file = std::fs::File::create(path).unwrap();
    file.set_len(util::MAX_JSON_BYTES + 1).unwrap();
    path.to_path_buf()
}

#[test]
fn json_depth_and_size_bounds() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("input.json");
    std::fs::write(&path, format!("{}0{}", "[".repeat(65), "]".repeat(65))).unwrap();
    refused(util::load_json_path(&py(&path), "input"), "nesting levels");
    std::fs::write(
        &path,
        json!({"literal": format!("{}\\\"", "[".repeat(1000))}).to_string(),
    )
    .unwrap();
    let loaded = value(&util::load_json_path(&py(&path), "input").unwrap());
    assert!(loaded["literal"].as_str().unwrap().starts_with('['));
    std::fs::write(&path, format!("{}0{}", "[".repeat(64), "]".repeat(64))).unwrap();
    assert!(util::load_json_path(&py(&path), "input").unwrap().truthy());

    let big = oversized(&dir.path().join("big.json"));
    let refusal = util::load_json_path(&py(&big), "input");
    assert!(matches!(
        &refusal,
        Err(Error::Monitor {
            too_large: true,
            ..
        })
    ));
    refused(refusal, "50000000");

    let snapshot = SourceSnapshot {
        path: py(&dir.path().join("probe.json")),
        content: vec![b' '; util::MAX_JSON_BYTES as usize + 1],
        sha256: String::new(),
    };
    refused(
        util::load_json(&snapshot, "probe"),
        "^probe exceeds 50000000 bytes.$",
    );
}

#[test]
fn every_compare_json_input_is_bounded() {
    for index in 0..3 {
        let dir = tempdir().unwrap();
        let mut inputs = [
            sample("baseline", "sample-sources.json"),
            sample("observations", "sample-register-observation.json"),
            sample("mappings", "sample-source-skill-map.json"),
        ];
        inputs[index] = oversized(&dir.path().join("oversized.json"));
        let result = compare_paths(&inputs[0], &inputs[1], &inputs[2]);
        assert!(matches!(
            result,
            Err(Error::Monitor {
                too_large: true,
                ..
            })
        ));
    }
}

#[test]
fn review_budget_blocks_before_receipt_creation() {
    for oversized_queue in [true, false] {
        let dir = tempdir().unwrap();
        assert_eq!(
            run(&compare_argv(&dir.path().join("q")), None)
                .status
                .code(),
            Some(0)
        );
        let mut queue = dir.path().join("q").join("impact-queue.json");
        let mut decision = sample("decisions", "sample-technical-review.json");
        let big = oversized(&dir.path().join("oversized.json"));
        if oversized_queue {
            queue = big;
        } else {
            decision = big;
        }
        let receipt = dir.path().join("new-parent").join("receipt.json");
        let done = cli(
            &[
                "validate-review",
                "--queue",
                &queue.to_string_lossy(),
                "--decision",
                &decision.to_string_lossy(),
                "--out",
                &receipt.to_string_lossy(),
            ],
            None,
        );
        assert_eq!(done.status.code(), Some(2));
        assert!(text(&done.stderr).contains("exceeds 50000000 bytes"));
        assert!(!receipt.parent().unwrap().exists());
    }
}

#[test]
fn metadata_is_literal_text_outside_code_spans() {
    let mut queue = queue();
    queue["items"][0]["source"]["title"] = json!("# **Title** _italic_ <img> ![link](url)");
    queue["items"][0]["source"]["register_id"] = json!("id`close`");
    let markdown = monitor::render_markdown(&radar_json(&queue));
    assert!(markdown.contains(r"\# \*\*Title\*\* \_italic\_ \<img\> \!\[link\]\(url\)"));
    assert!(markdown.contains(r"id\`close\`"));
    assert!(!markdown.contains(r"`id\`close\``"));
}

#[test]
fn workpaper_map_preserves_both_review_tasks_and_no_legal_conclusion() {
    let queue = compare_paths(
        &sample("baseline", "sample-sources.json"),
        &sample("observations", "sample-register-observation.json"),
        &sample("mappings", "sample-workpaper-map.json"),
    )
    .unwrap();
    let changed: Vec<&Value> = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["state"] == "OPEN")
        .collect();
    assert_eq!(changed.len(), 1);
    let candidates = changed[0]["impact_candidates"].as_array().unwrap();
    let mut skills: Vec<&str> = candidates
        .iter()
        .map(|c| c["skill_ref"].as_str().unwrap())
        .collect();
    skills.sort_unstable();
    assert_eq!(skills, ["bas-preparation", "cashflow-forecast"]);
    let questions: String = candidates
        .iter()
        .map(|c| c["review_question"].as_str().unwrap())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(questions.contains("6150.00 purchase tie-out"));
    assert!(questions.contains("preserve the original snapshot"));
    assert!(questions.contains("September 2099"));
    assert!(!changed[0]["limitations"].as_array().unwrap().is_empty());
}

#[test]
fn stored_queue_refusals_name_the_exact_condition() {
    type Mutation = fn(&mut Value);
    let cases: Vec<(Mutation, &str)> = vec![
        (
            |q| q["schema_version"] = json!("au-tax-impact-queue.v1"),
            "Queue or decision schema version is unsupported.",
        ),
        (
            |q| q["baseline"]["id"] = json!(format!("sha256:{}", "b".repeat(64))),
            "Impact queue baseline id must equal the baseline source digest.",
        ),
        (
            |q| q["observation"]["complete"] = json!("yes"),
            "Impact queue observation complete must be a boolean.",
        ),
        (
            |q| q["observation"]["id"] = json!(format!("sha256:{}", "b".repeat(64))),
            "Impact queue observation id must equal the observation source digest.",
        ),
        (
            |q| q["items"] = json!({}),
            "Impact queue items must be a list.",
        ),
        (
            |q| q["items"][0]["change_kind"] = json!("REPEALED"),
            "Impact queue item 1 has an unsupported change_kind.",
        ),
        (
            |q| q["items"][0]["source"]["observed_compilation"] = Value::Null,
            "Impact queue item 1 source must include observed_compilation for SUPERSEDED.",
        ),
        (
            |q| q["items"][0]["impact_candidates"] = json!({}),
            "Impact queue item 1 impact_candidates must be a list.",
        ),
        (
            |q| q["items"][0]["impact_candidates"][0]["mapping_basis"] = json!("title_match"),
            "Impact queue item 1 candidate 1 has an unsupported mapping_basis.",
        ),
        (
            |q| {
                let first = q["items"][0]["impact_candidates"][0].clone();
                q["items"][0]["impact_candidates"]
                    .as_array_mut()
                    .unwrap()
                    .push(first);
            },
            "Impact queue item 1 contains duplicate candidate mapping IDs.",
        ),
        (
            |q| q["items"][0]["mapping_status"] = json!("GUESSED"),
            "Impact queue item 1 has an unsupported mapping_status.",
        ),
        (
            |q| q["items"][0]["impact_candidates"] = json!([]),
            "Impact queue item 1 cannot be MAPPED without a candidate.",
        ),
        (
            |q| q["items"][0]["state"] = json!("BLOCKED"),
            "Impact queue item 1 change_kind must remain OPEN.",
        ),
        (
            |q| q["run_id"] = json!(format!("sha256:{}", "b".repeat(64))),
            "Impact queue run_id does not match its exact source digests.",
        ),
    ];
    for (mutate, message) in cases {
        let mut queue = queue();
        mutate(&mut queue);
        assert_eq!(
            monitor::verify_queue_integrity(&radar_json(&queue))
                .unwrap_err()
                .to_string(),
            message
        );
    }
}

#[test]
fn refusals_carry_their_exact_messages() {
    let dir = tempdir().unwrap();
    type Mutation = fn(&mut Value);
    let baseline_cases: Vec<(Mutation, &str)> = vec![
        (
            |b| b["source"] = json!("Somewhere else"),
            "Baseline must be a Federal Register title index with a titles list.",
        ),
        (
            |b| {
                b["titles"][0].as_object_mut().unwrap().remove("name");
            },
            "Baseline title 1 has an invalid shape.",
        ),
        (
            |b| b["titles"][0]["version_is_current"] = json!("yes"),
            "title 1 version_is_current must be a boolean.",
        ),
    ];
    for (mutate, message) in baseline_cases {
        let mut baseline = payload("baseline", "sample-sources.json");
        mutate(&mut baseline);
        assert_eq!(
            message_of(compare_fixtures(dir.path(), Some(&baseline), None, None)),
            message
        );
    }
    assert_eq!(
        message_of(compare_fixtures(dir.path(), None, Some(&json!([])), None)),
        "Register observation must be a JSON object with schema_version."
    );
    let observation_cases: Vec<(Mutation, &str)> = vec![
        (
            |o| o["complete"] = json!("true"),
            "Observation complete/observations fields are invalid.",
        ),
        (
            |o| {
                let first = o["observations"][0].clone();
                o["observations"].as_array_mut().unwrap().push(first);
            },
            "Observation contains duplicate register IDs.",
        ),
    ];
    for (mutate, message) in observation_cases {
        let mut observation = payload("observations", "sample-register-observation.json");
        mutate(&mut observation);
        assert_eq!(
            message_of(compare_fixtures(dir.path(), None, Some(&observation), None)),
            message
        );
    }
    let mapping_cases: Vec<(Mutation, &str)> = vec![
        (
            |m| m["schema_version"] = json!("au-tax-source-skill-map.v0"),
            "Source-to-skill map has an unsupported schema.",
        ),
        (
            |m| {
                m["entries"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("owner_role");
            },
            "Mapping entry 1 has an invalid shape.",
        ),
    ];
    for (mutate, message) in mapping_cases {
        let mut mapping = payload("mappings", "sample-source-skill-map.json");
        mutate(&mut mapping);
        assert_eq!(
            message_of(compare_fixtures(dir.path(), None, None, Some(&mapping))),
            message
        );
    }
    let queue = queue();
    for (mutate, message) in [
        (
            (|d: &mut Value| d["schema_version"] = json!("au-tax-technical-review.v1")) as Mutation,
            "Queue or decision schema version is unsupported.",
        ),
        (
            |d: &mut Value| d["decisions"] = json!([]),
            "Technical review decision must include at least one decision.",
        ),
    ] {
        let mut decision = matching_decision(&queue);
        mutate(&mut decision);
        assert_eq!(
            message_of(validate_payloads(dir.path(), &queue, &decision)),
            message
        );
    }
}

fn message_of(result: tax_radar_au::radar::Result<Value>) -> String {
    result.unwrap_err().to_string()
}

#[test]
fn a_fully_unchanged_observation_reports_no_change() {
    let dir = tempdir().unwrap();
    let mut observation = payload("observations", "sample-register-observation.json");
    clear_compilation(&mut observation["observations"][0]);
    observation["observations"][0]["state"] = json!("UNCHANGED");
    let queue = compare_fixtures(dir.path(), None, Some(&observation), None).unwrap();
    assert_eq!(queue["run_status"], "NO_CHANGE_DETECTED");
    assert_eq!(queue["items"], json!([]));
    assert!(monitor::render_markdown(&radar_json(&queue)).contains(
        "No changed items were identified within the complete synthetic observation scope."
    ));
}

#[test]
fn source_rules_for_scope_and_observed_compilations() {
    let mut queue = queue();
    queue["items"][0]["change_kind"] = json!("NO_LONGER_IN_FORCE");
    let message = monitor::verify_queue_integrity(&radar_json(&queue))
        .unwrap_err()
        .to_string();
    assert_eq!(
        message,
        "Impact queue item 1 source observed_compilation is unsupported for NO_LONGER_IN_FORCE."
    );

    let queue = queue_with_scope_source();
    let message = monitor::verify_queue_integrity(&radar_json(&queue))
        .unwrap_err()
        .to_string();
    assert_eq!(
        message,
        "Impact queue item 1 source must be null for INCOMPLETE_SCOPE."
    );
}

fn queue_with_scope_source() -> Value {
    let mut queue = queue();
    queue["items"][0]["change_kind"] = json!("INCOMPLETE_SCOPE");
    queue["items"][0]["source"] = json!({});
    queue
}

#[test]
fn hooks_are_per_thread_and_cleared() {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let seen = calls.clone();
    let dir = tempdir().unwrap();
    hooked(
        move |step, _src, _dst| {
            seen.borrow_mut().push(step);
            Fault::Proceed
        },
        || pair(&dir.path().join("pair"), "a\n", "b\n").unwrap(),
    );
    assert!(calls.borrow().contains(&Step::Replace));
    let before = calls.borrow().len();
    pair(&dir.path().join("pair"), "c\n", "d\n").unwrap();
    assert_eq!(calls.borrow().len(), before);
}
