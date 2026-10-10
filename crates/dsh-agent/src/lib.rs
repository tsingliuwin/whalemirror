//! dsh-agent — the `Agent` interface vocabulary: inbox, status, and the live
//! `agent/*` event vocabulary.

pub mod events;

pub use events::{
    AgentErrorOccurred, AgentErrorPayload, AgentInboxClaimed, AgentInboxDiscarded,
    AgentInboxInserted, AgentPreStep, AgentRequest, AgentRequestError, AgentRequestPayload,
    AgentStatusChanged, AgentTurnStopping, InboxClaimedPayload, PreStepDecision, PreStepInput,
    RequestErrorAction, RequestErrorPayload, TurnStoppingPayload,
};

use dsh_llm::Message;

/// One of the two ordered pending-message lists owned by an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InboxTarget {
    NextTurn,
    NextStep,
}

/// Whether an agent is working (`Running`) or between turns (`Idle`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    Running,
}

/// Incremental projection of the agent's pending input.
///
/// v1 keeps the two lists in memory; the reference also replays durable
/// `agent/inbox/spliced` log events, which arrives with the persistence
/// milestone. Splice/claim semantics are identical to the reference.
#[derive(Default)]
pub struct Inbox {
    next_turn: Vec<Message>,
    next_step: Vec<Message>,
}

impl Inbox {
    pub fn new() -> Self {
        Self::default()
    }

    /// Prompts awaiting individual turns.
    pub fn next_turn(&self) -> &[Message] {
        &self.next_turn
    }

    /// Input awaiting the next step boundary.
    pub fn next_step(&self) -> &[Message] {
        &self.next_step
    }

    /// Whether either pending-message list contains work.
    pub fn has_pending(&self) -> bool {
        !self.next_turn.is_empty() || !self.next_step.is_empty()
    }

    /// Whether the next-step list contains work.
    pub fn has_pending_next_step(&self) -> bool {
        !self.next_step.is_empty()
    }

    /// Remove every pending message, next-step before next-turn.
    pub fn clear(&mut self) {
        self.next_step.clear();
        self.next_turn.clear();
    }

    /// Apply standard splice semantics to one pending list.
    ///
    /// `usize::MAX` for `start` means "append" (the reference uses `Infinity`).
    pub fn splice(&mut self, target: InboxTarget, start: usize, delete_count: usize, inserted: Vec<Message>) {
        let list = match target {
            InboxTarget::NextTurn => &mut self.next_turn,
            InboxTarget::NextStep => &mut self.next_step,
        };
        let start = start.min(list.len());
        let delete = delete_count.min(list.len() - start);
        list.splice(start..start + delete, inserted);
    }

    /// Append one message to a pending list.
    pub fn append(&mut self, target: InboxTarget, message: Message) {
        self.splice(target, usize::MAX, 0, vec![message]);
    }

    /// 插话提升：把 next-turn 中 id 命中的一条移入 next-step（本轮下个
    /// 步界消费——上游 queue.steer 的消费语义）。未命中返回 false。
    pub fn promote(&mut self, message_id: &str) -> bool {
        if let Some(pos) = self.next_turn.iter().position(|m| m.id.0 == message_id) {
            let m = self.next_turn.remove(pos);
            self.next_step.push(m);
            true
        } else {
            false
        }
    }

    /// 全部提升（上游空稿加速手势 steerQueue）：next-turn 整批移入
    /// next-step，返回移动条数。
    pub fn promote_all(&mut self) -> usize {
        let moved = std::mem::take(&mut self.next_turn);
        let n = moved.len();
        self.next_step.extend(moved);
        n
    }

    /// Remove and return the complete batch proposed for one step: all
    /// next-step input followed by one queued turn when `target` is next-turn.
    pub fn claim(&mut self, target: InboxTarget, _turn: u64) -> Vec<Message> {
        let mut claimed = std::mem::take(&mut self.next_step);
        if target == InboxTarget::NextTurn && !self.next_turn.is_empty() {
            claimed.push(self.next_turn.remove(0));
        }
        claimed
    }
}
#[cfg(test)]
mod promote_tests {
    use super::*;

    fn msg(id: &str, text: &str) -> Message {
        let mut m = Message::user_text(text);
        m.id = dsh_llm::MessageId(id.into());
        m
    }

    #[test]
    fn promote_moves_one_matching_to_next_step() {
        let mut inbox = Inbox::new();
        inbox.append(InboxTarget::NextTurn, msg("a", "one"));
        inbox.append(InboxTarget::NextTurn, msg("b", "two"));
        assert!(inbox.promote("a"));
        assert_eq!(inbox.next_turn.len(), 1);
        assert_eq!(inbox.next_turn[0].id.0, "b");
        assert_eq!(inbox.next_step.len(), 1);
        assert_eq!(inbox.next_step[0].id.0, "a");
        // claim（step 2 起 target=NextStep）：提升消息本轮消费
        let claimed = inbox.claim(InboxTarget::NextStep, 1);
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id.0, "a");
    }

    #[test]
    fn promote_miss_returns_false() {
        let mut inbox = Inbox::new();
        inbox.append(InboxTarget::NextTurn, msg("a", "one"));
        assert!(!inbox.promote("zzz"));
        assert_eq!(inbox.next_turn.len(), 1);
    }

    #[test]
    fn promote_all_flushes_next_turn_into_next_step() {
        let mut inbox = Inbox::new();
        inbox.append(InboxTarget::NextTurn, msg("a", "one"));
        inbox.append(InboxTarget::NextTurn, msg("b", "two"));
        inbox.append(InboxTarget::NextStep, msg("s", "steered"));
        assert_eq!(inbox.promote_all(), 2);
        assert!(inbox.next_turn.is_empty());
        // 次序：既有 next-step 在前，提升批按原序随后
        assert_eq!(inbox.next_step.iter().map(|m| m.id.0.as_str()).collect::<Vec<_>>(), ["s", "a", "b"]);
    }
}
