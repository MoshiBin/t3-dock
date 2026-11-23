//! The part of T3 Code's V2 shell stream that t3-dock reads.
//!
//! Types holding titles deliberately do not implement `Debug`, and parse errors never quote the
//! input, so neither can leak thread content into a log.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Deserializer};

pub const PROTOCOL: u64 = 2;
pub const REQUEST_ID: &str = "shell";

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    pub project_id: Option<String>,
    pub title: Option<String>,
    pub creation_source: Option<String>,
    pub lineage: Option<Lineage>,
    /// Set on a thread that another thread delegated, possibly from another environment.
    pub delegated_from: Option<DelegatedFrom>,
    /// The merged key of the thread that delegated this one. Filled in by the merge, not sent.
    #[serde(skip)]
    pub delegated_by: Option<String>,

    pub status: Option<String>,
    /// Absent and null differ: older servers omit it, and null means no live run.
    #[serde(default, deserialize_with = "present")]
    pub activity_run_status: Option<Option<String>>,
    pub latest_run_id: Option<String>,
    pub active_provider_thread_id: Option<String>,
    pub blocked_on_thread_wait: Option<bool>,
    pub pending_background_tasks: Option<Vec<BackgroundTask>>,
    pub last_error_class: Option<String>,
    pub limit_recovery: Option<Present>,

    pub pending_runtime_request: Option<RuntimeRequest>,
    pub pending_guest_message_count: Option<u64>,
    pub has_actionable_proposed_plan: Option<bool>,
    pub interaction_mode: Option<String>,

    /// Absent and null differ: when absent, a finished run is dated by `updatedAt`.
    #[serde(default, deserialize_with = "present")]
    pub latest_run_completed_at: Option<Option<String>>,
    pub last_visited_at: Option<String>,

    pub archived_at: Option<String>,
    pub deleted_at: Option<String>,
    pub settled_override: Option<String>,
    pub snoozed_until: Option<String>,
    pub snoozed_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lineage {
    pub parent_thread_id: Option<String>,
    pub relationship_to_parent: Option<String>,
    /// A side chat is a fork that T3 shows inside its parent, not in the sidebar.
    pub side_chat: Option<bool>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedFrom {
    pub environment_id: Option<String>,
    pub thread_id: Option<String>,
}

#[derive(Clone, Deserialize)]
pub struct BackgroundTask {
    pub kind: Option<String>,
    pub awaited: Option<bool>,
}

#[derive(Clone, Deserialize)]
pub struct RuntimeRequest {
    pub kind: Option<String>,
    pub blocking: Option<bool>,
}

/// A field whose presence matters but whose content does not.
#[derive(Clone)]
pub struct Present;

impl<'de> Deserialize<'de> for Present {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer).map(|_| Present)
    }
}

/// Wraps a present value, including null, in `Some`.
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    #[serde(rename = "_tag")]
    pub tag: String,
    pub request_id: Option<String>,
    pub values: Option<Vec<Item>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    kind: String,
    sequence: Option<u64>,
    location: Option<String>,
    thread: Option<Thread>,
    thread_id: Option<String>,
    project: Option<Project>,
    project_id: Option<String>,
    snapshot: Option<Snapshot>,
    resolved_repository_identity_roots: Option<Present>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    snapshot_sequence: u64,
    threads: Vec<Thread>,
    projects: Option<Vec<Project>>,
}

#[derive(Deserialize)]
struct Project {
    id: String,
    title: Option<String>,
}

/// Where JSON failed to parse, without serde's message, which can quote the offending value.
#[derive(Debug)]
pub struct ParseError {
    category: serde_json::error::Category,
    line: usize,
    column: usize,
}

impl From<serde_json::Error> for ParseError {
    fn from(error: serde_json::Error) -> Self {
        Self {
            category: error.classify(),
            line: error.line(),
            column: error.column(),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unexpected JSON ({:?} at {}:{})",
            self.category, self.line, self.column
        )
    }
}

/// A WebSocket message holds one frame or an array of them.
pub fn parse_frames(text: &str) -> Result<Vec<Frame>, ParseError> {
    if text.trim_start().starts_with('[') {
        Ok(serde_json::from_str(text)?)
    } else {
        Ok(vec![serde_json::from_str(text)?])
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    Snapshot,
    Changed,
    Unchanged,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StreamError {
    UpdateBeforeSnapshot,
    SnapshotRequired,
    MissingField,
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UpdateBeforeSnapshot => "update arrived before the snapshot",
            Self::SnapshotRequired => "server asked for a new snapshot",
            Self::MissingField => "stream item lacks a required field",
        })
    }
}

pub type Threads = HashMap<String, Arc<Thread>>;
/// Project names by id.
pub type Projects = HashMap<String, String>;

