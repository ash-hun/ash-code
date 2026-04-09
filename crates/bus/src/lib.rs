//! ash-bus — in-process session event bus (M8).
//!
//! A `SessionBus` is a `HashMap<session_id, broadcast::Sender<BusEvent>>`.
//! Producers (currently the Rust query loop) call `publish`; subscribers
//! (currently nobody — wired up to consumers in M9) call `subscribe` to
//! get a `broadcast::Receiver<BusEvent>`. The bus is intentionally tiny
//! and unopinionated; the M8 plan keeps the consumer side empty so we
//! can evolve the wire shape based on real demand later.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::broadcast;

pub const CRATE_NAME: &str = "ash-bus";
pub const DEFAULT_CHANNEL_CAPACITY: usize = 256;

#[derive(Debug, Clone)]
pub enum BusEvent {
    UserMessage { text: String },
    AssistantText { text: String },
    ToolCall { id: String, name: String, args: String },
    ToolResult { name: String, ok: bool, body: String },
    TurnFinish { stop_reason: String, in_tok: i32, out_tok: i32 },
    TurnError { message: String },
    Cancelled { reason: String },
    Outcome { stop_reason: String, turns_taken: usize, denied: bool },
}

#[derive(Clone, Default)]
pub struct SessionBus {
    inner: Arc<RwLock<HashMap<String, broadcast::Sender<BusEvent>>>>,
    capacity: usize,
}

impl SessionBus {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            capacity: DEFAULT_CHANNEL_CAPACITY,
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            capacity: cap,
        }
    }

    fn ensure_sender(&self, session_id: &str) -> broadcast::Sender<BusEvent> {
        if let Some(tx) = self.inner.read().get(session_id) {
            return tx.clone();
        }
        let mut map = self.inner.write();
        map.entry(session_id.to_string())
            .or_insert_with(|| broadcast::channel(self.capacity).0)
            .clone()
    }

    /// Publish an event to a session's channel. Cheap when nobody is
    /// subscribed — `broadcast::send` returns an error in that case
    /// which we deliberately swallow.
    pub fn publish(&self, session_id: &str, event: BusEvent) {
        let tx = self.ensure_sender(session_id);
        let _ = tx.send(event);
    }

    pub fn subscribe(&self, session_id: &str) -> broadcast::Receiver<BusEvent> {
        self.ensure_sender(session_id).subscribe()
    }

    /// Drop the channel for a session. Existing receivers will get
    /// `RecvError::Closed` on their next `recv`.
    pub fn close(&self, session_id: &str) {
        self.inner.write().remove(session_id);
    }

    pub fn session_count(&self) -> usize {
        self.inner.read().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn publish_then_subscribe_misses_past_events() {
        // tokio::sync::broadcast does not replay history.
        let bus = SessionBus::new();
        bus.publish("s1", BusEvent::UserMessage { text: "hi".into() });
        let mut rx = bus.subscribe("s1");
        // No event waiting.
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn subscribe_then_publish_delivers() {
        let bus = SessionBus::new();
        let mut rx = bus.subscribe("s1");
        bus.publish("s1", BusEvent::UserMessage { text: "hi".into() });
        let event = rx.recv().await.unwrap();
        match event {
            BusEvent::UserMessage { text } => assert_eq!(text, "hi"),
            _ => panic!("wrong event"),
        }
    }

    #[tokio::test]
    async fn two_subscribers_both_receive() {
        let bus = SessionBus::new();
        let mut rx_a = bus.subscribe("s1");
        let mut rx_b = bus.subscribe("s1");
        bus.publish(
            "s1",
            BusEvent::AssistantText { text: "hello".into() },
        );
        let a = rx_a.recv().await.unwrap();
        let b = rx_b.recv().await.unwrap();
        match (a, b) {
            (BusEvent::AssistantText { text: ta }, BusEvent::AssistantText { text: tb }) => {
                assert_eq!(ta, "hello");
                assert_eq!(tb, "hello");
            }
            _ => panic!(),
        }
    }

    #[tokio::test]
    async fn close_drops_session_channel() {
        let bus = SessionBus::new();
        let _rx = bus.subscribe("doomed");
        assert_eq!(bus.session_count(), 1);
        bus.close("doomed");
        assert_eq!(bus.session_count(), 0);
    }

    #[tokio::test]
    async fn isolated_sessions_do_not_cross_talk() {
        let bus = SessionBus::new();
        let mut rx_a = bus.subscribe("a");
        let mut rx_b = bus.subscribe("b");
        bus.publish("a", BusEvent::UserMessage { text: "for a".into() });
        let a_event = rx_a.recv().await.unwrap();
        match a_event {
            BusEvent::UserMessage { text } => assert_eq!(text, "for a"),
            _ => panic!(),
        }
        assert!(rx_b.try_recv().is_err());
    }

    #[tokio::test]
    async fn outcome_event_is_terminal_marker() {
        let bus = SessionBus::new();
        let mut rx = bus.subscribe("s");
        bus.publish(
            "s",
            BusEvent::Outcome {
                stop_reason: "end_turn".to_string(),
                turns_taken: 2,
                denied: false,
            },
        );
        let event = rx.recv().await.unwrap();
        match event {
            BusEvent::Outcome { stop_reason, turns_taken, denied } => {
                assert_eq!(stop_reason, "end_turn");
                assert_eq!(turns_taken, 2);
                assert!(!denied);
            }
            _ => panic!(),
        }
    }
}
