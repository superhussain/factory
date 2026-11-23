//! The board as `serde_json::Value`, and Python's job patching and batch mirroring.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{json, Map, Value};

use super::consts::{BATCH_REF, IDEA, TERMINAL};
use super::exit::{fail, Res};
use super::http::api;
use super::pyjson::{dumps, loads, COMPACT};
use super::time::now;

/// Python truthiness of a value that may be missing.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
    }
}

pub fn load_board(text: Option<&str>) -> Res<Value> {
    match text.filter(|t| !t.is_empty()) {
        Some(text) => loads(text).map_err(|e| fail(format!("the board is not valid JSON: {e}"), 1)),
        None => Ok(json!({"updated": "", "jobs": []})),
    }
}

pub fn dump_board(v: &Value) -> String {
    dumps(v, COMPACT) + "\n"
}

pub fn read_board(home: &Path) -> Res<Value> {
    let r = api(home, "GET", "/api/board", None, &[200, 404], &[])?;
    let text = if r.status == 200 {
        Some(r.text()?)
    } else {
        None
    };
    load_board(text.as_deref())
}

/// RFC 7386 merge patch: null removes a key.
pub fn merge_patch(target: Value, patch: &Value) -> Value {
    let Value::Object(patch) = patch else {
        return patch.clone();
    };
    let mut target = match target {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    for (key, value) in patch {
        if value.is_null() {
            target.shift_remove(key);
        } else {
            let old = target.get(key).cloned().unwrap_or(Value::Null);
            target.insert(key.clone(), merge_patch(old, value));
        }
    }
    Value::Object(target)
}

fn jobs(data: &mut Value) -> &mut Vec<Value> {
    data["jobs"].as_array_mut().expect("board jobs is a list")
}

fn job_index(data: &Value, job_id: &str) -> Option<usize> {
    data["jobs"]
        .as_array()?
        .iter()
        .position(|j| j.get("id").and_then(Value::as_str) == Some(job_id))
}

/// Python's `patch_job`: merge patch into one job, stamp it, and move its riders. Returns the job.
pub fn patch_job(data: &mut Value, job_id: &str, patch: &Value) -> Value {
    let index = match job_index(data, job_id) {
        Some(i) => i,
        None => {
            jobs(data).push(json!({"id": job_id}));
            jobs(data).len() - 1
        }
    };
    let prev = jobs(data)[index].get("stage").cloned();
    if patch.is_object() {
        let job = std::mem::take(&mut jobs(data)[index]);
        jobs(data)[index] = merge_patch(job, patch);
    }
    let stamp = now();
    jobs(data)[index]["updated"] = Value::from(stamp.clone());
    data["updated"] = Value::from(stamp.clone());
    let job = jobs(data)[index].as_object_mut().unwrap();
    if truthy(patch.get("claim")) && !job.contains_key("started") {
        job.insert("started".into(), job["updated"].clone());
    }
    let stage = job.get("stage").cloned();
    if stage != prev {
        if stage
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(|s| TERMINAL.contains(&s))
        {
            job.insert("done".into(), job["updated"].clone());
        } else {
            job.shift_remove("done");
        }
    }
    let moved = stage != prev || patch.get("pr").is_some() || patch.get("needs").is_some();
    if moved && lead_of(&jobs(data)[index]).is_none() {
        mirror_batch(data, job_id);
    }
    jobs(data)[index].clone()
}

/// The id of the job this idea is batched into.
pub fn lead_of(j: &Value) -> Option<String> {
    j.get("refs")?
        .as_array()?
        .iter()
        .find(|r| r.get("kind").and_then(Value::as_str) == Some(BATCH_REF))
        .and_then(|r| r.get("id")?.as_str().map(String::from))
}

/// The lead ids of a refs list's batched-into refs.
pub fn batch_refs(refs: &Value) -> BTreeSet<String> {
    let Some(refs) = refs.as_array() else {
        return BTreeSet::new();
    };
    refs.iter()
        .filter(|r| r.get("kind").and_then(Value::as_str) == Some(BATCH_REF))
        .filter_map(|r| r.get("id")?.as_str().map(String::from))
        .collect()
}

pub fn riders_of(data: &Value, lead_id: &str) -> Vec<String> {
    data["jobs"]
        .as_array()
        .map(|jobs| {
            jobs.iter()
                .filter(|j| lead_of(j).as_deref() == Some(lead_id))
                .filter_map(|j| j.get("id")?.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn get_str<'a>(j: &'a Value, key: &str) -> Option<&'a str> {
    j.get(key).and_then(Value::as_str)
}

fn or_null(v: Option<&Value>) -> Value {
    v.cloned().unwrap_or(Value::Null)
}

/// Python's `mirror_batch`: move a lead's riders with its stage and PR, or back to idea when it closes.
pub fn mirror_batch(data: &mut Value, lead_id: &str) {
    let Some(lead) = job_index(data, lead_id).map(|i| data["jobs"][i].clone()) else {
        return;
    };
    for rider_id in riders_of(data, lead_id) {
        let Some(rider) = job_index(data, &rider_id).map(|i| data["jobs"][i].clone()) else {
            continue;
        };
        if get_str(&rider, "stage").is_some_and(|s| TERMINAL.contains(&s)) {
            continue;
        }
        let stacked = rider.get("stacked") == Some(&Value::Bool(true));
        let all_refs = rider["refs"].as_array().cloned().unwrap_or_default();
        let refs: Vec<Value> = all_refs
            .iter()
            .filter(|r| r.is_object() && r.get("kind").and_then(Value::as_str) != Some(BATCH_REF))
            .cloned()
            .collect();
        let refs_or_null = if refs.is_empty() {
            Value::Null
        } else {
            Value::Array(refs)
        };
        let lead_stage = lead.get("stage");
        let ask = if get_str(&lead, "stage") == Some("needs-human") {
            or_null(lead.get("needs"))
        } else {
            Value::Null
        };
        let change = if get_str(&lead, "stage") == Some("closed") {
            let mut change = json!({"stage": IDEA, "refs": refs_or_null, "note": format!("unbatched: {lead_id} closed"), "needs": null});
            if stacked {
                change["stacked"] = Value::Null;
                change["pr"] = Value::Null;
                if truthy(rider.get("pr")) {
                    let note = format!(
                        "{}; stacked PR {} is still open",
                        change["note"].as_str().unwrap(),
                        py_str(&rider["pr"])
                    );
                    change["note"] = Value::from(note);
                }
            }
            change
        } else if !stacked {
            let change =
                json!({"stage": or_null(lead_stage), "pr": or_null(lead.get("pr")), "needs": ask});
            if unchanged(&rider, &change) {
                continue;
            }
            change
        } else if get_str(&lead, "stage") != Some("merged") {
            let change = json!({"stage": or_null(lead_stage), "needs": ask});
            if unchanged(&rider, &change) {
                continue;
            }
            change
        } else if truthy(rider.get("pr")) {
            let refs: Vec<Value> = all_refs
                .iter()
                .filter(|r| r.is_object())
                .map(|r| {
                    if r.get("kind").and_then(Value::as_str) == Some(BATCH_REF) {
                        json!({"id": lead_id, "kind": "follow-up-of"})
                    } else {
                        r.clone()
                    }
                })
                .collect();
            let flag = |key: &str| {
                if lead.get(key) == Some(&Value::Bool(true)) {
                    Value::Bool(true)
                } else {
                    Value::Null
                }
            };
            json!({"refs": refs, "stacked": null, "automerge": flag("automerge"), "escalated": flag("escalated"),
                   "note": format!("stacked on {lead_id}, which merged"), "needs": null})
        } else {
            json!({"stage": IDEA, "refs": refs_or_null, "stacked": null,
                   "note": format!("unbatched: {lead_id} merged before its stacked PR opened"), "needs": null})
        };
        patch_job(data, &rider_id, &change);
    }
}

/// Python's `str(v)` for the values a PR field holds.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// True when patching rider with change would set nothing new.
pub fn unchanged(rider: &Value, change: &Value) -> bool {
    change.as_object().is_some_and(|c| {
        c.iter()
            .all(|(k, v)| rider.get(k).unwrap_or(&Value::Null) == v)
    })
}

/// True for an escalated job, including ones from before the field that carry only the note prefix.
pub fn escalated(j: &Value) -> bool {
    j.get("escalated") == Some(&Value::Bool(true))
        || get_str(j, "note").is_some_and(|n| n.to_lowercase().starts_with("escalated:"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_patch_drops_nulls() {
        let got = merge_patch(
            json!({"a": 1, "b": {"c": 2, "d": 3}}),
            &json!({"a": null, "b": {"c": null, "e": 4}, "f": [1]}),
        );
        assert_eq!(dumps(&got, COMPACT), r#"{"b":{"d":3,"e":4},"f":[1]}"#);
        assert_eq!(merge_patch(json!({"a": 1}), &json!(5)), json!(5));
    }

    #[test]
    fn merged_stage_sets_done() {
        let mut data = json!({"updated": "", "jobs": [{"id": "1-a", "stage": "review"}]});
        let job = patch_job(&mut data, "1-a", &json!({"stage": "merged"}));
        assert_eq!(job["done"], job["updated"]);
        assert_eq!(data["updated"], job["updated"]);
        let job = patch_job(
            &mut data,
            "1-a",
            &json!({"stage": "build", "claim": {"device": "x"}}),
        );
        assert!(job.get("done").is_none());
        assert_eq!(job["started"], job["updated"]);
        let new = patch_job(&mut data, "2-b", &json!({"stage": "idea"}));
        assert_eq!(new["id"], "2-b");
        assert_eq!(
            dumps(&data["jobs"][1], COMPACT).split(',').next(),
            Some(r#"{"id":"2-b""#)
        );
    }

    #[test]
    fn closed_lead_unbatches_its_rider() {
        let mut data = json!({"updated": "", "jobs": [
            {"id": "1-lead", "stage": "build"},
            {"id": "2-rider", "stage": "build", "refs": [{"id": "1-lead", "kind": "batched-into"}, {"id": "0-x", "kind": "rework-of"}], "needs": "ask"},
            {"id": "3-stacked", "stage": "build", "stacked": true, "pr": "https://x/pull/3", "refs": [{"id": "1-lead", "kind": "batched-into"}]},
        ]});
        assert_eq!(riders_of(&data, "1-lead"), ["2-rider", "3-stacked"]);
        patch_job(&mut data, "1-lead", &json!({"stage": "closed"}));
        let rider = &data["jobs"][1];
        assert_eq!(rider["stage"], "idea");
        assert_eq!(rider["refs"], json!([{"id": "0-x", "kind": "rework-of"}]));
        assert_eq!(rider["note"], "unbatched: 1-lead closed");
        assert!(rider.get("needs").is_none());
        let stacked = &data["jobs"][2];
        assert_eq!(
            stacked["note"],
            "unbatched: 1-lead closed; stacked PR https://x/pull/3 is still open"
        );
        assert!(
            stacked.get("refs").is_none()
                && stacked.get("pr").is_none()
                && stacked.get("stacked").is_none()
        );
    }

    #[test]
    fn rider_follows_lead_stage_and_pr() {
        let mut data = json!({"updated": "", "jobs": [
            {"id": "1-lead", "stage": "build"},
            {"id": "2-rider", "stage": "build", "refs": [{"id": "1-lead", "kind": "batched-into"}]},
        ]});
        patch_job(
            &mut data,
            "1-lead",
            &json!({"stage": "needs-human", "needs": "pick one", "pr": "p"}),
        );
        assert_eq!(data["jobs"][1]["stage"], "needs-human");
        assert_eq!(data["jobs"][1]["needs"], "pick one");
        assert_eq!(data["jobs"][1]["pr"], "p");
        assert!(
            escalated(&json!({"note": "Escalated: x"})) && !escalated(&json!({"escalated": 1}))
        );
        assert_eq!(
            batch_refs(&data["jobs"][1]["refs"])
                .into_iter()
                .collect::<Vec<_>>(),
            ["1-lead"]
        );
    }
}
