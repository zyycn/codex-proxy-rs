//! 用量读取的单次拉取、取消安全、显式失败及提前关闭

use super::session::{HostPeer, receive, send_call, shutdown, start_session};
use gateway_plugin_sdk::{
    ErrorCode, Frame, Message, PluginFault,
    call::services::usage::*,
    client::{CallFuture, CallReply, PluginCall, PluginHandler, write_frame},
};
use serde_json::json;

struct UsageHandler;

impl PluginHandler for UsageHandler {
    fn call(&self, call: PluginCall) -> CallFuture<'_> {
        Box::pin(async move {
            let mut reader = call
                .host
                .query_usage_statistics(QueryUsageStatistics {
                    filter: UsageReadFilter {
                        records: UsageRecordFilter {
                            start_ms: 0,
                            end_ms: 1000,
                            provider: None,
                            model: None,
                            account_ref: None,
                            client_key_ref: None,
                        },
                        routing_group_ref: None,
                    },
                    group_by: UsageGrouping::Model,
                    batch_size: Some(1),
                })
                .await?;
            assert!(!reader.next_batch().await?.unwrap().complete);
            {
                let next = reader.next_batch();
                tokio::pin!(next);
                assert!(
                    std::future::poll_fn(|cx| std::task::Poll::Ready(next.as_mut().poll(cx)))
                        .await
                        .is_pending()
                );
            }
            match call.method.as_str() {
                "close" => {
                    reader.close().await?;
                    assert!(reader.next_batch().await?.is_none());
                }
                "error" => assert_eq!(
                    reader.next_batch().await.unwrap_err().code,
                    ErrorCode::Timeout
                ),
                _ => {
                    assert!(reader.next_batch().await?.unwrap().complete);
                    assert!(reader.next_batch().await?.is_none());
                }
            }
            Ok(CallReply::unary(json!(true), vec![]))
        })
    }
}

fn batch(complete: bool) -> UsageReadBatch {
    UsageReadBatch {
        observation: UsageReadObservation {
            generated_at_ms: 10,
            analytics: None,
            total_groups: Some(UsageCount(1)),
        },
        items: vec![UsageReadItem::Statistics(Box::new(UsageStatisticsRow {
            group: None,
            metrics: UsageStatisticsMetrics::default(),
        }))],
        complete,
    }
}

async fn callback(host: &mut HostPeer, expected: &str) -> u64 {
    let Message::Callback { id, method, .. } = receive(host).await.message else {
        panic!("callback expected");
    };
    assert_eq!(method, expected);
    id
}

async fn reply(host: &mut HostPeer, id: u64, value: impl serde::Serialize) {
    write_frame(
        &mut host.writer,
        &Frame {
            message: Message::Result {
                id,
                result: json!({}),
            },
            payload: serde_json::to_vec(&value).unwrap(),
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn usage_reader_retains_pending_reads_and_distinguishes_failure_from_completion() {
    for mode in ["complete", "error", "close"] {
        let (mut host, task) = start_session(UsageHandler).await;
        send_call(&mut host, 1, mode, json!({}), vec![]).await;
        let id = callback(&mut host, STATISTICS_QUERY).await;
        reply(
            &mut host,
            id,
            UsageReadOpened {
                reader: Some("reader".into()),
                batch: batch(false),
            },
        )
        .await;
        let read_id = callback(&mut host, READ_NEXT).await;
        match mode {
            "close" => {
                let close_id = callback(&mut host, READ_CLOSE).await;
                reply(&mut host, close_id, ()).await;
            }
            "error" => write_frame(
                &mut host.writer,
                &Frame {
                    message: Message::Error {
                        id: read_id,
                        error: PluginFault::new(ErrorCode::Timeout, "read expired"),
                    },
                    payload: vec![],
                },
            )
            .await
            .unwrap(),
            _ => reply(&mut host, read_id, batch(true)).await,
        }
        assert!(
            matches!(receive(&mut host).await.message, Message::Result { id: 1, result } if result == json!(true))
        );
        shutdown(&mut host, task).await;
    }
}
