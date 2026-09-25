use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use semver::{Version, VersionReq};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;

use crate::{Capability, Contributions, PROTOCOL_VERSION, Permission, Stage};

/// 当前插件清单格式版本。
pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeKind {
    TrustedProcess,
}

/// 插件展示图标；路径始终引用同一清单中声明的包资源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PluginIcon {
    Path(String),
    Themed(PluginIconVariants),
}

/// 为浅色与深色界面分别声明的插件图标。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginIconVariants {
    pub light: String,
    pub dark: String,
}

/// 宿主版本兼容范围。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Engines {
    #[serde(rename = "codex-proxy-rs")]
    pub codex_proxy_rs: VersionReq,
}

/// 安装包的唯一目标平台。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageTarget {
    pub os: String,
    pub architecture: String,
}

/// 构建生成的安装包元数据；作者源清单可以省略。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Package {
    pub protocol_version: u32,
    pub target: PackageTarget,
    pub files: BTreeMap<String, String>,
}

/// 插件私有状态的单个命名空间声明；宿主仍会施加更严格的全局上限。
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StateNamespace {
    pub namespace: String,
    pub schema_version: u32,
    pub schema: serde_json::Value,
    pub maximum_records: u32,
    pub maximum_bytes: u64,
    pub maximum_value_bytes: u32,
    #[serde(default)]
    pub migrates_from: Vec<u32>,
}

impl fmt::Debug for StateNamespace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StateNamespace")
            .field("namespace", &self.namespace)
            .field("schema_version", &self.schema_version)
            .field("schema", &"[REDACTED]")
            .field("maximum_records", &self.maximum_records)
            .field("maximum_bytes", &self.maximum_bytes)
            .field("maximum_value_bytes", &self.maximum_value_bytes)
            .field("migrates_from", &self.migrates_from)
            .finish()
    }
}

/// 插件作者清单。`package` 缺省时表示尚未构建的源清单。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub manifest_version: u32,
    pub name: String,
    pub display_name: String,
    pub publisher: String,
    pub version: Version,
    pub description: String,
    pub license: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub engines: Engines,
    pub main: String,
    pub runtime: RuntimeKind,
    #[serde(deserialize_with = "crate::capability::deserialize_contributions")]
    pub contributes: Contributions,
    #[serde(default)]
    pub permissions: BTreeSet<Permission>,
    #[serde(default = "empty_configuration_schema")]
    pub configuration_schema: serde_json::Value,
    #[serde(default)]
    pub secret_fields: BTreeSet<String>,
    #[serde(default)]
    pub resources: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<PluginIcon>,
    #[serde(default)]
    pub state: Vec<StateNamespace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<Package>,
}

fn empty_configuration_schema() -> serde_json::Value {
    serde_json::json!({})
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("plugin manifest is invalid")]
    Invalid,
    #[error("plugin protocol or manifest version is incompatible")]
    Incompatible,
    #[error("plugin does not support the host version or platform")]
    Platform,
}

impl Manifest {
    /// 读取作者清单并生成与 CLI、运行注册一致的完整声明。
    ///
    /// # Errors
    ///
    /// JSON、版本、能力声明或安装包字段不符合作者合同时返回错误。
    pub fn from_author_slice(bytes: &[u8]) -> Result<Self, ManifestError> {
        let UniqueValue(source) =
            serde_json::from_slice(bytes).map_err(|_| ManifestError::Invalid)?;
        let mut manifest: Self =
            serde_json::from_value(source).map_err(|_| ManifestError::Invalid)?;
        manifest.normalize_author()?;
        Ok(manifest)
    }

    /// 作者清单允许省略扩展项 ID、能力版本和固定阶段，安装清单不隐式补全。
    ///
    /// # Errors
    ///
    /// 已含构建元数据、阶段与固定合同冲突或规范化后清单无效时返回错误。
    pub fn normalize_author(&mut self) -> Result<(), ManifestError> {
        if self.package.is_some() {
            return Err(ManifestError::Invalid);
        }
        let plugin_id = self.plugin_id()?;
        for (capability, declaration) in &mut self.contributes {
            if declaration.id.is_empty() {
                declaration.id =
                    format!("{plugin_id}.{}", capability.identifier().replace('_', "-"));
            }
            if *capability != Capability::Middleware {
                if !declaration.stages.is_empty() && declaration.stages != capability.fixed_stages()
                {
                    return Err(ManifestError::Invalid);
                }
                declaration.stages = capability.fixed_stages().to_vec();
            }
        }
        self.validate()
    }

    /// 派生并校验稳定插件 ID。
    ///
    /// # Errors
    ///
    /// 发布者、机器短名或派生后的完整 ID 不符合命名边界时返回错误。
    pub fn plugin_id(&self) -> Result<String, ManifestError> {
        if !valid_plugin_segment(&self.publisher) || !valid_plugin_segment(&self.name) {
            return Err(ManifestError::Invalid);
        }
        let plugin_id = format!("{}.{}", self.publisher, self.name);
        if plugin_id.len() > 64 {
            return Err(ManifestError::Invalid);
        }
        Ok(plugin_id)
    }

