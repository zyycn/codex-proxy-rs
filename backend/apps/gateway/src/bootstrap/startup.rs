//! 启动模式共享的基础设施与 Provider 组装，不启动 HTTP 或 Worker

use std::{path::PathBuf, sync::Arc};

use gateway_admin::ports::provider::ProviderAdminRegistry;
use gateway_core::{engine::provider::ProviderRegistry, time::DeploymentTimeZone};
use gateway_host::{ConfigError, HostBundle, HostConfig, outbound::HttpClient};
use gateway_store::{StoreBundle, StoreConfig};

use super::BootstrapError;

pub(super) enum Mode {
    Server,
    Command,
    Help,
}

pub(super) struct Environment {
    pub(super) host: HostBundle,
    pub(super) store: StoreBundle,
    pub(super) timezone: DeploymentTimeZone,
    pub(super) plugin_cache: PathBuf,
    pub(super) version: String,
    pub(super) plugin_http: Arc<HttpClient>,
}

impl Environment {
    pub(super) async fn initialize(
        host: HostConfig,
        store: StoreConfig,
        mode: Mode,
    ) -> Result<Self, BootstrapError> {
        let timezone = host.timezone;
        let store = store.with_timezone(timezone);
        let plugin_cache = host.runtime_data_dir().join("plugins");
        let host = match mode {
            Mode::Server => gateway_host::initialize(host).await?,
            Mode::Command | Mode::Help => gateway_host::initialize_command_line(host).await?,
        };
        host.report_startup_ready("Host");
        let store = match mode {
            Mode::Server => gateway_store::initialize(store).await?,
            Mode::Command => gateway_store::initialize_command_line(store).await?,
            Mode::Help => gateway_store::initialize_read_only(store).await?,
        };
        host.report_startup_ready("Store");
        let version = host
            .system_operations()
            .version()
            .await
            .map_err(|_| ConfigError::InvalidField("host.version"))?
            .version;
        let plugin_http = Arc::new(HttpClient::new()?);
        Ok(Self {
            host,
            store,
            timezone,
            plugin_cache,
            version,
            plugin_http,
        })
    }
}

pub(super) struct Providers {
    pub(super) openai: provider_openai::ProviderBundle,
    pub(super) xai: provider_xai::ProviderBundle,
    pub(super) core: ProviderRegistry,
    pub(super) admin: ProviderAdminRegistry,
    pub(super) peer: gateway_admin::PeerUpstreamBundle,
}

impl Providers {
    pub(super) async fn initialize(
        config: provider_openai::OpenAiConfig,
        environment: &Environment,
    ) -> Result<Self, BootstrapError> {
        let ports = environment.store.provider_ports();
        let upstream_reader = Arc::new(gateway_core::account::UpstreamInfoReader::default());
        let openai = provider_openai::initialize(
            config.with_timezone(environment.timezone),
            ports.clone(),
            upstream_reader.clone(),
        )
        .await?;
        environment.host.report_startup_ready("OpenAI Provider");
        let xai = provider_xai::initialize(ports).await?;
        environment.host.report_startup_ready("xAI Provider");
        let core = ProviderRegistry::new([openai.core_provider(), xai.core_provider()])?;
        let admin = ProviderAdminRegistry::new([openai.admin_provider(), xai.admin_provider()])
            .map_err(|_| gateway_admin::model::AdminError::invalid("Provider 管理身份冲突"))?;
        let peer = gateway_admin::initialize_peer_upstream(
            environment.store.provider_ports().accounts(),
            environment.store.upstream_info(),
            &admin,
            Arc::new(gateway_host::peer::HttpPeerTransport::with_client_builder(
                provider_openai::build_reqwest_client_with_custom_ca,
            )),
        )
        .map_err(|_| gateway_admin::model::AdminError::invalid("互联后台任务配置无效"))?;
        upstream_reader
            .bind(&peer.scheduling_source())
            .map_err(|_| gateway_admin::model::AdminError::invalid("互联观测端口重复绑定"))?;
        Ok(Self {
            peer,
            openai,
            xai,
            core,
            admin,
        })
    }
}
