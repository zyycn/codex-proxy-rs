//! 有界复用账号隔离的 HTTP 客户端；令牌、项目与系统指令仍按本次请求构造

use std::{collections::VecDeque, sync::Mutex};

use gateway_core::{
    account::{CredentialRevision, OutboundProxy, ProviderAccount, ProviderAccountId},
    error::{ProviderError, ProviderErrorKind},
};
use reqwest::Client;
use url::Url;

use super::{CloudCodeClient, unsent};
use crate::profile::ClientProfile;

const MAX_CACHED_CLIENTS: usize = 64;

#[derive(PartialEq, Eq)]
struct Identity {
    account: ProviderAccountId,
    revision: CredentialRevision,
    proxy: Option<OutboundProxy>,
    user_agent: String,
}

#[derive(Default)]
pub(crate) struct ClientPool {
    clients: Mutex<VecDeque<(Identity, Client)>>,
}

impl ClientPool {
    pub(crate) fn client(
        &self,
        origin: &Url,
        account: &ProviderAccount,
        profile: &ClientProfile,
    ) -> Result<CloudCodeClient, ProviderError> {
        let identity = Identity {
            account: account.id().clone(),
            revision: account.revision(),
            proxy: account.outbound_proxy().cloned(),
            user_agent: profile.user_agent(),
        };
        let mut clients = self
            .clients
            .lock()
            .map_err(|_| unsent(ProviderErrorKind::ProviderInfrastructureUnavailable))?;
        let existing = clients
            .iter()
            .position(|(key, _)| key == &identity)
            .and_then(|index| clients.remove(index));
        let client = if let Some((_, client)) = existing {
            client
        } else {
            CloudCodeClient::build_client(account.outbound_proxy(), profile)?
        };
        clients.push_back((identity, client.clone()));
        if clients.len() > MAX_CACHED_CLIENTS {
            clients.pop_front();
        }
        drop(clients);
        // 系统提示不缓存，画像只修改提示词时也立即使用本次冻结值
        CloudCodeClient::with_client(origin, client, profile)
    }
}
