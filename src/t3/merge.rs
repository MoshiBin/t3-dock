//! Runs one stream per environment and merges them into the single view the keys show.
//!
//! Local threads keep their own ids. A remote thread's id, parent and project are prefixed with
//! its environment id and a NUL, so ids from different servers never meet, the parent links that
//! fold subagents still resolve, and a press knows which environment to open.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::interval;

use super::catalog::{self, CatalogError, Remote};
use super::wire::{Projects, Threads};
use super::{Health, Phase, Source, View, run};
use crate::log::log;

const SEPARATOR: char = '\0';
const CATALOG_POLL: Duration = Duration::from_secs(30);

/// The key of a remote thread or project.
pub fn remote_key(environment_id: &str, id: &str) -> String {
    format!("{environment_id}{SEPARATOR}{id}")
}

/// A remote key's environment and own id. `None` for a local key.
pub fn split_key(key: &str) -> Option<(&str, &str)> {
    key.split_once(SEPARATOR)
}

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Key {
    Local,
    Remote(String),
}

/// The newest view of every source, remote ones already prefixed.
#[derive(Default)]
struct Merger {
    local: View,
    remotes: BTreeMap<String, View>,
}

impl Merger {
    fn set_remote(&mut self, remote: &Remote, view: &View) {
        let environment = remote.environment_id.as_str();
        let threads: Threads = view
            .threads
            .values()
            .map(|thread| {
                let mut thread = (**thread).clone();
                thread.id = remote_key(environment, &thread.id);
                thread.project_id = thread
                    .project_id
                    .map(|project| remote_key(environment, &project));
                if let Some(lineage) = &mut thread.lineage {
                    lineage.parent_thread_id = lineage
                        .parent_thread_id
                        .take()
                        .map(|parent| remote_key(environment, &parent));
                }
                (thread.id.clone(), Arc::new(thread))
            })
            .collect();
        let projects: Projects = view
            .projects
            .iter()
            .map(|(id, name)| {
                let name = if remote.label.is_empty() {
                    name.clone()
                } else {
                    format!("{name} ({})", remote.label)
                };
                (remote_key(environment, id), name)
            })
            .collect();
        self.remotes.insert(
            remote.environment_id.clone(),
            View {
                phase: view.phase,
                environment_id: remote.environment_id.clone(),
                threads: Arc::new(threads),
                projects: Arc::new(projects),
                health: Health::default(),
            },
        );
    }

    /// Live sources only: a source that dropped out takes its threads with it.
    fn view(&self) -> View {
        let remotes: Vec<&View> = self
            .remotes
            .values()
            // The local server is never also counted as a remote.
            .filter(|remote| remote.environment_id != self.local.environment_id)
            .collect();
        let live = std::iter::once(&self.local)
            .chain(remotes.iter().copied())
            .filter(|view| view.phase == Phase::Live);
        let (mut threads, mut projects) = (Threads::new(), Projects::new());
        let mut any_live = false;
        for view in live {
            any_live = true;
            threads.extend(view.threads.iter().map(|(id, t)| (id.clone(), t.clone())));
            projects.extend(view.projects.iter().map(|(id, n)| (id.clone(), n.clone())));
        }
        self.link_delegations(&mut threads);
        View {
            phase: if any_live {
                Phase::Live
            } else {
                self.local.phase
            },
            environment_id: self.local.environment_id.clone(),
            threads: Arc::new(threads),
            projects: Arc::new(projects),
            health: Health {
                local: self.local.phase,
                remotes: remotes.iter().map(|view| view.phase).collect(),
            },
        }
    }
}

impl Merger {
    /// Points each delegated thread at its delegating thread's merged key, whichever environment
    /// either runs in.
    fn link_delegations(&self, threads: &mut Threads) {
        for thread in threads.values_mut() {
            let Some(from) = &thread.delegated_from else {
                continue;
            };
            let (Some(environment), Some(id)) = (&from.environment_id, &from.thread_id) else {
                continue;
            };
            let key = if *environment == self.local.environment_id {
                id.clone()
            } else {
                remote_key(environment, id)
            };
            Arc::make_mut(thread).delegated_by = Some(key);
        }
    }
}

