//! 互联只读查询的短期限流，不占用推理请求配额

use super::PeerLinkError;
use gateway_core::policy::ClientApiKeyId;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};

const WINDOW: Duration = Duration::from_secs(60);
const REQUESTS_PER_WINDOW: usize = 30;

#[derive(Default)]
pub(super) struct QueryRateLimit(Mutex<BTreeMap<ClientApiKeyId, VecDeque<Instant>>>);

impl QueryRateLimit {
    pub(super) fn acquire(&self, id: &ClientApiKeyId) -> Result<(), PeerLinkError> {
        let now = Instant::now();
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|_, requests| {
            while requests
                .front()
                .is_some_and(|time| now.duration_since(*time) >= WINDOW)
            {
                requests.pop_front();
            }
            !requests.is_empty()
        });
        let requests = entries.entry(id.clone()).or_default();
        if requests.len() >= REQUESTS_PER_WINDOW {
            let retry_after = requests.front().map_or(1, |time| {
                WINDOW
                    .saturating_sub(now.duration_since(*time))
                    .as_secs()
                    .saturating_add(1)
            });
            return Err(PeerLinkError::RateLimited { retry_after });
        }
        requests.push_back(now);
        Ok(())
    }
}
