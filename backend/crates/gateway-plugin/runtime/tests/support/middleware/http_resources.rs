//! 测试插件使用的 HTTP 资源回调与上传生命周期检查

use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::middleware::http as wire,
    client::{HostClient, HostReply, SessionError},
};
use serde_json::{Value, json};

async fn call(host: &HostClient, method: &str, params: Value) -> Result<HostReply, PluginFault> {
    host.call(method, params, Vec::new())
        .await
        .map_err(SessionError::into_plugin_fault)
}

async fn create(host: &HostClient) -> Result<String, PluginFault> {
    let reply = call(host, wire::BODY_CREATE_METHOD, json!({})).await?;
    let pipe: wire::BodyPipe = serde_json::from_value(reply.result).unwrap();
    Ok(pipe.handle)
}

pub async fn check_upload_lifecycle(host: &HostClient) -> Result<(), PluginFault> {
    let mut handles = Vec::new();
    for _ in 0..16 {
        handles.push(create(host).await?);
    }
    assert_eq!(create(host).await.unwrap_err().code, ErrorCode::Capacity);

    // 关闭立即归还名额；重复关闭不能误伤后续分配
    let closed = handles.remove(0);
    for _ in 0..2 {
        call(host, wire::BODY_CLOSE_METHOD, json!({"handle":closed})).await?;
    }
    handles.push(create(host).await?);
    assert_eq!(create(host).await.unwrap_err().code, ErrorCode::Capacity);

    // 写完后读端仍能取得 EOF，但不再占用活动上传名额
    let finished = handles.remove(0);
    call(
        host,
        wire::BODY_WRITE_METHOD,
        json!({"handle":finished,"eof":true,"trailers":null}),
    )
    .await?;
    handles.push(create(host).await?);
    let eof = call(
        host,
        wire::BODY_READ_METHOD,
        json!({"handle":finished,"maximum_bytes":1}),
    )
    .await?;
    assert_eq!(eof.result["eof"], true);

    // 正文已转交给子请求，关闭返回正文必须唤醒被单帧背压阻塞的上传
    let active = handles.remove(0);
    let reply = call(
        host,
        wire::DISPATCH_METHOD,
        json!({
            "settings":null,"method":"POST","uri":"/child","version":"HTTP/1.1",
            "headers":[],"timeout_ms":null,"body":{"kind":"handle","handle":active},
        }),
    )
    .await?;
    let response: wire::Response = serde_json::from_value(reply.result).unwrap();
    let wire::Body::Handle { handle } = response.body else {
        panic!("response handle expected")
    };
    let write = json!({"handle":active,"eof":false,"trailers":null});
    host.call(wire::BODY_WRITE_METHOD, write.clone(), vec![1])
        .await
        .map_err(SessionError::into_plugin_fault)?;
    let blocked = host.call(wire::BODY_WRITE_METHOD, write, vec![2]);
    tokio::pin!(blocked);
    tokio::select! {
        _ = &mut blocked => panic!("upload should wait for the reader"),
        () = tokio::time::sleep(std::time::Duration::from_millis(20)) => {},
    }
    call(host, wire::BODY_CLOSE_METHOD, json!({"handle":handle})).await?;
    let error = blocked
        .await
        .err()
        .expect("closed reader must fail the write")
        .into_plugin_fault();
    assert_eq!(error.code, ErrorCode::Conflict);
    handles.push(create(host).await?);
    assert_eq!(create(host).await.unwrap_err().code, ErrorCode::Capacity);
    for handle in handles {
        call(host, wire::BODY_CLOSE_METHOD, json!({"handle":handle})).await?;
    }
    Ok(())
}
