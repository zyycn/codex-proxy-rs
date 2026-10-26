//! 配置提交与迁移恢复共用的状态激活和发布收尾

use gateway_core::routing::extensions::ExtensionSetReference;

use super::{PluginsService, configuration::apply_replacements};
use crate::{
    model::{
        AdminError,
        plugins::instances::{
            PluginInstanceMutation, PluginInstanceReplacement, PluginInstanceSnapshot,
        },
    },
    use_case::publish_committed,
};

impl PluginsService {
    /// 以事务返回的版本激活并发布，候选强引用必须覆盖整个提交后阶段
    pub(super) async fn publish_instance(
        &self,
        mut snapshot: PluginInstanceSnapshot,
        prepared: ExtensionSetReference,
        mutation: &PluginInstanceMutation,
        replacements: &[PluginInstanceReplacement],
    ) -> Result<(), AdminError> {
        let prepared = if mutation.config_revision == snapshot.config_revision
            && mutation.instance.revision == snapshot.config_revision
        {
            prepared
        } else {
            snapshot.config_revision = mutation.config_revision;
            snapshot
                .instances
                .retain(|item| item.id != mutation.instance.id);
            snapshot.instances.push(mutation.instance.clone());
            apply_replacements(&mut snapshot, replacements);
            self.preparation.prepare(snapshot).await?
        };
        self.preparation
            .activate_state(&prepared, &mutation.instance)
            .await?;
        publish_committed(self.snapshots.as_ref(), mutation.config_revision).await?;
        drop(prepared);
        Ok(())
    }
}