struct Running {
    remote: Remote,
    tasks: [JoinHandle<()>; 2],
}

impl Drop for Running {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Starts a source and forwards each of its views, tagged, to the merge.
fn spawn(
    t3_home: &std::path::Path,
    data: &std::path::Path,
    source: Source,
    key: Key,
    updates: &mpsc::Sender<(Key, View)>,
) -> [JoinHandle<()>; 2] {
    let (views_tx, mut views) = watch::channel(View::default());
    let client = tokio::spawn(run(t3_home.to_owned(), data.to_owned(), source, views_tx));
    let updates = updates.clone();
    let forward = tokio::spawn(async move {
        while views.changed().await.is_ok() {
            let view = views.borrow_and_update().clone();
            if updates.send((key.clone(), view)).await.is_err() {
                return;
            }
        }
    });
    [client, forward]
}

fn modified(t3_home: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(catalog::catalog_path(t3_home))
        .and_then(|meta| meta.modified())
        .ok()
}

/// The local stream, plus one per remote environment in the desktop client's catalog. The catalog
/// is read again when it changes, so environments added or removed in T3 follow.
pub async fn run_all(t3_home: PathBuf, data: PathBuf, views: watch::Sender<View>) {
    let (updates_tx, mut updates) = mpsc::channel(64);
    let _local = spawn(&t3_home, &data, Source::Local, Key::Local, &updates_tx);
    let mut running: HashMap<String, Running> = HashMap::new();
    let mut merger = Merger::default();
    let mut poll = interval(CATALOG_POLL);
    let mut seen: Option<Option<SystemTime>> = None;
    let mut last_error = String::new();

    loop {
        tokio::select! {
            _ = poll.tick() => {
                let stamp = modified(&t3_home);
                if seen == Some(stamp) {
                    continue;
                }
                let home = t3_home.clone();
                let loaded = tokio::task::spawn_blocking(move || catalog::load(&home))
                    .await
                    .unwrap_or(Err(CatalogError::Unreadable));
                let remotes = match loaded {
                    Ok(found) => {
                        seen = Some(stamp);
                        last_error.clear();
                        log!(
                            "catalog: {} remote environments over SSH, {} of other kinds skipped",
                            found.remotes.len(),
                            found.unsupported
                        );
                        found.remotes
                    }
                    Err(CatalogError::Missing) => {
                        seen = Some(stamp);
                        Vec::new()
                    }
                    Err(error) => {
                        // Kept as it was, and tried again on the next poll.
                        let text = error.to_string();
                        if text != last_error {
                            log!("catalog: {text}");
                            last_error = text;
                        }
                        continue;
                    }
                };
                running.retain(|id, run| remotes.iter().any(|remote| {
                    remote.environment_id == *id && *remote == run.remote
                }));
                merger.remotes.retain(|id, _| running.contains_key(id));
                for (index, remote) in remotes.into_iter().enumerate() {
                    if running.contains_key(&remote.environment_id) {
                        continue;
                    }
                    let source = Source::Remote {
                        name: format!("t3 remote {}", index + 1),
                        remote: remote.clone(),
                    };
                    let key = Key::Remote(remote.environment_id.clone());
                    let tasks = spawn(&t3_home, &data, source, key, &updates_tx);
                    running.insert(remote.environment_id.clone(), Running { remote, tasks });
                }
                views.send_replace(merger.view());
            }
            update = updates.recv() => {
                let Some((key, view)) = update else { return };
                match key {
                    Key::Local => merger.local = view,
                    Key::Remote(id) => {
                        // Late updates from a stopped source are dropped.
                        let Some(run) = running.get(&id) else { continue };
                        let remote = run.remote.clone();
                        merger.set_remote(&remote, &view);
                    }
                }
                views.send_replace(merger.view());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::t3::catalog::SshTarget;
    use crate::t3::wire::Thread;

    fn view(phase: Phase, environment: &str, threads: &[Value]) -> View {
        let threads = threads
            .iter()
            .map(|value| {
                let thread: Thread = serde_json::from_value(value.clone()).ok().unwrap();
                (thread.id.clone(), Arc::new(thread))
            })
            .collect();
        View {
            phase,
            environment_id: environment.into(),
            threads: Arc::new(threads),
            projects: Arc::new(HashMap::from([("p".to_owned(), "Proj".to_owned())])),
            health: Health::default(),
        }
    }

    fn remote(environment: &str) -> Remote {
        Remote {
            environment_id: environment.into(),
            label: "Box".into(),
            ssh: SshTarget {
                alias: "box".into(),
                hostname: "box.invalid".into(),
                username: None,
                port: None,
            },
        }
    }

    #[test]
    fn remote_threads_are_prefixed_and_keep_their_parents() {
        let mut merger = Merger {
            local: view(Phase::Live, "env-local", &[json!({"id": "same"})]),
            ..Merger::default()
        };
        merger.set_remote(
            &remote("env-r"),
            &view(
                Phase::Live,
                "env-r",
                &[
                    json!({"id": "same", "projectId": "p"}),
                    json!({"id": "child", "lineage": {"parentThreadId": "same",
                                                      "relationshipToParent": "subagent"}}),
                ],
            ),
        );
        let merged = merger.view();
        assert_eq!(merged.threads.len(), 3);
        assert!(merged.threads.contains_key("same"));
        let parent = remote_key("env-r", "same");
        let child = &merged.threads[&remote_key("env-r", "child")];
        assert_eq!(
            child.lineage.as_ref().unwrap().parent_thread_id.as_deref(),
            Some(parent.as_str())
        );
        assert_eq!(split_key(&parent), Some(("env-r", "same")));
        assert_eq!(split_key("same"), None);
        assert_eq!(
            merged.projects[&remote_key("env-r", "p")],
            "Proj (Box)".to_owned()
        );
        assert_eq!(merged.environment_id, "env-local");
    }

    #[test]
    fn delegated_threads_point_at_their_delegator_across_environments() {
        let mut merger = Merger {
            local: view(
                Phase::Live,
                "env-local",
                &[
                    json!({"id": "boss"}),
                    json!({"id": "here", "delegatedFrom": {"environmentId": "env-r",
                                                           "threadId": "far-boss"}}),
                ],
            ),
            ..Merger::default()
        };
        merger.set_remote(
            &remote("env-r"),
            &view(
                Phase::Live,
                "env-r",
                &[
                    json!({"id": "far-boss"}),
                    json!({"id": "far", "delegatedFrom": {"environmentId": "env-local",
                                                          "threadId": "boss"}}),
                ],
            ),
        );
        let merged = merger.view();
        let far_boss = remote_key("env-r", "far-boss");
        assert_eq!(
            merged.threads["here"].delegated_by.as_deref(),
            Some(far_boss.as_str())
        );
        let far = &merged.threads[&remote_key("env-r", "far")];
        assert_eq!(far.delegated_by.as_deref(), Some("boss"));
        assert_eq!(merged.threads["boss"].delegated_by, None);
    }

    #[test]
    fn any_live_source_makes_the_view_live() {
        let mut merger = Merger {
            local: view(Phase::Offline, "env-local", &[json!({"id": "gone"})]),
            ..Merger::default()
        };
        merger.set_remote(
            &remote("env-r"),
            &view(Phase::Live, "env-r", &[json!({"id": "a"})]),
        );
        merger.set_remote(&remote("env-s"), &view(Phase::NeedsToken, "env-s", &[]));
        let merged = merger.view();
        assert_eq!(merged.phase, Phase::Live);
        assert_eq!(merged.threads.len(), 1);
        assert_eq!(merged.health.local, Phase::Offline);
        assert_eq!(merged.health.remotes, [Phase::Live, Phase::NeedsToken]);

        merger.remotes.clear();
        assert_eq!(merger.view().phase, Phase::Offline);
    }

    #[test]
    fn the_local_server_is_not_counted_twice() {
        let mut merger = Merger {
            local: view(Phase::Live, "env-local", &[json!({"id": "a"})]),
            ..Merger::default()
        };
        merger.set_remote(
            &remote("env-local"),
            &view(Phase::Live, "env-local", &[json!({"id": "a"})]),
        );
        assert_eq!(merger.view().threads.len(), 1);
    }
}
