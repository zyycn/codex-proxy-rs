//! 用量查询的按需批次读取，取消等待保留同一次 RPC

use super::{HostClient, payload_call, pending_read::PendingRead};
use crate::{PluginFault, call::services::usage::*};

pub struct UsageReader {
    host: HostClient,
    reader: Option<String>,
    first: Option<UsageReadBatch>,
    read: PendingRead<Result<UsageReadBatch, PluginFault>>,
}

impl HostClient {
    /// 在一次只读快照内扫描保留的原生请求事实
    ///
    /// # Errors
    /// 查询无效、存储超时或父调用终结时返回错误
    pub async fn scan_usage_records(
        &self,
        request: ScanUsageRecords,
    ) -> Result<UsageReader, PluginFault> {
        let opened = payload_call(self, RECORDS_SCAN, request).await?;
        Ok(UsageReader::new(self.clone(), opened))
    }

    /// 查询完整分组统计，首行包含全局总计
    ///
    /// # Errors
    /// 查询无效、统计未就绪、存储超时或父调用终结时返回错误
    pub async fn query_usage_statistics(
        &self,
        request: QueryUsageStatistics,
    ) -> Result<UsageReader, PluginFault> {
        let opened = payload_call(self, STATISTICS_QUERY, request).await?;
        Ok(UsageReader::new(self.clone(), opened))
    }
}

impl UsageReader {
    fn new(host: HostClient, opened: UsageReadOpened) -> Self {
        Self {
            host,
            reader: opened.reader,
            first: Some(opened.batch),
            read: PendingRead::default(),
        }
    }

    /// 按需读取下一批，完整结束后返回 None
    /// 取消一次等待后再次调用会继续同一次读取，父调用结束时宿主回收句柄
    ///
    /// # Errors
    /// 超时、取消、容量超限和存储失败保留各自错误，不作为完整结束
    pub async fn next_batch(&mut self) -> Result<Option<UsageReadBatch>, PluginFault> {
        if let Some(first) = self.first.take() {
            return Ok(Some(first));
        }
        let Some(reader) = &self.reader else {
            return Ok(None);
        };
        let result = self
            .read
            .run(|| {
                let host = self.host.clone();
                let request = UsageReadHandle {
                    reader: reader.clone(),
                };
                async move { payload_call(&host, READ_NEXT, request).await }
            })
            .await;
        if result.as_ref().map_or(true, |batch| batch.complete) {
            self.reader = None;
        }
        result.map(Some)
    }

    /// 提前关闭当前读取，释放宿主快照与连接
    ///
    /// # Errors
    /// 父调用终结或句柄失效时返回错误
    pub async fn close(&mut self) -> Result<(), PluginFault> {
        if let Some(reader) = &self.reader {
            payload_call::<_, ()>(
                &self.host,
                READ_CLOSE,
                UsageReadHandle {
                    reader: reader.clone(),
                },
            )
            .await?;
            self.reader = None;
        }
        self.first = None;
        self.read.clear();
        Ok(())
    }
}
