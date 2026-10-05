//! 插件嵌套调用的递归作用域

use std::{collections::BTreeSet, sync::Arc};

/// 一次请求已经进入过的插件实例集合
///
/// Core 把它随子请求传播给策略与观察边界；Runtime 只据此跳过对应
/// 实例，不能自行扩大授权或改变路由
/// 集合同时用于拒绝 A → B → A 间接递归
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionCallScope {
    instance_ids: Arc<BTreeSet<String>>,
}

impl ExtensionCallScope {
    pub const MAXIMUM_DEPTH: usize = 4;

    #[must_use]
    pub fn contains(&self, instance_id: &str) -> bool {
        self.instance_ids.contains(instance_id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.instance_ids.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.instance_ids.is_empty()
    }

    /// 返回加入当前发起实例后的新作用域；重复实例表示间接递归
    #[must_use]
    pub fn extending(&self, instance_id: String) -> Option<Self> {
        if self.contains(&instance_id) {
            return None;
        }
        let mut instance_ids = self.instance_ids.as_ref().clone();
        instance_ids.insert(instance_id);
        Some(Self {
            instance_ids: Arc::new(instance_ids),
        })
    }
}