    /// 校验源清单，并在 `package` 存在时同时执行严格安装包结构校验。
    ///
    /// # Errors
    ///
    /// 清单版本、身份、声明、路径、摘要、状态或安装包元数据不符合合同时返回错误。
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.manifest_version != MANIFEST_VERSION {
            return Err(ManifestError::Incompatible);
        }
        let plugin_id = self.plugin_id()?;
        if [&self.display_name, &self.description, &self.license]
            .iter()
            .any(|text| !valid_text(text))
            || self.author.as_ref().is_some_and(|text| !valid_text(text))
            || !valid_package_path(&self.main)
            || self.main == "plugin.json"
            || self.contributes.len() > 64
            || self.resources.len() > 128
            || self.secret_fields.len() > 64
            || self.state.len() > 16
        {
            return Err(ManifestError::Invalid);
        }
        self.validate_contributions(&plugin_id)?;
        self.validate_resources()?;
        self.validate_icon()?;
        self.validate_state()?;
        if let Some(package) = &self.package {
            self.validate_package(package)?;
        }
        Ok(())
    }

    /// 返回与当前宿主及平台匹配的安装包元数据。
    ///
    /// # Errors
    ///
    /// 源清单尚无安装包、清单无效、宿主版本不匹配或目标平台不同时返回错误。
    pub fn package_for(
        &self,
        host: &Version,
        os: &str,
        architecture: &str,
    ) -> Result<&Package, ManifestError> {
        self.validate()?;
        let package = self.package.as_ref().ok_or(ManifestError::Invalid)?;
        if !self.engines.codex_proxy_rs.matches(host)
            || package.target.os != os
            || package.target.architecture != architecture
        {
            return Err(ManifestError::Platform);
        }
        Ok(package)
    }

    fn validate_contributions(&self, plugin_id: &str) -> Result<(), ManifestError> {
        let mut contribution_ids = BTreeSet::new();
        let prefix = format!("{plugin_id}.");
        for (capability, declaration) in &self.contributes {
            let Some(local_id) = declaration.id.strip_prefix(&prefix) else {
                return Err(ManifestError::Invalid);
            };
            let stages = declaration.stages.iter().copied().collect::<BTreeSet<_>>();
            let input_formats = declaration.input_formats.iter().collect::<BTreeSet<_>>();
            let output_formats = declaration.output_formats.iter().collect::<BTreeSet<_>>();
            if !(declaration.version == 1
                || (*capability == Capability::Middleware && declaration.version == 2))
                || declaration.id.len() > 128
                || local_id.is_empty()
                || !local_id.is_ascii()
                || !local_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
                || !contribution_ids.insert(&declaration.id)
                || declaration.stages.len() > 32
                || stages.len() != declaration.stages.len()
                || input_formats.len() != declaration.input_formats.len()
                || output_formats.len() != declaration.output_formats.len()
                || declaration
                    .input_formats
                    .iter()
                    .chain(&declaration.output_formats)
                    .any(|format| !platform_component(format))
                || (*capability == Capability::Middleware
                    && (declaration.stages.is_empty()
                        || declaration
                            .stages
                            .iter()
                            .any(|stage| !matches!(stage, Stage::Request | Stage::Attempt))))
                || (*capability != Capability::Middleware
                    && declaration.stages != capability.fixed_stages())
            {
                return Err(ManifestError::Invalid);
            }
        }
        Ok(())
    }

    fn validate_resources(&self) -> Result<(), ManifestError> {
        for (path, content_type) in &self.resources {
            if !valid_package_path(path)
                || path == "plugin.json"
                || content_type.is_empty()
                || content_type.len() > 128
                || !content_type.is_ascii()
                || content_type.chars().any(char::is_control)
            {
                return Err(ManifestError::Invalid);
            }
        }
        if self
            .secret_fields
            .iter()
            .any(|field| !platform_component(field))
        {
            return Err(ManifestError::Invalid);
        }
        Ok(())
    }

    fn validate_icon(&self) -> Result<(), ManifestError> {
        let Some(icon) = &self.icon else {
            return Ok(());
        };
        let valid = |path: &str| {
            let extension = path.rsplit('.').next().unwrap_or_default();
            let content_types: &[&str] = match extension.to_ascii_lowercase().as_str() {
                "svg" => &["image/svg+xml"],
                "png" => &["image/png"],
                "jpg" | "jpeg" | "jfif" => &["image/jpeg"],
                "webp" => &["image/webp"],
                "gif" => &["image/gif"],
                "ico" => &["image/vnd.microsoft.icon", "image/x-icon"],
                "bmp" => &["image/bmp", "image/x-ms-bmp"],
                _ => &[],
            };
            valid_package_path(path)
                && self
                    .resources
                    .get(path)
                    .is_some_and(|actual| content_types.contains(&actual.as_str()))
        };
        let valid = match icon {
            PluginIcon::Path(path) => valid(path),
            PluginIcon::Themed(variants) => valid(&variants.light) && valid(&variants.dark),
        };
        valid.then_some(()).ok_or(ManifestError::Invalid)
    }

    fn validate_state(&self) -> Result<(), ManifestError> {
        let mut state_namespaces = BTreeSet::new();
        for state in &self.state {
            let schema_bytes =
                serde_json::to_vec(&state.schema).map_err(|_| ManifestError::Invalid)?;
            let migrations = state.migrates_from.iter().copied().collect::<BTreeSet<_>>();
            if !valid_namespace(&state.namespace)
                || !state_namespaces.insert(&state.namespace)
                || state.schema_version == 0
                || !state.schema.is_object()
                || schema_bytes.len() > 32 * 1024
                || state.maximum_records == 0
                || state.maximum_records > 10_000
                || state.maximum_bytes == 0
                || state.maximum_bytes > 16 * 1024 * 1024
                || state.maximum_value_bytes == 0
                || state.maximum_value_bytes > 256 * 1024
                || u64::from(state.maximum_value_bytes) > state.maximum_bytes
                || migrations.len() != state.migrates_from.len()
                || migrations.contains(&0)
                || migrations.contains(&state.schema_version)
            {
                return Err(ManifestError::Invalid);
            }
        }
        Ok(())
    }

    fn validate_package(&self, package: &Package) -> Result<(), ManifestError> {
        if package.protocol_version != PROTOCOL_VERSION {
            return Err(ManifestError::Incompatible);
        }
        // semver 将全版本 `*` 表示为空 comparator；`1.*` 仍有明确的主版本边界。
        if self.engines.codex_proxy_rs.comparators.is_empty()
            || !platform_component(&package.target.os)
            || !platform_component(&package.target.architecture)
            || package.files.is_empty()
            || package.files.len() > 256
            || !package.files.contains_key(&self.main)
            || self
                .resources
                .keys()
                .any(|path| !package.files.contains_key(path))
        {
            return Err(ManifestError::Invalid);
        }
        for (path, digest) in &package.files {
            if !valid_package_path(path)
                || path == "plugin.json"
                || digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(ManifestError::Invalid);
            }
            // 文件不能同时充当父目录，避免跨平台路径覆盖与解压顺序歧义。
            if path
                .split('/')
                .scan(String::new(), |parent, part| {
                    if !parent.is_empty() {
                        parent.push('/');
                    }
                    parent.push_str(part);
                    Some(parent.clone())
                })
                .any(|parent| parent != *path && package.files.contains_key(&parent))
            {
                return Err(ManifestError::Invalid);
            }
        }
        Ok(())
    }
}

