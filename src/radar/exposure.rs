//! List pseudonymous client profiles whose workflows an intact impact queue names.
//!
//! The queue records which workflows a source change may touch. A profiles file
//! records, under a code rather than a name, which workflows a firm runs for each
//! client. A profile is listed against a queue item only when one of the item's
//! candidate skill references appears in the profile's own list: exact membership,
//! no inference from attributes. A listed profile is a prompt to review that
//! client's work, never a finding that the client is affected.
use std::collections::{BTreeSet, HashMap, HashSet};

use super::monitor::{QUEUE_FIELDS, verify_queue_integrity, verify_queue_markdown};
use super::persist::{output_paths, refuse_input_overwrite, write_queue_files};
use super::util::{SourceSnapshot, load_json_exact, safe_markdown, sha256_json};
use super::{Error, Result};
use crate::pyjson::{Json, Layout, PyStr, dumps};
use crate::pypath::PyPath;

const PROFILES_SCHEMA: &str = "au-tax-client-profiles.v1";
const EXPOSURE_SCHEMA: &str = "au-tax-client-exposure.v1";
const MATCHING_BASIS: &str = "exact_candidate_skill_ref_in_profile_skill_refs";
const EXPOSURE_STEM: &str = "client-exposure";
const MAX_QUEUE_BYTES: u64 = 50_000_000;
const MAX_PROFILE_BYTES: u64 = 4_000_000;
const MAX_PROFILES: usize = 10_000;
const MAX_SKILL_REFS: usize = 200;
const MAX_SKILL_REF_CHARS: usize = 200;
const MAX_MATCHES: usize = 200_000;
const MAX_ASSOCIATIONS: usize = 200_000;
const MAX_MATCH_WORK: usize = 1_000_000;
const LIMITATIONS: [&str; 3] = [
    "A listed profile is a prompt to review that client's work, not a finding that the client is affected or that a source change has any legal effect.",
    "Matching is exact skill-reference membership. A profile that lists none of an item's skills is not thereby shown to be unaffected: the mapping may be incomplete.",
    "The impact queue is a synthetic metadata review. When the profiles describe real clients, this report is confidential client information.",
];

fn control(points: &[u32]) -> bool {
    points.iter().any(|&cp| cp < 32 || cp == 127)
}

/// A string with no lone surrogate, which JSON admits but the UTF-8 report cannot hold.
fn text(value: &Json) -> Option<&str> {
    match value {
        Json::Str(PyStr::Text(s)) => Some(s),
        _ => None,
    }
}

fn skill_ref(value: &Json, field: &str) -> Result<String> {
    let refuse = || {
        Error::monitor(format!(
            "{field} must be a skill reference of at most {MAX_SKILL_REF_CHARS} characters \
             without surrounding spaces, control characters or lone surrogates."
        ))
    };
    let Json::Str(string) = value else {
        return Err(refuse());
    };
    let points = string.code_points();
    let space = |cp: &u32| char::from_u32(*cp).is_some_and(crate::py::is_space);
    let padded = points.first().is_some_and(space) || points.last().is_some_and(space);
    if points.is_empty() || padded || points.len() > MAX_SKILL_REF_CHARS || control(&points) {
        return Err(refuse());
    }
    text(value).map(str::to_string).ok_or_else(refuse)
}

/// A pseudonymous code: `[A-Za-z0-9][A-Za-z0-9._:-]{0,63}`.
fn profile_id(value: &Json) -> Option<&str> {
    let id = value.as_str()?;
    let bytes = id.as_bytes();
    let ok = (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b".:_-".contains(b));
    ok.then_some(id)
}

type Profiles = (String, Vec<(String, Vec<String>)>);

