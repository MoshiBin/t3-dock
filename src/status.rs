//! What a thread's key should say.
//!
//! The rules are ported from `src/status.ts` in beastyrabbit/opendeck-t3-code-status dc27f17
//! (MIT, see THIRD_PARTY.md), which mirrors T3 Code's own sidebar.

use std::collections::{HashMap, HashSet};

use jiff::Timestamp;

use crate::t3::wire::{Thread, Threads};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    Approval,
    Input,
    Plan,
    Limited,
    Failed,
    Starting,
    Working,
    /// Live, but waiting on another thread or on background work.
    Waiting,
    Monitoring,
    /// Finished since the user last opened the thread.
    Done,
    Idle,
}

impl Status {
    /// Lower is more urgent.
    pub fn urgency(self) -> u8 {
        match self {
            Self::Approval | Self::Input | Self::Plan => 0,
            Self::Failed | Self::Limited => 1,
            Self::Starting | Self::Working | Self::Waiting => 2,
            Self::Done => 3,
            Self::Monitoring | Self::Idle => 4,
        }
    }
}

fn is_active(status: &str) -> bool {
    matches!(
        status,
        "preparing" | "queued" | "starting" | "running" | "waiting"
    )
}

fn is_generating(status: &str) -> bool {
    matches!(status, "preparing" | "starting" | "running")
}

fn is_finished(status: &str) -> bool {
    matches!(
        status,
        "idle" | "completed" | "interrupted" | "failed" | "cancelled" | "rolled_back"
    )
}

fn millis(value: Option<&str>) -> Option<i64> {
    value?
        .parse::<Timestamp>()
        .ok()
        .map(|time| time.as_millisecond())
}

fn own_status(thread: &Thread) -> &str {
    thread.status.as_deref().unwrap_or("idle")
}

fn activity_status(thread: &Thread) -> &str {
    match &thread.activity_run_status {
        Some(Some(status)) => status,
        _ => own_status(thread),
    }
}

fn never_ran(thread: &Thread) -> bool {
    thread.latest_run_id.is_none() && thread.active_provider_thread_id.is_none()
}

// Commands and monitors are often a dev server left running, so they hold only when awaited.
fn background_holds_completion(thread: &Thread) -> bool {
    thread
        .pending_background_tasks
        .iter()
        .flatten()
        .any(|task| {
            task.awaited == Some(true)
                || matches!(task.kind.as_deref(), Some("subagent" | "background_task"))
        })
}

/// The run status T3 presents for a thread.
fn runtime_status(thread: &Thread) -> Option<&str> {
    // Awaited background work parks the thread at idle instead of ending it.
    if background_holds_completion(thread) && own_status(thread) != "failed" {
        return Some("idle");
    }
    (!never_ran(thread)).then(|| activity_status(thread))
}

fn has_active_run(thread: &Thread) -> bool {
    !never_ran(thread) && is_active(activity_status(thread))
}

fn has_unseen_completion(thread: &Thread) -> bool {
    let completed = match &thread.latest_run_completed_at {
        Some(value) => value.as_deref(),
        None if thread.latest_run_id.is_some() && is_finished(own_status(thread)) => {
            thread.updated_at.as_deref()
        }
        None => None,
    };
    match (millis(completed), millis(thread.last_visited_at.as_deref())) {
        (Some(completed), Some(visited)) => completed > visited,
        _ => false,
    }
}

fn has_pending_approval(thread: &Thread) -> bool {
    thread
        .pending_runtime_request
        .as_ref()
        .and_then(|request| request.kind.as_deref())
        .is_some_and(|kind| kind != "user_input" && kind != "auth_refresh")
}

fn has_pending_input(thread: &Thread) -> bool {
    let Some(request) = &thread.pending_runtime_request else {
        return false;
    };
    // A non-blocking question only needs the user once the run stops working past it.
    request.kind.as_deref() == Some("user_input")
        && (request.blocking != Some(false) || !runtime_status(thread).is_some_and(is_generating))
}

