# Codex Proxy Plugin CLI

`cpr-plugin` 校验并规范化作者清单、收集声明的资源，生成可安装归档及 SHA-256 校验文件。
输出合同与 [SDK 版本](../../crates/gateway-plugin/sdk/README.md)一致

## 安装

从仓库根目录安装到本地工具目录：

```bash
cargo +1.97.0 install --locked --path backend/apps/plugin-cli --root /path/to/local-tools
```

将 `/path/to/local-tools/bin` 加入 `PATH`，或使用可执行文件的完整路径；工具链与宿主 [验证命令](../../../docs/architecture.md#验证命令)一致

## 打包

先构建目标平台的插件二进制；插件提供管理页面时，还需构建页面静态资源。打包命令传入作者清单、二进制和所需的资源目录映射，通过 `cpr-plugin package --help` 查看完整参数：

```bash
cpr-plugin package \
  --manifest /path/to/plugin/plugin.json \
  --binary /path/to/plugin/target/x86_64-unknown-linux-gnu/release/plugin \
  --target x86_64-unknown-linux-gnu \
  --resource-map web=web/dist \
  --output-dir /path/to/output
```

资源映射相对于作者清单所在目录解析。输出包括完整插件 ID、版本和目标 triple 命名的 `.tar.gz` 及 `.sha256`；归档根目录直接包含 `plugin.json`、可执行文件和已声明资源，不包含开发依赖

作者清单可省略普通贡献项的 `id`、`version` 和固定阶段；`middleware` 必须显式声明版本 `4`，
并从 `http`、`websocket`、`service`、`request`、`attempt` 中选择挂载阶段；`upstream_adapter` 必须显式声明版本 `2`。
CLI 与 SDK 的 `Manifest::from_author_slice`、`PluginBuilder::from_json` 共用规范化入口；归档中的
`plugin.json` 写入补全后的贡献项字段，不保留另一份作者声明。
完整规则见 [SDK 清单](../../crates/gateway-plugin/sdk/docs/manifest.md)

支持 `x86_64-unknown-linux-gnu`、`aarch64-unknown-linux-gnu` 和 `aarch64-apple-darwin`。
完整开发示例在独立仓库 `codex-proxy-plugins` 的 `examples/workbench`，可通过[子模块](../../../docs/development.md#源码联调)检出；本工具不负责源码构建、上传或启用插件

### 图标资源

在作者清单的 `icon` 中指定图标，并在 `resources` 中登记对应路径与 MIME 类型，配置方式见
[SDK 插件图标](../../crates/gateway-plugin/sdk/docs/manifest.md#插件图标)。图标与其他声明资源一样参与打包和摘要计算，
无需额外命令参数；工具保留文件原始内容，宿主安装时验证图标内容与资源限制

## 开发

Rust 包名为 `codex-proxy-plugin-cli`，位于宿主的 `backend` workspace，共享工具链、锁文件和检查规则；内部仅依赖 `gateway-plugin-sdk`

```bash
cargo +1.97.0 run --manifest-path backend/Cargo.toml -p codex-proxy-plugin-cli --locked -- package --help
RUST_MIN_STACK=16777216 cargo +1.97.0 test --manifest-path backend/Cargo.toml -p codex-proxy-plugin-cli --locked
```