fn load_profiles(snapshot: &SourceSnapshot) -> Result<Profiles> {
    let raw = load_json_exact(
        snapshot,
        &["schema_version", "profiles_version", "profiles"],
        "client profiles",
    )?;
    if raw.at("schema_version").as_str() != Some(PROFILES_SCHEMA) {
        return Err(Error::monitor(
            "Client profiles have an unsupported schema.",
        ));
    }
    let version = raw.at("profiles_version");
    let valid_version = match version {
        Json::Str(s) => {
            let points = s.code_points();
            let blank = points
                .iter()
                .all(|cp| char::from_u32(*cp).is_some_and(crate::py::is_space));
            !blank && !control(&points) && text(version).is_some()
        }
        _ => false,
    };
    if !valid_version {
        return Err(Error::monitor(
            "Client profiles profiles_version must be a non-empty string \
             without control characters or lone surrogates.",
        ));
    }
    let entries = match raw.at("profiles") {
        Json::Array(entries) if !entries.is_empty() => entries,
        _ => {
            return Err(Error::monitor(
                "Client profiles must contain a non-empty profiles list.",
            ));
        }
    };
    if entries.len() > MAX_PROFILES {
        return Err(Error::monitor(format!(
            "Client profiles exceed {MAX_PROFILES} profiles."
        )));
    }
    let mut profiles = Vec::new();
    let mut seen = HashSet::new();
    // Messages name positions, never values: a profile file can describe real
    // clients, and an error message travels further than the report does.
    for (index, entry) in entries.iter().enumerate().map(|(i, e)| (i + 1, e)) {
        if !entry.has_exactly(&["profile_id", "skill_refs"]) {
            return Err(Error::monitor(format!(
                "Client profile {index} must contain exactly: profile_id, skill_refs."
            )));
        }
        let Some(id) = profile_id(entry.at("profile_id")) else {
            return Err(Error::monitor(format!(
                "Client profile {index} profile_id must be a code of up to 64 letters, digits \
                 and . _ : - characters, with no spaces."
            )));
        };
        if !seen.insert(id.to_string()) {
            return Err(Error::monitor(format!(
                "Client profile {index} repeats an earlier profile_id."
            )));
        }
        let refs = match entry.at("skill_refs") {
            Json::Array(refs) if !refs.is_empty() && refs.len() <= MAX_SKILL_REFS => refs,
            _ => {
                return Err(Error::monitor(format!(
                    "Client profile {index} skill_refs must list 1 to {MAX_SKILL_REFS} skill references."
                )));
            }
        };
        let mut cleaned: Vec<String> = Vec::new();
        for (ref_index, value) in refs.iter().enumerate().map(|(i, r)| (i + 1, r)) {
            let value = skill_ref(
                value,
                &format!("Client profile {index} skill reference {ref_index}"),
            )?;
            if cleaned.contains(&value) {
                return Err(Error::monitor(format!(
                    "Client profile {index} repeats a skill reference."
                )));
            }
            cleaned.push(value);
        }
        profiles.push((id.to_string(), cleaned));
    }
    Ok((text(version).unwrap_or_default().to_string(), profiles))
}

fn exposure_status(mapping_status: &str, matched: bool) -> &'static str {
    match mapping_status {
        "NOT_EVALUATED" => "NOT_EVALUATED",
        "UNMAPPED_SOURCE" => "UNMAPPED_SOURCE",
        _ if matched => "PROFILES_MATCHED",
        _ => "NO_PROFILE_MATCH",
    }
}

fn sorted_list(values: &BTreeSet<String>) -> Json {
    Json::Array(values.iter().map(|v| Json::str(v)).collect())
}

