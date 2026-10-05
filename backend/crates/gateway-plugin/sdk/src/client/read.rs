//! 读取由资源持有，取消等待不会丢弃已发出的 RPC 或已收到的部分结果

use std::{future::Future, pin::Pin};

pub(super) struct PendingRead<T>(Option<Pin<Box<dyn Future<Output = T> + Send>>>);

impl<T> Default for PendingRead<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<T> PendingRead<T> {
    pub(super) async fn run<F: Future<Output = T> + Send + 'static>(
        &mut self,
        start: impl FnOnce() -> F,
    ) -> T {
        // 不后台预读；只有消费者再次等待时才继续驱动同一次读取
        let result = self.0.get_or_insert_with(|| Box::pin(start())).await;
        self.0 = None;
        result
    }

    pub(super) fn is_pending(&self) -> bool {
        self.0.is_some()
    }

    pub(super) fn clear(&mut self) {
        self.0 = None;
    }
}