/// `serde_json::Value` 默认保留重复对象键的最后一个值；作者清单不能让同一声明因解析入口而改变含义。
struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueValueVisitor)
    }
}

struct UniqueValueVisitor;

impl<'de> Visitor<'de> for UniqueValueVisitor {
    type Value = UniqueValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .map(UniqueValue)
            .ok_or_else(|| E::custom("JSON number is not finite"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        UniqueValue::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(UniqueValue(value)) = sequence.next_element()? {
            values.push(value);
        }
        Ok(UniqueValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut entries: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = serde_json::Map::new();
        while let Some((key, UniqueValue(value))) = entries.next_entry()? {
            if values.insert(key, value).is_some() {
                return Err(de::Error::custom("duplicate JSON object key"));
            }
        }
        Ok(UniqueValue(Value::Object(values)))
    }
}

fn valid_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
}

fn valid_plugin_segment(value: &str) -> bool {
    let Some(first) = value.as_bytes().first() else {
        return false;
    };
    let Some(last) = value.as_bytes().last() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && last.is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_namespace(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.".contains(&byte)
        })
}

fn platform_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

/// 逐个检查原始路径段，不能让操作系统先规范化掉 `.`、空段或反斜线。
#[must_use]
pub fn valid_package_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 240
        && path.is_ascii()
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with(['.', ' '])
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
                && !matches!(
                    part.split('.')
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase()
                        .as_str(),
                    "CON"
                        | "PRN"
                        | "AUX"
                        | "NUL"
                        | "COM1"
                        | "COM2"
                        | "COM3"
                        | "COM4"
                        | "COM5"
                        | "COM6"
                        | "COM7"
                        | "COM8"
                        | "COM9"
                        | "LPT1"
                        | "LPT2"
                        | "LPT3"
                        | "LPT4"
                        | "LPT5"
                        | "LPT6"
                        | "LPT7"
                        | "LPT8"
                        | "LPT9"
                )
        })
}
