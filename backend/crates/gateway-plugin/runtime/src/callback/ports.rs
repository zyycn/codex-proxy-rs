//! 组装实例宿主端口及调用资源索引，保持资源与插件实例同寿命

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};

use gateway_admin::model::AdminError;
use gateway_host::outbound::{HttpClient, NetworkPolicy};

use super::{
    CallResources, CallbackScope, MiddlewareCallback, PluginAccountPortSlot,
    PluginAffinityPortSlot, PluginClientIdentityPortSlot, PluginClientKeyPortSlot,
    PluginModelPortSlot, PluginResourcePorts,
};
use super::{
    accounts, affinity, client, data, keys, log, model, private_state, request_limits, resources,
    services,
};

pub(crate) struct PluginCallbackPorts {
    pub(crate) services: Arc<services::ServicePorts>,
    pub(crate) http: Arc<HttpClient>,
    pub(crate) accounts: Arc<PluginAccountPortSlot>,
    pub(crate) keys: Arc<PluginClientKeyPortSlot>,
    pub(crate) clients: Arc<PluginClientIdentityPortSlot>,
    pub(crate) models: Arc<PluginModelPortSlot>,
    pub(crate) affinity: Arc<PluginAffinityPortSlot>,
    pub(crate) resources: Arc<PluginResourcePorts>,
}

pub(crate) struct PluginCallbacks {
    pub(crate) registration_body_limit: Option<usize>,
    pub(super) services: Arc<services::ServicePorts>,
    pub(super) service_actor: gateway_admin::model::usage_data::UsageActor,
    pub(super) accounts: Arc<accounts::PluginAccounts>,
    pub(super) resources: Arc<resources::PluginResources>,
    pub(super) data: Arc<data::PluginData>,
    pub(super) keys: Arc<keys::PluginClientKeys>,
    pub(super) affinity: Arc<affinity::PluginAffinity>,
    pub(super) log: Arc<log::PluginLog>,
    pub(super) clients: Arc<client::PluginClients>,
    pub(super) models: Arc<model::PluginModels>,
    pub(super) private_state: Arc<private_state::PluginPrivateState>,
    pub(super) http: Arc<HttpClient>,
    pub(super) network: Arc<NetworkPolicy>,
    pub(super) scopes: Mutex<BTreeMap<String, Weak<CallbackScope>>>,
    pub(super) pending_middleware: Mutex<BTreeMap<String, Arc<dyn MiddlewareCallback>>>,
    pub(super) calls: Mutex<BTreeMap<u64, Arc<CallResources>>>,
    pub(super) maximum_payload: usize,
    pub(super) request_limits: Arc<request_limits::RequestLimits>,
}

impl PluginCallbacks {
    pub(crate) fn new(
        instance: &gateway_admin::model::plugins::instances::PluginInstance,
        maximum_payload: usize,
        manifest: &gateway_plugin_sdk::Manifest,
        log_slots: Arc<tokio::sync::Semaphore>,
        private_state: Arc<private_state::PluginPrivateState>,
        ports: PluginCallbackPorts,
        request_limits: Arc<request_limits::RequestLimits>,
    ) -> Result<Self, AdminError> {
        let clients = Arc::new(client::PluginClients::new(ports.clients));
        Ok(Self {
            registration_body_limit: None,
            services: ports.services,
            service_actor: gateway_admin::model::usage_data::UsageActor::Plugin {
                publisher: manifest.publisher.clone(),
                name: manifest.name.clone(),
                instance_id: instance.id.clone(),
            },
            resources: Arc::new(resources::PluginResources::new(instance, ports.resources)),
            data: Arc::new(data::PluginData::new(
                ports.accounts.clone(),
                ports.keys.clone(),
            )),
            accounts: Arc::new(accounts::PluginAccounts::new(ports.accounts)),
            keys: Arc::new(keys::PluginClientKeys::new(instance, ports.keys)),
            affinity: Arc::new(affinity::PluginAffinity::new(ports.affinity)),
            log: Arc::new(
                log::PluginLog::new(manifest, log_slots)
                    .map_err(|_| AdminError::invalid("插件清单身份不合法"))?,
            ),
            clients: clients.clone(),
            models: Arc::new(model::PluginModels::new(
                ports.models,
                clients,
                request_limits.clone(),
                maximum_payload,
            )),
            private_state,
            http: ports.http,
            network: Arc::new(NetworkPolicy::unrestricted()),
            scopes: Mutex::new(BTreeMap::new()),
            pending_middleware: Mutex::new(BTreeMap::new()),
            calls: Mutex::new(BTreeMap::new()),
            maximum_payload,
            request_limits,
        })
    }
}
