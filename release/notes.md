# v3.7.2

## 问题修复

- 修复 OpenAI 账号在同一额度窗口内用量回落后仍保持耗尽、无法参与调度的问题。连续两次额度观测确认相关窗口未触顶后可恢复，无需一直等待原重置日期；保留进入新窗口后的原有恢复路径。
- `openai.wire_profile.location` 支持留空、设为 `null` 或省略。未配置时透传客户端原有的 Web Search 地区、环境日期和时区，也不会补写客户端未提供的字段；完整填写时继续按配置覆盖。

## 发布与部署

- Release 附件新增同版本的 `config.example.yaml` 和 `compose.yaml`，并纳入 `checksums.txt` 校验。各平台归档同时包含 `deploy/config.example.yaml`。
- 发布附件中的 Compose 默认固定到该版本镜像，快速开始从同一 Release 下载部署文件，避免混用 `main` 分支配置与已发布镜像。
- 发布说明中的安装、下载与文档说明统一使用中文，部署文档链接固定到对应版本。
- 发版时将版本号与发布说明合并为一次提交，并在该提交上创建 tag。

## 升级说明

- 从 v3.7.1 升级无需新增数据库迁移，已有完整的 `location` 配置继续生效。
- 省略 `location` 时，行为由默认覆盖为 `US / Ohio / Piketon / America/New_York` 改为透传。需要保留原覆盖行为的部署应显式填写这四个字段；使用 `location: null` 需要升级到 v3.7.2。
- 升级前对比本版本配置模板并合并必要配置，保留已有凭据和 Compose 自定义项，不要用模板覆盖 `config.yaml`。使用二进制归档手动部署时，将 `api.asset_directory` 设为 `../web/dist`。
