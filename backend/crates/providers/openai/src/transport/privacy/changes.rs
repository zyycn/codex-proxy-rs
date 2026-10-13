//! 暂存单条规则改动的正文片段，校验最终大小后统一提交

use serde_json::Value;

use super::{Budget, MAX_REQUEST_BYTES, encoded_size};

pub(super) struct BodyChanges {
    original_size: usize,
    replacements: Vec<(String, Value)>,
    removed_bytes: usize,
    added_bytes: usize,
}

impl BodyChanges {
    pub(super) fn new(original_size: usize) -> Self {
        Self {
            original_size,
            replacements: Vec::new(),
            removed_bytes: 0,
            added_bytes: 0,
        }
    }

    pub(super) fn stage(
        &mut self,
        body: &Value,
        path: &str,
        budget: &mut Budget<'_>,
        apply: impl FnOnce(&mut Value, &mut Budget<'_>) -> Result<usize, &'static str>,
    ) -> Result<usize, &'static str> {
        let original = body.pointer(path).ok_or("目标正文片段不存在")?;
        let mut next = original.clone();
        let count = apply(&mut next, budget)?;
        if count > 0 {
            self.removed_bytes += if path.is_empty() {
                self.original_size
            } else {
                encoded_size(original, MAX_REQUEST_BYTES)?
            };
            // 片段互不重叠，其编码字节总量是最终正文大小的下界
            // 提前停止必然超限的改写，避免暂存所有膨胀后的上下文
            self.added_bytes = self
                .added_bytes
                .checked_add(encoded_size(&next, MAX_REQUEST_BYTES)?)
                .filter(|size| *size <= MAX_REQUEST_BYTES)
                .ok_or("请求或预览样本超出大小限制")?;
            self.replacements.push((path.to_owned(), next));
        }
        Ok(count)
    }

    pub(super) fn size(&self) -> Result<usize, &'static str> {
        self.original_size
            .checked_sub(self.removed_bytes)
            .and_then(|size| size.checked_add(self.added_bytes))
            .filter(|size| *size <= MAX_REQUEST_BYTES)
            .ok_or("请求或预览样本超出大小限制")
    }

    pub(super) fn commit(self, body: &mut Value) {
        // 路径来自本条规则执行前的正文，片段互不包含且提交前正文尚未改写
        // 仅替换值，不增删这些路径的祖先，因此先前替换不会使后续路径失效
        for (path, next) in self.replacements {
            if let Some(target) = body.pointer_mut(&path) {
                *target = next;
            }
        }
    }
}
