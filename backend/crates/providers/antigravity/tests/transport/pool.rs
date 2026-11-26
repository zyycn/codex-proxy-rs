//! 用回环保活连接验证账号隔离、当前请求材料与客户端缓存淘汰

use crate::{
    profile::{ClientProfile, Selection},
    transport::ClientPool,
};
use gateway_core::{
    account::{CredentialRevision, OutboundProxy, ProviderAccount, ProviderAccountId},
    routing::ProviderKind,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::{JoinHandle, JoinSet},
};
use url::Url;

struct RecordedRequest {
    connection: usize,
    headers: BTreeMap<String, String>,
    body: Value,
}

struct KeepAlivePeer {
    origin: Url,
    connections: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    task: JoinHandle<()>,
}

impl KeepAlivePeer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let connections = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let count = connections.clone();
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            let mut handlers = JoinSet::new();
            while let Ok((socket, _)) = listener.accept().await {
                let connection = count.fetch_add(1, Ordering::SeqCst);
                handlers.spawn(serve_connection(socket, connection, recorded.clone()));
            }
        });
        Self {
            origin,
            connections,
            requests,
            task,
        }
    }

    fn connection_count(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

impl Drop for KeepAlivePeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve_connection(
    mut socket: TcpStream,
    connection: usize,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
) {
    loop {
        let mut raw = Vec::new();
        while !raw.ends_with(b"\r\n\r\n") {
            let mut byte = [0; 1];
            if !matches!(socket.read(&mut byte).await, Ok(1)) {
                return;
            }
            raw.push(byte[0]);
            assert!(raw.len() < 65536);
        }
        let raw = String::from_utf8(raw).unwrap();
        let headers = raw
            .lines()
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().to_owned()))
            .collect::<BTreeMap<_, _>>();
        let length = headers["content-length"].parse().unwrap();
        let mut body = vec![0; length];
        if socket.read_exact(&mut body).await.is_err() {
            return;
        }
        requests.lock().unwrap().push(RecordedRequest {
            connection,
            headers,
            body: serde_json::from_slice(&body).unwrap(),
        });
        let body = br#"{"response":{"candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}]}}"#;
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
            body.len()
        );
        if socket.write_all(headers.as_bytes()).await.is_err()
            || socket.write_all(body).await.is_err()
            || socket.flush().await.is_err()
        {
            return;
        }
    }
}

fn account(id: &str, revision: u64) -> ProviderAccount {
    ProviderAccount::new(
        ProviderAccountId::new(id).unwrap(),
        ProviderKind::new("antigravity").unwrap(),
        id.into(),
        None,
        "oauth".into(),
        CredentialRevision::new(revision).unwrap(),
        None,
    )
}

async fn generate(
    pool: &ClientPool,
    origin: &Url,
    account: &ProviderAccount,
    profile: &ClientProfile,
    project: &str,
    token: &str,
) {
    let client = pool.client(origin, account, profile).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        client
            .generate(
                project,
                "fixture-model",
                token,
                br#"{"contents":[{"parts":[{"text":"ping","thoughtSignature":"fixture-signature"}]}]}"#,
                false.into(),
                &[],
            )
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
    })
    .await
    .expect("local keep-alive peer must respond");
}

#[tokio::test]
async fn repeated_account_reuses_tcp_without_caching_token_project_or_prompt() {
    let peer = KeepAlivePeer::start().await;
    let pool = ClientPool::default();
    let account = account("acct_one", 1);
    let mut profile = Selection::default().resolve("2.34.0");
    for index in 0..5 {
        profile.system_prompt = format!("fixture prompt {index}");
        generate(
            &pool,
            &peer.origin,
            &account,
            &profile,
            &format!("fixture-project-{index}"),
            &format!("fixture-token-{index}"),
        )
        .await;
    }
    assert_eq!(peer.connection_count(), 1);
    let sent = peer.requests.lock().unwrap();
    assert_eq!(sent.len(), 5);
    for (index, request) in sent.iter().enumerate() {
        assert_eq!(
            request.headers["authorization"],
            format!("Bearer fixture-token-{index}")
        );
        assert_eq!(request.body["project"], format!("fixture-project-{index}"));
        assert_eq!(
            request.body["request"]["systemInstruction"],
            json!({"parts": [{"text": format!("fixture prompt {index}")}]}),
        );
        assert_eq!(
            request.body["request"]["contents"][0]["parts"][0]["thoughtSignature"],
            "fixture-signature"
        );
    }
}

#[tokio::test]
async fn account_revision_and_user_agent_isolate_connection_pools() {
    let peer = KeepAlivePeer::start().await;
    let pool = ClientPool::default();
    let profile = Selection::default().resolve("2.34.0");
    let changed_profile = Selection::default().resolve("2.35.0");
    for (id, revision, profile) in [
        ("acct_one", 1, &profile),
        ("acct_two", 1, &profile),
        ("acct_one", 2, &profile),
        ("acct_one", 1, &changed_profile),
        ("acct_one", 1, &profile),
    ] {
        generate(
            &pool,
            &peer.origin,
            &account(id, revision),
            profile,
            "fixture-project",
            "fixture-token",
        )
        .await;
    }
    assert_eq!(peer.connection_count(), 4);
    let sent = peer.requests.lock().unwrap();
    assert_eq!(sent[0].connection, sent[4].connection);
    assert_ne!(sent[0].headers["user-agent"], sent[3].headers["user-agent"]);
}

#[tokio::test]
async fn proxy_routes_keep_independent_pools_and_preserve_the_direct_pool() {
    let direct = KeepAlivePeer::start().await;
    let first_proxy = KeepAlivePeer::start().await;
    let second_proxy = KeepAlivePeer::start().await;
    let pool = ClientPool::default();
    let profile = Selection::default().resolve("2.34.0");
    for proxy in [
        None,
        Some(&first_proxy.origin),
        Some(&second_proxy.origin),
        None,
    ] {
        let account = account("acct_one", 1)
            .with_outbound_proxy(proxy.map(|url| OutboundProxy::parse(url.as_str()).unwrap()));
        generate(
            &pool,
            &direct.origin,
            &account,
            &profile,
            "fixture-project",
            "fixture-token",
        )
        .await;
    }
    assert_eq!(direct.connection_count(), 1);
    assert_eq!(first_proxy.connection_count(), 1);
    assert_eq!(second_proxy.connection_count(), 1);
    assert_eq!(direct.requests.lock().unwrap().len(), 2);
    assert_eq!(first_proxy.requests.lock().unwrap().len(), 1);
    assert_eq!(second_proxy.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cache_evicts_the_least_recently_used_client_after_sixty_four_identities() {
    let peer = KeepAlivePeer::start().await;
    let pool = ClientPool::default();
    let profile = Selection::default().resolve("2.34.0");
    for index in (0..64).chain([0, 64, 0, 1]) {
        generate(
            &pool,
            &peer.origin,
            &account(&format!("acct_{index}"), 1),
            &profile,
            "fixture-project",
            "fixture-token",
        )
        .await;
    }
    let sent = peer.requests.lock().unwrap();
    assert_eq!(sent[0].connection, sent[64].connection);
    assert_eq!(sent[0].connection, sent[66].connection);
    assert_ne!(sent[1].connection, sent[67].connection);
    assert_eq!(peer.connection_count(), 66);
}
