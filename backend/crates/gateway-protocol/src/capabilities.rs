//! 协议解码与请求聚合共同使用的能力事实，不依赖协议分派

/// 请求正文表达的能力要求
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestFacts {
    /// 显式输出预算
    pub max_output_tokens: Option<u64>,
    /// 工具声明
    pub tools: bool,
    /// 图片输入
    pub vision: bool,
    /// 图片生成工具声明
    pub image_generation: bool,
    /// 推理配置
    pub reasoning: bool,
    /// JSON Schema 输出
    pub json_schema: bool,
    /// 原生服务端续接
    pub native_continuation: bool,
}
