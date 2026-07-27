//! Admission and shutdown ownership for finite IPC command work.
//!
//! Tauri's `spawn_blocking` pool owns the OS threads, but it does not give the
//! application a process-lifecycle join point. A command queued immediately
//! before quit may otherwise begin mutating SQLite or the filesystem after
//! final sidecar/session/WAL finalization starts. Every command that can touch
//! application state takes a permit from this registry inside its blocking
//! closure. Shutdown closes admission first and waits for existing permits.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandClass {
    Read,
    Mutation,
}

#[derive(Debug, Clone)]
pub struct CommandWorkSnapshot {
    pub id: u64,
    pub name: &'static str,
    pub class: CommandClass,
    pub started_at: SystemTime,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("application is stopping; command work is no longer accepted")]
pub struct CommandAdmissionClosed;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandShutdownReport {
    pub acknowledged: bool,
    pub remaining: Vec<&'static str>,
}

struct RegistryState {
    accepting: bool,
    next_id: u64,
    active: BTreeMap<u64, CommandWorkSnapshot>,
}

pub struct CommandWorkRegistry {
    state: Mutex<RegistryState>,
    changed: Condvar,
}

impl Default for CommandWorkRegistry {
    fn default() -> Self {
        Self {
            state: Mutex::new(RegistryState {
                accepting: true,
                next_id: 1,
                active: BTreeMap::new(),
            }),
            changed: Condvar::new(),
        }
    }
}

pub struct CommandWorkPermit {
    registry: Arc<CommandWorkRegistry>,
    id: u64,
}

impl CommandWorkRegistry {
    pub fn admit(
        self: &Arc<Self>,
        name: &'static str,
        class: CommandClass,
    ) -> Result<CommandWorkPermit, CommandAdmissionClosed> {
        let mut state = self.state.lock().expect("command work mutex");
        if !state.accepting {
            return Err(CommandAdmissionClosed);
        }
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1).max(1);
        state.active.insert(
            id,
            CommandWorkSnapshot {
                id,
                name,
                class,
                started_at: SystemTime::now(),
            },
        );
        Ok(CommandWorkPermit {
            registry: Arc::clone(self),
            id,
        })
    }

    pub fn snapshots(&self) -> Vec<CommandWorkSnapshot> {
        self.state
            .lock()
            .expect("command work mutex")
            .active
            .values()
            .cloned()
            .collect()
    }

    pub fn begin_shutdown(&self) {
        self.state.lock().expect("command work mutex").accepting = false;
        self.changed.notify_all();
    }

    pub fn shutdown(&self, timeout: Duration) -> CommandShutdownReport {
        self.begin_shutdown();
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().expect("command work mutex");
        while !state.active.is_empty() {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (next, _) = self
                .changed
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .expect("command work condvar");
            state = next;
        }
        CommandShutdownReport {
            acknowledged: state.active.is_empty(),
            remaining: state.active.values().map(|work| work.name).collect(),
        }
    }
}

impl Drop for CommandWorkPermit {
    fn drop(&mut self) {
        self.registry
            .state
            .lock()
            .expect("command work mutex")
            .active
            .remove(&self.id);
        self.registry.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn shutdown_rejects_late_work_and_waits_for_an_admitted_command() {
        let registry = Arc::new(CommandWorkRegistry::default());
        let permit = registry
            .admit("library.add-root", CommandClass::Mutation)
            .unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let registry_for_shutdown = Arc::clone(&registry);
        let waiter = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            registry_for_shutdown.shutdown(Duration::from_secs(1))
        });
        started_rx.recv().unwrap();
        while registry.admit("late", CommandClass::Mutation).is_ok() {
            std::thread::yield_now();
        }
        drop(permit);

        assert_eq!(
            waiter.join().unwrap(),
            CommandShutdownReport {
                acknowledged: true,
                remaining: Vec::new(),
            }
        );
        assert_eq!(
            registry.admit("late", CommandClass::Read).err(),
            Some(CommandAdmissionClosed)
        );
    }

    #[test]
    fn timeout_names_unacknowledged_command_work() {
        let registry = Arc::new(CommandWorkRegistry::default());
        let _permit = registry
            .admit("collections.flush", CommandClass::Mutation)
            .unwrap();
        assert_eq!(
            registry.shutdown(Duration::ZERO),
            CommandShutdownReport {
                acknowledged: false,
                remaining: vec!["collections.flush"],
            }
        );
    }
}