/// Build the exposure report from an intact queue and a profiles file.
///
/// Each input is read once, and the bytes that are hashed are the bytes that
/// are parsed. The queue must pass the same complete checks validate-review
/// applies, including the recomputed digests and the Markdown companion.
pub fn exposure(queue_path: &PyPath, profiles_path: &PyPath) -> Result<Json> {
    let queue_source = SourceSnapshot::capture(queue_path, "impact queue", MAX_QUEUE_BYTES)?;
    let queue = load_json_exact(&queue_source, &QUEUE_FIELDS, "impact queue")?;
    verify_queue_integrity(&queue)?;
    verify_queue_markdown(queue_path, &queue)?;
    let profiles_source =
        SourceSnapshot::capture(profiles_path, "client profiles", MAX_PROFILE_BYTES)?;
    let (version, profiles) = load_profiles(&profiles_source)?;

    if profiles.iter().map(|(_, refs)| refs.len()).sum::<usize>() > MAX_ASSOCIATIONS {
        return Err(Error::monitor(format!(
            "Profiles exceed {MAX_ASSOCIATIONS} skill associations; split the profiles file."
        )));
    }
    let mut by_skill: HashMap<&str, Vec<&str>> = HashMap::new();
    for (id, refs) in &profiles {
        for skill in refs {
            by_skill.entry(skill).or_default().push(id);
        }
    }

    let mut items = Vec::new();
    let mut matched_profiles: HashSet<&str> = HashSet::new();
    let mut match_count = 0usize;
    let mut work = 0usize;
    let queue_items = match queue.at("items") {
        Json::Array(items) => items.as_slice(),
        _ => &[],
    };
    for item in queue_items {
        let mut found: HashMap<&str, (BTreeSet<String>, BTreeSet<String>)> = HashMap::new();
        if let Json::Array(candidates) = item.at("impact_candidates") {
            for candidate in candidates {
                let skill = candidate.at("skill_ref").as_str().unwrap_or_default();
                let mapping_id = candidate.at("mapping_id").as_str().unwrap_or_default();
                for &id in by_skill.get(skill).map_or(&[][..], Vec::as_slice) {
                    work += 1;
                    if work > MAX_MATCH_WORK {
                        return Err(Error::monitor(format!(
                            "Exposure exceeds {MAX_MATCH_WORK} candidate/profile visits; split the inputs."
                        )));
                    }
                    if !found.contains_key(id) {
                        match_count += 1;
                        if match_count > MAX_MATCHES {
                            return Err(Error::monitor(format!(
                                "Exposure exceeds {MAX_MATCHES} profile matches; split the profiles file."
                            )));
                        }
                    }
                    let (skills, mappings) = found.entry(id).or_default();
                    skills.insert(skill.to_string());
                    mappings.insert(mapping_id.to_string());
                }
            }
        }
        matched_profiles.extend(found.keys().copied());
        let mut ids: Vec<&str> = found.keys().copied().collect();
        ids.sort_unstable();
        let matches = ids
            .into_iter()
            .map(|id| {
                let (skills, mappings) = &found[id];
                Json::obj([
                    ("profile_id", Json::str(id)),
                    ("matched_skill_refs", sorted_list(skills)),
                    ("mapping_ids", sorted_list(mappings)),
                ])
            })
            .collect();
        let mapping_status = item.at("mapping_status").as_str().unwrap_or_default();
        items.push(Json::obj([
            ("item_id", item.at("item_id").clone()),
            ("state", item.at("state").clone()),
            ("change_kind", item.at("change_kind").clone()),
            ("mapping_status", item.at("mapping_status").clone()),
            (
                "exposure_status",
                Json::str(exposure_status(mapping_status, !found.is_empty())),
            ),
            ("source", item.at("source").clone()),
            ("matches", Json::Array(matches)),
            ("limitations", item.at("limitations").clone()),
        ]));
    }
    let mut unmatched: Vec<&str> = profiles
        .iter()
        .map(|(id, _)| id.as_str())
        .filter(|id| !matched_profiles.contains(id))
        .collect();
    unmatched.sort_unstable();
    let prefixed = |s: &str| Json::str(&format!("sha256:{s}"));
    let mut report = Json::obj([
        ("schema_version", Json::str(EXPOSURE_SCHEMA)),
        ("matching_basis", Json::str(MATCHING_BASIS)),
        (
            "queue",
            Json::obj([
                ("run_id", queue.at("run_id").clone()),
                ("queue_digest", queue.at("queue_digest").clone()),
                ("file_sha256", prefixed(&queue_source.sha256)),
                ("mode", queue.at("mode").clone()),
                ("run_status", queue.at("run_status").clone()),
                (
                    "observation_complete",
                    queue.at("observation").at("complete").clone(),
                ),
            ]),
        ),
        (
            "profiles",
            Json::obj([
                ("profiles_version", Json::str(&version)),
                ("file_sha256", prefixed(&profiles_source.sha256)),
                ("profile_count", Json::Int(profiles.len().into())),
            ]),
        ),
        ("items", Json::Array(items)),
        (
            "profiles_without_candidate_match",
            Json::Array(unmatched.into_iter().map(Json::str).collect()),
        ),
        (
            "limitations",
            Json::Array(LIMITATIONS.iter().map(|l| Json::str(l)).collect()),
        ),
    ]);
    let digest = format!("sha256:{}", sha256_json(&report));
    if let Json::Object(pairs) = &mut report {
        pairs.push((PyStr::Text("exposure_digest".into()), Json::str(&digest)));
    }
    Ok(report)
}

