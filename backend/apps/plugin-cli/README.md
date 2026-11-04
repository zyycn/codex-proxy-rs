# Codex Proxy Plugin CLI

`cpr-plugin` 校验并规范化作者清单、收集声明的资源，生成可安装归档及 SHA-256 校验文件。
输出合同与 [SDK 版本](../../crates/gateway-plugin/sdk/README.md)一致

## 安装

从仓库根目录安装到本地工具目录：

```bash
cargo +1.99.1 install --locked --path backend/apps/plugin-cli --root /path/to/local-tools
```

将 `/path/to/local-tools/bin` 加入 `PATH`，或使用可执行文件的完整路径；工具链与宿主 [验证命令](../../../docs/architecture.md#验证命令)一致

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

`--resource-map web=web/dist` 将包内 `web/` 前缀映射到作者清单所在目录下的 `web/dist/`。
可重复传入不同前缀，源目录必须位于项目内；没有对应资源时省略该参数，未使用的映射会被拒绝。
未映射的资源按清单中的相对路径读取，只收集 `resources` 声明的文件

输出为 `<插件 ID>-<版本>-<目标 triple>.tar.gz` 及同名 `.tar.gz.sha256`；归档根目录直接包含 `plugin.json`、可执行文件和声明资源。
输入清单及生成后的清单均不得超过 64 KiB，压缩包上限为 32 MiB，展开归档上限为 128 MiB

作者清单不填写 `package`；字段省略规则、能力版本和挂载阶段见 [SDK 扩展项声明](../../crates/gateway-plugin/sdk/docs/manifest.md#扩展项简写)。
CLI 生成规范化后的 `plugin.json`，补入协议版本、目标平台和文件摘要。打包成功只证明输入符合包合同，实际启动与能力仍需在目标宿主验证

支持 `x86_64-unknown-linux-gnu`、`aarch64-unknown-linux-gnu` 和 `aarch64-apple-darwin`。
完整开发示例在独立仓库 `codex-proxy-plugins` 的 `examples/workbench`，可通过[子模块](../../../docs/development.md#源码联调)检出；本工具不负责源码构建、上传或启用插件

### 图标资源

在作者清单的 `icon` 中指定图标，并在 `resources` 中登记对应路径与 MIME 类型，配置方式见
[SDK 插件图标](../../crates/gateway-plugin/sdk/docs/manifest.md#插件图标)。图标与其他声明资源一样参与打包和摘要计算，
无需额外命令参数；工具保留文件原始内容，宿主安装时验证图标内容与资源限制

## 开发

Rust 包名为 `codex-proxy-plugin-cli`，位于宿主的 `backend` workspace，共享工具链、锁文件和检查规则；内部仅依赖 `gateway-plugin-sdk`

```bash
cargo +1.99.1 run --manifest-path backend/Cargo.toml -p codex-proxy-plugin-cli --locked -- package --help
RUST_MIN_STACK=16777216 cargo +1.99.1 test --manifest-path backend/Cargo.toml -p codex-proxy-plugin-cli --locked
```
