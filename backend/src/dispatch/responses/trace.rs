//! Request-level trace state for Responses dispatch.

#[derive(Debug, Default)]
pub(super) struct ResponseDispatchTrace {
    next_attempt_index: i64,
    attempts: Vec<ResponseDispatchAttempt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResponseDispatchAttempt {
    index: i64,
    account_id: String,
}

impl ResponseDispatchTrace {
    pub(super) fn start_attempt(&mut self, account_id: &str) -> ResponseDispatchAttempt {
        let attempt = ResponseDispatchAttempt {
            index: self.next_attempt_index,
            account_id: account_id.to_string(),
        };
        self.next_attempt_index += 1;
        self.attempts.push(attempt.clone());
        attempt
    }

    pub(super) fn attempts(&self) -> &[ResponseDispatchAttempt] {
        &self.attempts
    }
}

impl ResponseDispatchAttempt {
    pub(super) fn index(&self) -> i64 {
        self.index
    }

    pub(super) fn account_id(&self) -> &str {
        &self.account_id
    }
}