fn has_guest_message(thread: &Thread) -> bool {
    thread
        .pending_guest_message_count
        .is_some_and(|count| count > 0)
}

fn is_subagent(thread: &Thread) -> bool {
    thread
        .lineage
        .as_ref()
        .and_then(|lineage| lineage.relationship_to_parent.as_deref())
        == Some("subagent")
}

fn is_side_chat(thread: &Thread) -> bool {
    thread
        .lineage
        .as_ref()
        .and_then(|lineage| lineage.side_chat)
        == Some(true)
}

/// Subagents and side chats live inside their parent in T3, not in its sidebar.
pub fn is_child(thread: &Thread) -> bool {
    is_subagent(thread) || is_side_chat(thread)
}

fn parent_id(thread: &Thread) -> Option<&str> {
    thread.lineage.as_ref()?.parent_thread_id.as_deref()
}

/// What children ask of their parents: `Approval` or `Input`, by parent id.
///
/// A live subagent's approval reaches every parent up to the top thread. A coordinator answers
/// its subagent's questions and provider-native subagents ask on their parent, so neither is
/// flagged. A side chat's approval or question reaches its direct parent.
pub fn child_requests(threads: &Threads) -> HashMap<&str, Status> {
    let mut flagged = HashMap::new();
    let mut flag = |id, status| {
        let current = flagged.entry(id).or_insert(status);
        if status == Status::Approval {
            *current = status;
        }
    };
    for child in threads.values() {
        if is_side_chat(child) {
            let request = child
                .pending_runtime_request
                .as_ref()
                .and_then(|request| request.kind.as_deref());
            let status = match request {
                None | Some("auth_refresh") => continue,
                Some("user_input") => Status::Input,
                Some(_) => Status::Approval,
            };
            if let Some(parent) = parent_id(child) {
                flag(parent, status);
            }
            continue;
        }
        if !is_subagent(child)
            || child.creation_source.as_deref() == Some("provider")
            || !has_pending_approval(child)
        {
            continue;
        }
        let live = match &child.activity_run_status {
            Some(status) => status.is_some(),
            None => matches!(
                own_status(child),
                "preparing" | "starting" | "running" | "waiting"
            ),
        };
        if !live {
            continue;
        }
        let mut seen = HashSet::from([child.id.as_str()]);
        let mut next = parent_id(child);
        while let Some(id) = next {
            if !seen.insert(id) {
                break;
            }
            flag(id, Status::Approval);
            next = threads
                .get(id)
                .filter(|parent| is_child(parent))
                .and_then(|parent| parent_id(parent));
        }
    }
    flagged
}

/// `child` is what the thread's children ask of it, from `child_requests`.
pub fn classify(thread: &Thread, child: Option<Status>) -> Status {
    if child == Some(Status::Approval) || has_pending_approval(thread) {
        return Status::Approval;
    }
    if child == Some(Status::Input) || has_pending_input(thread) || has_guest_message(thread) {
        return Status::Input;
    }
    let status = runtime_status(thread);
    let run_active = has_active_run(thread);
    if thread.interaction_mode.as_deref() == Some("plan")
        && thread.has_actionable_proposed_plan == Some(true)
        && !run_active
    {
        return Status::Plan;
    }
    let failed = status == Some("failed");
    let usage_limit = failed && thread.last_error_class.as_deref() == Some("usage_limit");
    if (thread.limit_recovery.is_some() || usage_limit) && !run_active {
        return Status::Limited;
    }
    if failed {
        return Status::Failed;
    }
    let waits_on_thread = thread.blocked_on_thread_wait == Some(true);
    if matches!(status, Some("preparing" | "queued" | "starting")) {
        return if waits_on_thread {
            Status::Waiting
        } else {
            Status::Starting
        };
    }
    let parked = background_holds_completion(thread);
    if status.is_some_and(is_active) || parked {
        return if waits_on_thread || status == Some("idle") {
            Status::Waiting
        } else {
            Status::Working
        };
    }
    if has_unseen_completion(thread) {
        return Status::Done;
    }
    if thread
        .pending_background_tasks
        .as_ref()
        .is_some_and(|tasks| !tasks.is_empty())
    {
        return Status::Monitoring;
    }
    Status::Idle
}