fn f(value: &Json) -> String {
    super::monitor::py_format(value)
}

fn md(value: &Json) -> String {
    safe_markdown(value.as_str().unwrap_or_default())
}

pub fn render_exposure_markdown(report: &Json) -> String {
    let queue = report.at("queue");
    let profiles = report.at("profiles");
    let mut lines: Vec<String> = vec![
        "# Client exposure report".into(),
        String::new(),
        format!("**Queue run status: {}**", f(queue.at("run_status"))),
        format!("Exposure digest: `{}`", f(report.at("exposure_digest"))),
        format!("Queue digest: `{}`", f(queue.at("queue_digest"))),
        format!(
            "Profiles file: `{}`, version {}, {} profile(s)",
            f(profiles.at("file_sha256")),
            md(profiles.at("profiles_version")),
            f(profiles.at("profile_count"))
        ),
        String::new(),
    ];
    let list = |value: &Json| match value {
        Json::Array(items) => items.clone(),
        _ => Vec::new(),
    };
    for limitation in list(report.at("limitations")) {
        lines.push(format!("- {}", f(&limitation)));
    }
    lines.extend([String::new(), "## Queue items".into(), String::new()]);
    let items = list(report.at("items"));
    if items.is_empty() {
        lines.push("The queue holds no items, so no profile is listed. This is not a statement about live law.".into());
    }
    for item in &items {
        lines.push(format!(
            "### {}: {} ({})",
            f(item.at("state")),
            f(item.at("change_kind")),
            f(item.at("exposure_status"))
        ));
        lines.push(String::new());
        let source = item.at("source");
        if matches!(source, Json::Null) {
            lines.push("- Source: none; the observation scope was incomplete.".into());
        } else {
            lines.push(format!(
                "- Source: {} ({}, {})",
                md(source.at("title")),
                md(source.at("register_id")),
                md(source.at("collection"))
            ));
        }
        lines.push(format!(
            "- Mapping status: {}",
            f(item.at("mapping_status"))
        ));
        for found in list(item.at("matches")) {
            let join = |key: &str| {
                list(found.at(key))
                    .iter()
                    .map(md)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            lines.push(format!(
                "- Profile {}: skills {} (mappings {})",
                md(found.at("profile_id")),
                join("matched_skill_refs"),
                join("mapping_ids")
            ));
        }
        if item.at("exposure_status").as_str() == Some("NO_PROFILE_MATCH") {
            lines.push("- No profile lists a candidate skill for this item.".into());
        }
        for limitation in list(item.at("limitations")) {
            lines.push(format!("- Limitation: {}", md(&limitation)));
        }
        lines.push(String::new());
    }
    lines.extend([
        "## Profiles without a candidate match".into(),
        String::new(),
    ]);
    let unmatched = list(report.at("profiles_without_candidate_match"));
    if unmatched.is_empty() {
        lines.push("Every profile matched at least one candidate.".into());
    } else {
        lines.extend(unmatched.iter().map(|id| format!("- {}", md(id))));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// Write the report pair without replacing the queue/profiles inputs, in that order.
pub fn write_exposure(
    report: &Json,
    output_dir: &PyPath,
    queue_path: &PyPath,
    profiles_path: &PyPath,
) -> Result<(PyPath, PyPath)> {
    let (json_path, markdown_path) = output_paths(output_dir, EXPOSURE_STEM)?;
    let companion = queue_path.with_suffix(".md").map_err(Error::monitor)?;
    refuse_input_overwrite(
        &[json_path, markdown_path],
        &[queue_path.clone(), companion, profiles_path.clone()],
    )?;
    write_queue_files(
        &format!("{}\n", dumps(report, Layout::Indented)),
        &render_exposure_markdown(report),
        output_dir,
        EXPOSURE_STEM,
        true,
    )
}
