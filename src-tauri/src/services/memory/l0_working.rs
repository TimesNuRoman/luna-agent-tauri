//! L0 — Working memory.
//!
//! In-RAM ring buffer of the last `CAP` chat messages from the
//! current task. Cheap, never persisted to disk (on restart the user
//! re-establishes context by talking to the agent).
//!
//! Concurrency: `parking_lot::Mutex`. Reads (e.g. for the chat
//! system-prompt prefix) take the lock for nanoseconds.

use std::collections::VecDeque;

use super::schema::ChatMsg;

/// Hard cap on the ring buffer. 32 messages = ~16 turns, which is
/// plenty for one focused task. If a session is longer than that, L1
/// is the source of truth (and L2 is the "smart" view).
pub const L0_CAP: usize = 32;

#[derive(Debug, Default)]
pub struct L0Working {
    buf: VecDeque<ChatMsg>,
}

impl L0Working {
    /// Append a message. Drops the oldest if at capacity.
    pub fn push(&mut self, msg: ChatMsg) {
        if self.buf.len() >= L0_CAP {
            self.buf.pop_front();
        }
        self.buf.push_back(msg);
    }

    /// Cheap clone of the buffer contents in arrival order.
    pub fn snapshot(&self) -> Vec<ChatMsg> {
        self.buf.iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Clear the buffer (e.g. when the user opens a new chat).
    pub fn clear(&mut self) {
        self.buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(role: &str, content: &str) -> ChatMsg {
        ChatMsg { role: role.into(), content: content.into() }
    }

    #[test]
    fn ring_buffer_evicts_oldest() {
        let mut l0 = L0Working::default();
        for i in 0..(L0_CAP + 5) {
            l0.push(m("user", &format!("msg {i}")));
        }
        let snap = l0.snapshot();
        assert_eq!(snap.len(), L0_CAP);
        // The 5 oldest ("msg 0" through "msg 4") should be gone.
        assert_eq!(snap[0].content, "msg 5");
        assert_eq!(snap[L0_CAP - 1].content, format!("msg {}", L0_CAP + 4));
    }

    #[test]
    fn clear_empties() {
        let mut l0 = L0Working::default();
        l0.push(m("user", "hi"));
        assert!(!l0.is_empty());
        l0.clear();
        assert!(l0.is_empty());
    }
}
