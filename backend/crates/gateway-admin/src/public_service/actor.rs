//! 公开服务写入主体由受信宿主边界设置，插件输入不能覆盖

use crate::model::usage_data::UsageActor;
use std::future::Future;

tokio::task_local! {
    static ACTOR: UsageActor;
}

/// 与 middleware Origin 独立传递，嵌套服务不会丢失真实调用主体
pub async fn scope_actor<F: Future>(actor: UsageActor, future: F) -> F::Output {
    ACTOR.scope(actor, future).await
}

pub(super) fn current_actor() -> Result<UsageActor, crate::model::usage_data::UsageDataError> {
    ACTOR.try_with(Clone::clone).map_err(|_| {
        crate::model::usage_data::UsageDataError::invalid(
            "usage mutation requires a trusted caller identity",
        )
    })
}
