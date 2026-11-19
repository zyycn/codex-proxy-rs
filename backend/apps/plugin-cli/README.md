# Codex Proxy Plugin CLI

`cpr-plugin` 按 [SDK 合同](../../crates/gateway-plugin/sdk/README.md)校验清单，将插件可执行文件与声明的资源打包，并输出 SHA-256 校验文件

## 安装

从仓库根目录安装到本地工具目录：

```bash
cargo +1.99.1 install --locked --path backend/apps/plugin-cli --root /path/to/local-tools
```

将 `/path/to/local-tools/bin` 加入 `PATH`，或使用可执行文件的完整路径

## 打包

先构建目标平台的二进制；有管理页面时，也构建静态资源。全部参数见 `cpr-plugin package --help`

```bash
cpr-plugin package \
  --manifest /path/to/plugin/plugin.json \
  --binary /path/to/plugin/target/x86_64-unknown-linux-gnu/release/plugin \
  --target x86_64-unknown-linux-gnu \
  --resource-map web=web/dist \
  --output-dir /path/to/output
```

资源文件须在 `resources` 中声明，默认从清单所在目录按相对路径读取。
`--resource-map web=web/dist` 从项目内的 `web/dist/` 读取包内 `web/` 资源。可为不同前缀重复传参，未匹配任何资源的映射会报错

输出为 `<插件 ID>-<版本>-<目标 triple>.tar.gz` 及对应的 `.tar.gz.sha256`。归档根目录包含 `plugin.json`、可执行文件和声明资源。
输入清单及生成后的清单均不得超过 64 KiB，压缩包上限为 32 MiB，展开归档上限为 128 MiB

作者清单须省略 `package`。CLI 将协议版本、目标平台和文件摘要写入 `plugin.json`。可省略的声明字段见 [SDK 扩展项声明](../../crates/gateway-plugin/sdk/docs/manifest.md#扩展项简写)

支持 `x86_64-unknown-linux-gnu`、`aarch64-unknown-linux-gnu` 和 `aarch64-apple-darwin`。
打包后在目标宿主验证启动和插件功能。完整示例见插件[子模块](../../../docs/development.md#源码联调)的 `examples/workbench`

### 图标资源

按 [SDK 插件图标](../../crates/gateway-plugin/sdk/docs/manifest.md#插件图标)填写 `icon` 和 `resources`。CLI 自动打包原文件并计算摘要，宿主在安装时校验

## 开发

Rust 包名为 `codex-proxy-plugin-cli`，位于宿主的 `backend` workspace，共享工具链、锁文件和检查规则；内部仅依赖 `gateway-plugin-sdk`

```bash
cargo +1.99.1 run --manifest-path backend/Cargo.toml -p codex-proxy-plugin-cli --locked -- package --help
RUST_MIN_STACK=16777216 cargo +1.99.1 test --manifest-path backend/Cargo.toml -p codex-proxy-plugin-cli --locked
```
