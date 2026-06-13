#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema,
}