// Only a request for the user or a fresh failure ends a snooze early.
fn is_snoozed(thread: &Thread, now: Timestamp, child_request: bool) -> bool {
    let Some(wake_at) = millis(thread.snoozed_until.as_deref()) else {
        return false;
    };
    if wake_at <= now.as_millisecond()
        || thread.pending_runtime_request.is_some()
        || child_request
        || has_guest_message(thread)
    {
        return false;
    }
    if runtime_status(thread) != Some("failed") {
        return true;
    }
    let completed = thread
        .latest_run_completed_at
        .as_ref()
        .and_then(|value| value.as_deref())
        .filter(|_| own_status(thread) == "failed");
    let failed_at = millis(completed.or(thread.updated_at.as_deref()));
    matches!(
        (failed_at, millis(thread.snoozed_at.as_deref())),
        (Some(failed), Some(snoozed)) if failed <= snoozed
    )
}

/// Whether a thread deserves a key.
pub fn is_relevant(thread: &Thread, now: Timestamp, child: Option<Status>) -> bool {
    if thread.archived_at.is_some() || thread.deleted_at.is_some() || is_child(thread) {
        return false;
    }
    let child_request = child.is_some();
    if is_snoozed(thread, now, child_request) {
        return false;
    }
    // T3 settles threads itself, but live work and requests stay visible.
    thread.settled_override.as_deref() != Some("settled")
        || child_request
        || thread.pending_runtime_request.is_some()
        || has_guest_message(thread)
        || has_active_run(thread)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{Value, json};

    use super::*;

    const BEFORE: &str = "2030-01-01T10:00:00.000Z";
    const AFTER: &str = "2030-01-01T11:00:00.000Z";

    fn thread(fields: Value) -> Thread {
        let mut value = json!({"id": "t", "status": "completed", "latestRunId": "run:1"});
        for (key, field) in fields.as_object().unwrap() {
            value[key] = field.clone();
        }
        serde_json::from_value(value).ok().unwrap()
    }

    fn status(fields: Value) -> Status {
        classify(&thread(fields), None)
    }

    fn relevant(fields: Value) -> bool {
        is_relevant(
            &thread(fields),
            "2030-01-01T12:00:00Z".parse().unwrap(),
            None,
        )
    }

    fn request(kind: &str) -> Value {
        json!({"kind": kind})
    }

    #[test]
    fn run_states() {
        for (run, expected) in [
            ("preparing", Status::Starting),
            ("queued", Status::Starting),
            ("starting", Status::Starting),
            ("running", Status::Working),
            ("waiting", Status::Working),
            ("failed", Status::Failed),
            ("completed", Status::Idle),
            ("interrupted", Status::Idle),
            ("from-the-future", Status::Idle),
        ] {
            assert_eq!(status(json!({"status": run})), expected, "{run}");
        }
    }

    #[test]
    fn activity_status_wins_over_status() {
        let fields = json!({"status": "completed", "activityRunStatus": "running"});
        assert_eq!(status(fields), Status::Working);
        let fields = json!({"status": "running", "activityRunStatus": null});
        assert_eq!(status(fields), Status::Working);
    }

    #[test]
    fn a_thread_that_never_ran_is_idle() {
        let fields = json!({"status": "running", "latestRunId": null});
        assert_eq!(status(fields), Status::Idle);
    }

    #[test]
    fn requests_need_the_user() {
        let approval = json!({"status": "running", "pendingRuntimeRequest": request("command")});
        assert_eq!(status(approval), Status::Approval);
        let input = json!({"status": "running", "pendingRuntimeRequest": request("user_input")});
        assert_eq!(status(input), Status::Input);
        let refresh =
            json!({"status": "running", "pendingRuntimeRequest": request("auth_refresh")});
        assert_eq!(status(refresh), Status::Working);
        assert_eq!(
            status(json!({"pendingGuestMessageCount": 1})),
            Status::Input
        );
    }

    #[test]
    fn non_blocking_question_waits_for_the_run_to_stop() {
        let question = json!({"kind": "user_input", "blocking": false});
        let running = json!({"status": "running", "pendingRuntimeRequest": question});
        assert_eq!(status(running), Status::Working);
        let finished = json!({"pendingRuntimeRequest": question});
        assert_eq!(status(finished), Status::Input);
    }

    #[test]
    fn plan_only_counts_in_plan_mode_after_the_run() {
        let plan = json!({"interactionMode": "plan", "hasActionableProposedPlan": true});
        assert_eq!(status(plan), Status::Plan);
        let running = json!({"status": "running", "interactionMode": "plan", "hasActionableProposedPlan": true});
        assert_eq!(status(running), Status::Working);
        assert_eq!(
            status(json!({"hasActionableProposedPlan": true})),
            Status::Idle
        );
    }

    #[test]
    fn usage_limits() {
        let failed = json!({"status": "failed", "lastErrorClass": "usage_limit"});
        assert_eq!(status(failed), Status::Limited);
        assert_eq!(status(json!({"limitRecovery": {}})), Status::Limited);
        let running = json!({"status": "running", "limitRecovery": {}});
        assert_eq!(status(running), Status::Working);
    }

    #[test]
    fn parked_and_blocked_runs_wait() {
        let parked = json!({"pendingBackgroundTasks": [{"kind": "subagent"}]});
        assert_eq!(status(parked), Status::Waiting);
        let awaited = json!({"pendingBackgroundTasks": [{"kind": "command", "awaited": true}]});
        assert_eq!(status(awaited), Status::Waiting);
        let blocked = json!({"status": "running", "blockedOnThreadWait": true});
        assert_eq!(status(blocked), Status::Waiting);
        let failed = json!({"status": "failed", "pendingBackgroundTasks": [{"kind": "subagent"}]});
        assert_eq!(status(failed), Status::Failed);
        let server = json!({"pendingBackgroundTasks": [{"kind": "command"}]});
        assert_eq!(status(server), Status::Monitoring);
    }

    #[test]
    fn done_means_unread() {
        let unread = json!({"latestRunCompletedAt": AFTER, "lastVisitedAt": BEFORE});
        assert_eq!(status(unread), Status::Done);
        let read = json!({"latestRunCompletedAt": BEFORE, "lastVisitedAt": AFTER});
        assert_eq!(status(read), Status::Idle);
        let dated_by_update = json!({"updatedAt": AFTER, "lastVisitedAt": BEFORE});
        assert_eq!(status(dated_by_update), Status::Done);
        let null =
            json!({"latestRunCompletedAt": null, "updatedAt": AFTER, "lastVisitedAt": BEFORE});
        assert_eq!(status(null), Status::Idle);
    }

    #[test]
    fn visibility() {
        assert!(relevant(json!({})));
        assert!(!relevant(json!({"archivedAt": BEFORE})));
        assert!(!relevant(json!({"deletedAt": BEFORE})));
        assert!(!relevant(
            json!({"lineage": {"relationshipToParent": "subagent"}})
        ));
        assert!(relevant(
            json!({"lineage": {"relationshipToParent": "fork"}})
        ));
        assert!(!relevant(
            json!({"lineage": {"relationshipToParent": "fork", "sideChat": true}})
        ));
        assert!(!relevant(json!({"settledOverride": "settled"})));
        assert!(relevant(
            json!({"settledOverride": "settled", "status": "running"})
        ));
    }

    #[test]
    fn snoozing() {
        let later = "2030-01-01T13:00:00Z";
        assert!(!relevant(json!({"snoozedUntil": later})));
        assert!(relevant(json!({"snoozedUntil": AFTER})));
        assert!(relevant(
            json!({"snoozedUntil": later, "pendingRuntimeRequest": request("command")})
        ));
        assert!(relevant(json!({"snoozedUntil": later, "snoozedAt": BEFORE,
                                "status": "failed", "updatedAt": AFTER})));
        assert!(!relevant(json!({"snoozedUntil": later, "snoozedAt": AFTER,
                                 "status": "failed", "updatedAt": BEFORE})));
    }

    fn child(id: &str, parent: &str, relationship: Value, extra: Value) -> Thread {
        let mut fields = json!({"id": id, "lineage": {"parentThreadId": parent}});
        for (key, field) in relationship.as_object().unwrap() {
            fields["lineage"][key] = field.clone();
        }
        for (key, field) in extra.as_object().unwrap() {
            fields[key] = field.clone();
        }
        thread(fields)
    }

    fn index(threads: Vec<Thread>) -> Threads {
        threads
            .into_iter()
            .map(|thread| (thread.id.clone(), Arc::new(thread)))
            .collect()
    }

    #[test]
    fn subagent_approvals_bubble_up_to_the_top_thread() {
        let subagent = json!({"relationshipToParent": "subagent"});
        let approval = request("command");
        let threads = index(vec![
            thread(json!({"id": "top"})),
            child("middle", "top", subagent.clone(), json!({})),
            child(
                "leaf",
                "middle",
                subagent.clone(),
                json!({"status": "running", "pendingRuntimeRequest": approval}),
            ),
            thread(json!({"id": "other"})),
            child(
                "native",
                "other",
                subagent.clone(),
                json!({"status": "running", "creationSource": "provider",
                       "pendingRuntimeRequest": approval}),
            ),
            child(
                "stopped",
                "other",
                subagent.clone(),
                json!({"status": "running", "activityRunStatus": null,
                       "pendingRuntimeRequest": approval}),
            ),
            child(
                "asks",
                "other",
                subagent,
                json!({"status": "running", "pendingRuntimeRequest": request("user_input")}),
            ),
        ]);

        let requests = child_requests(&threads);
        assert_eq!(
            requests,
            HashMap::from([("middle", Status::Approval), ("top", Status::Approval)])
        );
        assert_eq!(
            classify(&threads["top"], requests.get("top").copied()),
            Status::Approval
        );
    }

    #[test]
    fn side_chats_ask_through_their_parent() {
        let side_chat = json!({"relationshipToParent": "fork", "sideChat": true});
        let threads = index(vec![
            thread(json!({"id": "asked", "settledOverride": "settled"})),
            child(
                "question",
                "asked",
                side_chat.clone(),
                json!({"pendingRuntimeRequest": request("user_input")}),
            ),
            thread(json!({"id": "approve"})),
            child(
                "command",
                "approve",
                side_chat.clone(),
                json!({"pendingRuntimeRequest": request("command")}),
            ),
            child(
                "other-question",
                "approve",
                side_chat.clone(),
                json!({"pendingRuntimeRequest": request("user_input")}),
            ),
            thread(json!({"id": "quiet"})),
            child("idle", "quiet", side_chat.clone(), json!({})),
            child(
                "refresh",
                "quiet",
                side_chat,
                json!({"pendingRuntimeRequest": request("auth_refresh")}),
            ),
        ]);

        let requests = child_requests(&threads);
        assert_eq!(
            requests,
            HashMap::from([("asked", Status::Input), ("approve", Status::Approval)])
        );
        let now = "2030-01-01T12:00:00Z".parse().unwrap();
        let asked = &threads["asked"];
        assert!(is_relevant(asked, now, requests.get("asked").copied()));
        assert_eq!(classify(asked, Some(Status::Input)), Status::Input);
        assert!(!is_relevant(&threads["question"], now, None));
    }
}