/// Active threads and projects as built from one subscription.
#[derive(Default)]
pub struct Shell {
    sequence: Option<u64>,
    pub threads: Threads,
    pub projects: Projects,
}

impl Shell {
    pub fn apply(&mut self, item: Item) -> Result<Applied, StreamError> {
        match item.kind.as_str() {
            "synchronized" => return Ok(Applied::Unchanged),
            "snapshot-required" => return Err(StreamError::SnapshotRequired),
            "snapshot" => {
                // Repository identity refreshes reuse this kind with an empty thread list.
                if item.resolved_repository_identity_roots.is_some() {
                    return Ok(Applied::Unchanged);
                }
                let snapshot = item.snapshot.ok_or(StreamError::MissingField)?;
                self.sequence = Some(snapshot.snapshot_sequence);
                self.threads = snapshot
                    .threads
                    .into_iter()
                    .map(|thread| (thread.id.clone(), Arc::new(thread)))
                    .collect();
                self.projects = snapshot
                    .projects
                    .unwrap_or_default()
                    .into_iter()
                    .map(|project| (project.id, project.title.unwrap_or_default()))
                    .collect();
                return Ok(Applied::Snapshot);
            }
            "thread.updated" | "thread.removed" | "project.updated" | "project.removed" => {}
            // Kinds added by newer servers.
            _ => return Ok(Applied::Unchanged),
        }

        let last = self.sequence.ok_or(StreamError::UpdateBeforeSnapshot)?;
        let sequence = item.sequence.ok_or(StreamError::MissingField)?;
        if sequence <= last {
            return Ok(Applied::Unchanged);
        }

        let archive = item.location.as_deref() == Some("archive");
        let changed = match item.kind.as_str() {
            "thread.updated" => {
                let thread = item.thread.ok_or(StreamError::MissingField)?;
                if archive {
                    self.threads.remove(&thread.id).is_some()
                } else {
                    self.threads.insert(thread.id.clone(), Arc::new(thread));
                    true
                }
            }
            "thread.removed" => {
                let id = item.thread_id.ok_or(StreamError::MissingField)?;
                // Removal from the archive is how an unarchive arrives, so keep the active copy.
                !archive && self.threads.remove(&id).is_some()
            }
            "project.updated" => {
                let project = item.project.ok_or(StreamError::MissingField)?;
                let title = project.title.unwrap_or_default();
                self.projects.insert(project.id, title.clone()) != Some(title)
            }
            _ => {
                let id = item.project_id.ok_or(StreamError::MissingField)?;
                self.projects.remove(&id).is_some()
            }
        };
        self.sequence = Some(sequence);
        Ok(if changed {
            Applied::Changed
        } else {
            Applied::Unchanged
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn apply(shell: &mut Shell, value: Value) -> Result<Applied, StreamError> {
        let item = serde_json::from_value(value).expect("fixture parses");
        shell.apply(item)
    }

    fn synced() -> Shell {
        let mut shell = Shell::default();
        let item = json!({"kind": "snapshot", "snapshot": {
            "snapshotSequence": 10,
            "threads": [{"id": "thread:a", "title": "Invented A"}],
            "archivedThreads": [{"id": "thread:old"}],
            "projects": [{"id": "project:x"}],
        }});
        assert_eq!(apply(&mut shell, item), Ok(Applied::Snapshot));
        shell
    }

    #[test]
    fn frame_is_one_object_or_an_array() {
        let one = parse_frames(r#" {"_tag":"Pong"} "#).ok().unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].tag, "Pong");

        let many = parse_frames(
            r#" [{"_tag":"Ping"},{"_tag":"Chunk","requestId":"shell","values":[{"kind":"synchronized"}]}]"#,
        )
        .ok()
        .unwrap();
        assert_eq!(many.len(), 2);
        assert_eq!(many[1].request_id.as_deref(), Some(REQUEST_ID));
        assert_eq!(many[1].values.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn exit_and_defect_bodies_are_ignored() {
        for text in [
            r#"{"_tag":"Exit","requestId":"shell","exit":{"_tag":"Failure","cause":[{"error":"x"}]}}"#,
            r#"{"_tag":"Defect","defect":{"message":"x"}}"#,
        ] {
            assert!(parse_frames(text).is_ok());
        }
    }

    #[test]
    fn parse_errors_do_not_quote_content() {
        for text in [
            r#"{"_tag":"Chunk","values":[{"kind":"snapshot","snapshot":"PRIVATE TEXT"}]}"#,
            r#"{"_tag":"Chunk","values":"PRIVATE TEXT"}"#,
        ] {
            let error = parse_frames(text).err().unwrap();
            assert!(!error.to_string().contains("PRIVATE"));
            assert!(!format!("{error:?}").contains("PRIVATE"));
        }
    }

    #[test]
    fn thread_parsing_tolerates_nulls_and_unknown_fields() {
        let thread: Thread = serde_json::from_value(json!({
            "id": "thread:a",
            "title": null,
            "status": "a-status-from-the-future",
            "pendingBackgroundTasks": null,
            "latestVisibleMessage": {"text": "never read", "extra": [1, 2]},
            "limitRecovery": {"anything": true},
            "newField": {"nested": 1},
        }))
        .ok()
        .unwrap();
        assert!(thread.title.is_none());
        assert!(thread.pending_background_tasks.is_none());
        assert!(thread.limit_recovery.is_some());
    }

    #[test]
    fn absent_differs_from_null() {
        let absent: Thread = serde_json::from_value(json!({"id": "a"})).ok().unwrap();
        let null: Thread = serde_json::from_value(
            json!({"id": "a", "activityRunStatus": null, "latestRunCompletedAt": null}),
        )
        .ok()
        .unwrap();
        assert!(absent.activity_run_status.is_none() && absent.latest_run_completed_at.is_none());
        assert_eq!(null.activity_run_status, Some(None));
        assert_eq!(null.latest_run_completed_at, Some(None));
    }

    #[test]
    fn snapshot_keeps_only_active_threads() {
        let shell = synced();
        assert_eq!(shell.threads.len(), 1);
        assert!(shell.threads.contains_key("thread:a"));
    }

    #[test]
    fn updates_replace_threads_and_stale_ones_are_dropped() {
        let mut shell = synced();
        let update = |sequence: u64, title: &str| {
            json!({"kind": "thread.updated", "sequence": sequence, "location": "active",
                   "thread": {"id": "thread:a", "title": title}})
        };
        assert_eq!(apply(&mut shell, update(12, "Newer")), Ok(Applied::Changed));
        assert_eq!(
            apply(&mut shell, update(11, "Older")),
            Ok(Applied::Unchanged)
        );
        assert_eq!(shell.threads["thread:a"].title.as_deref(), Some("Newer"));
    }

    #[test]
    fn archiving_and_removal() {
        let mut shell = synced();
        let unarchive = json!({"kind": "thread.removed", "sequence": 11, "location": "archive", "threadId": "thread:a"});
        assert_eq!(apply(&mut shell, unarchive), Ok(Applied::Unchanged));
        assert_eq!(shell.threads.len(), 1);

        let archive = json!({"kind": "thread.updated", "sequence": 12, "location": "archive", "thread": {"id": "thread:a"}});
        assert_eq!(apply(&mut shell, archive), Ok(Applied::Changed));
        assert!(shell.threads.is_empty());

        let mut shell = synced();
        let remove = json!({"kind": "thread.removed", "sequence": 11, "location": "active", "threadId": "thread:a"});
        assert_eq!(apply(&mut shell, remove), Ok(Applied::Changed));
        assert!(shell.threads.is_empty());
    }

    #[test]
    fn projects_follow_the_stream() {
        let mut shell = synced();
        assert!(shell.projects.contains_key("project:x"));
        let rename = json!({"kind": "project.updated", "sequence": 11,
                            "project": {"id": "project:x", "title": "Invented project"}});
        assert_eq!(apply(&mut shell, rename), Ok(Applied::Changed));
        assert_eq!(shell.projects["project:x"], "Invented project");
        let remove = json!({"kind": "project.removed", "sequence": 12, "projectId": "project:x"});
        assert_eq!(apply(&mut shell, remove), Ok(Applied::Changed));
        assert!(shell.projects.is_empty());
    }

    #[test]
    fn identity_refresh_does_not_clear_threads() {
        let mut shell = synced();
        let refresh = json!({"kind": "snapshot", "resolvedRepositoryIdentityRoots": [],
                             "snapshot": {"snapshotSequence": 11, "threads": []}});
        assert_eq!(apply(&mut shell, refresh), Ok(Applied::Unchanged));
        assert_eq!(shell.threads.len(), 1);
    }

    #[test]
    fn unknown_kinds_are_ignored_and_broken_ones_are_not() {
        let mut shell = synced();
        assert_eq!(
            apply(&mut shell, json!({"kind": "something.new", "sequence": 11})),
            Ok(Applied::Unchanged)
        );
        assert_eq!(
            apply(
                &mut shell,
                json!({"kind": "thread.updated", "sequence": 11})
            ),
            Err(StreamError::MissingField)
        );
        assert_eq!(
            apply(&mut shell, json!({"kind": "snapshot-required"})),
            Err(StreamError::SnapshotRequired)
        );
        assert_eq!(
            apply(
                &mut Shell::default(),
                json!({"kind": "thread.removed", "sequence": 1, "threadId": "a"})
            ),
            Err(StreamError::UpdateBeforeSnapshot)
        );
    }
}
