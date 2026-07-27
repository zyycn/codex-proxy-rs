use std::fmt;

use zeroize::Zeroizing;

/// 内存中的 secret；debug 输出恒为脱敏，缓冲区在 drop 时清零。
#[derive(Clone)]
pub struct SecretValue(Zeroizing<String>);

impl SecretValue {
    /// 包装持有所有权的 secret 值。
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    /// 仅在明确的协议或 transport 边界暴露 secret。
    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    pub fn constant_time_eq(&self, other: &Self) -> bool {
        let left = self.0.as_bytes();
        let right = other.0.as_bytes();
        if left.len() != right.len() {
            return false;
        }

        left.iter()
            .zip(right)
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

impl From<String> for SecretValue {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}
