//! The core runtime event bus (spec/RUNTIME.md §8.3): readiness changes
//! are events; the UI subscribes; the debug panel shows raw states.
//! Mirrors the shell's existing emission pattern — low-rate, payload-light
//! (UI §7.4 wire discipline).

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

/// The two managed external processes (RUNTIME §1.2 — nothing else).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessId {
    Llm,
    Asr,
}

impl ProcessId {
    pub fn as_str(self) -> &'static str {
        match self {
            ProcessId::Llm => "llm",
            ProcessId::Asr => "asr",
        }
    }
}

/// Events on the bus. Readiness gates UI features (§8.3); state changes
/// feed the debug panel; download progress feeds settings.
#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeEvent {
    StateChanged {
        process: ProcessId,
        state: String,
        at_ms: u64,
    },
    /// The §8.3 gating signal: a feature appears/enables only when its
    /// backing service is Ready.
    Readiness { process: ProcessId, ready: bool },
    DownloadProgress {
        model_id: String,
        downloaded_bytes: u64,
        total_bytes: u64,
    },
    /// §11: "may I GC model X?" is answered over the bus; the GC
    /// coordinator publishes the question's resolution for the debug
    /// panel.
    GcResolved {
        model_id: String,
        allowed: bool,
        reason: String,
    },
}

/// Multi-subscriber bus over std mpsc; dead receivers are pruned on
/// publish. Cloneable handle shared by supervisors, the download manager,
/// and the GC coordinator.
#[derive(Clone, Default)]
pub struct RuntimeBus {
    subscribers: Arc<Mutex<Vec<Sender<RuntimeEvent>>>>,
}

impl RuntimeBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe(&self) -> Receiver<RuntimeEvent> {
        let (tx, rx) = channel();
        self.subscribers.lock().expect("bus mutex").push(tx);
        rx
    }

    pub fn publish(&self, event: RuntimeEvent) {
        self.subscribers
            .lock()
            .expect("bus mutex")
            .retain(|tx| tx.send(event.clone()).is_ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_fans_out_and_prunes_dead_subscribers() {
        let bus = RuntimeBus::new();
        let a = bus.subscribe();
        let b = bus.subscribe();
        bus.publish(RuntimeEvent::Readiness {
            process: ProcessId::Asr,
            ready: true,
        });
        assert!(a.try_recv().is_ok());
        assert!(b.try_recv().is_ok());
        drop(a);
        bus.publish(RuntimeEvent::Readiness {
            process: ProcessId::Asr,
            ready: false,
        });
        assert!(b.try_recv().is_ok());
        assert_eq!(bus.subscribers.lock().unwrap().len(), 1, "dead sub pruned");
    }
}
