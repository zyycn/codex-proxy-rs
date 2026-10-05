//! 插件运行时测试共用的制品、实例与端口构造辅助

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write as _,
    sync::{Arc, OnceLock},
};

use flate2::{Compression, write::GzEncoder};
use gateway_plugin_sdk::{
    Capability, ContributionDeclaration, Contributions, Engines, Manifest, PROTOCOL_VERSION,
    Package, PackageTarget, RuntimeKind, Stage,
};
use sha2::{Digest as _, Sha256};
pub mod environment;
pub mod native;
pub mod store;

pub const DEFAULT_PLUGIN_ID: &str = "test.example";

pub fn contribution(
    capability: Capability,
    stages: Vec<Stage>,
    input_formats: Vec<String>,
    output_formats: Vec<String>,
) -> (Capability, ContributionDeclaration) {
    contribution_for_id(
        DEFAULT_PLUGIN_ID,
        capability,
        stages,
        input_formats,
        output_formats,
    )
}

pub fn contribution_for_id(
    plugin_id: &str,
    capability: Capability,
    stages: Vec<Stage>,
    input_formats: Vec<String>,
    output_formats: Vec<String>,
) -> (Capability, ContributionDeclaration) {
    let local_id = match capability {
        Capability::FrontendAuthentication => "frontendAuthentication",
        Capability::Scheduler => "scheduler",
        Capability::ModelRouter => "modelRouter",
        Capability::ModelCatalog => "modelCatalog",
        Capability::RetryPolicy => "retryPolicy",
        Capability::Middleware => "middleware",
        Capability::UpstreamAdapter => "upstreamAdapter",
        Capability::Observer => "observer",
        Capability::CommandLine => "commandLine",
        Capability::Management => "management",
        Capability::Maintenance => "maintenance",
    };
    (
        capability,
        ContributionDeclaration {
            id: format!("{plugin_id}.{local_id}"),
            version: if capability == Capability::Middleware {
                3
            } else {
                1
            },
            stages,
            input_formats,
            output_formats,
        },
    )
}

struct WorkerFixture {
    bytes: Vec<u8>,
    digest: String,
}

fn worker_fixture() -> &'static WorkerFixture {
    static WORKER: OnceLock<WorkerFixture> = OnceLock::new();
    WORKER.get_or_init(|| {
        let bytes = std::fs::read(env!("CARGO_BIN_EXE_gateway-plugin-test-worker"))
            .expect("Cargo 应为集成测试构建 Rust 测试插件");
        let digest = hex::encode(Sha256::digest(&bytes));
        WorkerFixture { bytes, digest }
    })
}

pub fn worker() -> &'static [u8] {
    &worker_fixture().bytes
}

pub fn package(worker: &[u8]) -> Arc<[u8]> {
    package_with_contributions(worker, Contributions::new())
}

pub fn package_with_contributions(worker: &[u8], contributes: Contributions) -> Arc<[u8]> {
    package_with_contributions_and_state(worker, contributes, vec![])
}

pub fn package_with_contributions_for_id(
    worker: &[u8],
    plugin_id: &str,

    contributes: Contributions,
) -> Arc<[u8]> {
    package_with_identity_and_state(worker, plugin_id, contributes, vec![])
}

pub fn package_with_contributions_and_state(
    worker: &[u8],

    contributes: Contributions,
    state: Vec<gateway_plugin_sdk::StateNamespace>,
) -> Arc<[u8]> {
    package_with_identity_and_state(worker, DEFAULT_PLUGIN_ID, contributes, state)
}

fn package_with_identity_and_state(
    worker: &[u8],
    plugin_id: &str,

    contributes: Contributions,
    state: Vec<gateway_plugin_sdk::StateNamespace>,
) -> Arc<[u8]> {
    let (publisher, name) = plugin_id
        .split_once('.')
        .expect("测试插件 ID 必须使用 publisher.name");
    let digest = if std::ptr::eq(worker, self::worker()) {
        worker_fixture().digest.clone()
    } else {
        hex::encode(Sha256::digest(worker))
    };
    let files = BTreeMap::from([("bin/worker".to_owned(), digest)]);
    let manifest = Manifest {
        manifest_version: gateway_plugin_sdk::MANIFEST_VERSION,
        name: name.to_owned(),
        display_name: "示例".to_owned(),
        publisher: publisher.to_owned(),
        version: "1.0.0".parse().unwrap(),
        description: "测试插件".to_owned(),
        license: "MIT".to_owned(),
        author: Some("test".to_owned()),
        engines: Engines {
            codex_proxy_rs: ">=1.0.0, <2.0.0".parse().unwrap(),
        },
        main: "bin/worker".to_owned(),
        runtime: RuntimeKind::TrustedProcess,
        contributes,

        configuration_schema: serde_json::json!({}),
        secret_fields: BTreeSet::new(),
        resources: BTreeMap::new(),
        icon: None,
        state,
        package: Some(Package {
            protocol_version: PROTOCOL_VERSION,
            target: PackageTarget {
                os: std::env::consts::OS.to_owned(),
                architecture: std::env::consts::ARCH.to_owned(),
            },
            files,
        }),
    };
    let manifest = serde_json::to_vec(&manifest).unwrap();
    let key = hex::encode(Sha256::digest(&manifest));
    // nextest 的用例分属独立进程；清单含 worker 摘要，只共享不可变归档
    // 私有解包目录、会话、进程及 Store 仍由每个用例独立创建
    let cache = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("plugin-packages-v1");
    std::fs::create_dir_all(&cache).unwrap();
    let path = cache.join(format!("{key}.tar.gz"));
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(cache.join(format!("{key}.lock")))
        .unwrap();
    lock.lock().unwrap();
    match std::fs::read(&path) {
        Ok(bytes) => return bytes.into(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("read test plugin archive: {error}"),
    }
    let package = archive(BTreeMap::from([
        ("plugin.json".into(), manifest),
        ("bin/worker".into(), worker.to_vec()),
    ]));
    // 原子发布避免构建中断后其他进程读到不完整的压缩包，文件锁随进程退出释放
    let mut temporary = tempfile::NamedTempFile::new_in(cache).unwrap();
    temporary.write_all(&package).unwrap();
    temporary.persist(path).unwrap();
    package
}

pub fn archive(files: BTreeMap<String, Vec<u8>>) -> Arc<[u8]> {
    let mut tar = tar::Builder::new(Vec::new());
    for (path, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o700);
        header.set_cksum();
        tar.append_data(&mut header, path, bytes.as_slice())
            .unwrap();
    }
    let mut gzip = GzEncoder::new(Vec::new(), Compression::fast());
    gzip.write_all(&tar.into_inner().unwrap()).unwrap();
    gzip.finish().unwrap().into()
}
