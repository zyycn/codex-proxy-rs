# v3.54.2

## 问题修复

- 「请求国家」策略仅对 Codex OAuth 账号生效，OpenAI API Key 请求保留客户端的 `request_country` 原值，省略时也保持省略

## 优化改进

- 尚未识别为 CPR 互联的 OpenAI API Key 账号可在「更多操作」中选择「检测 CPR 互联」，识别成功后可在额度卡片中刷新额度
