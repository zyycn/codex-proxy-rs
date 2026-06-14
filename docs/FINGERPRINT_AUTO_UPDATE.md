# Codex Desktop 指纹自动更新实现

## 实现日期
2026-06-13

## 背景

Codex Desktop 会定期发布新版本，版本号变化会影响 HTTP 指纹识别。为保持与真实客户端一致，需要实现自动同步机制。

---

## 实现方案

### 核心机制

参考 Node.js 版本实现，采用相同的更新策略：

1. **轮询 Sparkle Appcast** - 定期检查官方更新源
2. **自动解析版本** - 提取 `app_version`、`build_number`
3. **持久化状态** - 写入 `data/version-state.json` 和数据库
4. **运行时生效** - 后台任务自动应用新版本

### 关键参数

| 参数 | 值 | 说明 |
|------|-----|------|
| Appcast URL | `https://persistent.oaistatic.com/codex-app-prod/appcast.xml` | Codex Desktop 官方更新源 |
| 轮询间隔 | 3 天 | 与 Node.js 版本一致 |
| 当前版本 | `26.519.81530` | app_version |
| 当前构建号 | `3178` | build_number |
| Chromium 版本 | `146` | 浏览器内核版本 |

---

## 实现细节

### 1. UpdateChecker 核心结构

```rust
pub struct UpdateChecker {
    db: Option<SqlitePool>,
    state: Arc<Mutex<InternalState>>,
}

impl UpdateChecker {
    pub fn new(db: Option<SqlitePool>, current_version: String, current_build: String) -> Self
    pub async fn check_for_update(&self) -> Result<UpdateState, UpdateError>
    pub fn start_background_checker(self) -> JoinHandle<()>
}
```

**特性：**
- 使用 `Arc<Mutex<>>` 实现内部状态共享
- 返回 `JoinHandle<()>` 支持优雅关闭
- 异步设计，不阻塞主线程

### 2. Appcast XML 解析

支持两种 Sparkle 格式：

**属性语法：**
```xml
<item>
  <sparkle:shortVersionString="26.519.81530"/>
  <sparkle:version="3178"/>
  <enclosure url="https://..." />
</item>
```

**元素语法：**
```xml
<item>
  <sparkle:shortVersionString>26.519.81530</sparkle:shortVersionString>
  <sparkle:version>3178</sparkle:version>
  <enclosure url="https://..." />
</item>
```

### 3. 版本状态持久化

**文件结构：**
```
data/
├── update-state.json       # 最近检查状态
├── version-state.json      # 当前应用版本
└── extracted-fingerprint.json  # 可选：提取的完整指纹
```

**update-state.json 格式：**
```json
{
  "last_check": "2026-06-13T10:30:00Z",
  "latest_version": "26.519.81530",
  "latest_build": "3178",
  "download_url": "https://...",
  "update_available": false,
  "current_version": "26.519.81530",
  "current_build": "3178"
}
```

**version-state.json 格式：**
```json
{
  "app_version": "26.519.81530",
  "build_number": "3178",
  "chromium_version": "146"
}
```

### 4. 数据库持久化

自动更新通过 `FingerprintRepository::upsert_auto_update()` 写入 `fingerprints` 表的 `auto_updated` 当前记录：

```sql
insert into fingerprints (
    id, app_version, build_number, platform, arch,
    chromium_version, user_agent_template, source, created_at
) values ('auto_updated', ?, ?, 'darwin', 'arm64', ?, ?, 'auto_update', ?)
on conflict(id) do update set
    app_version = excluded.app_version,
    build_number = excluded.build_number,
    chromium_version = excluded.chromium_version,
    created_at = excluded.created_at
```

**好处：**
- 启动时可以稳定加载最新 `auto_update` fingerprint
- 自动更新逻辑不再直接持有业务表 SQL
- 历史记录写入仍由 `FingerprintRepository::insert_update()` 支持

### 5. Chromium 版本匹配

如果存在 `extracted-fingerprint.json`（手动提取的完整指纹）：

```rust
fn load_matching_chromium_version(version: &str, build: &str) -> Option<String> {
    // 读取 extracted-fingerprint.json
    // 匹配 app_version 和 build_number
    // 返回对应的 chromium_version
}
```

**场景：**
- 自动更新时，先检查本地是否有匹配的提取指纹
- 如果有，使用提取的 `chromium_version`（更准确）
- 否则，保持默认值 `"146"`

---

## 集成到后台任务

### coordinator.rs 修改

```rust
pub async fn start_background_tasks(
    state: &AppState,
    db_pool: SqlitePool,
    config: &AppConfig,
) -> BackgroundTaskCoordinator {
    let mut coordinator = BackgroundTaskCoordinator::default();

    // 指纹自动更新（3 天轮询一次）
    let fingerprint = Fingerprint::default_codex_desktop();
    let update_checker = UpdateChecker::new(
        Some(db_pool.clone()),
        fingerprint.app_version.clone(),
        fingerprint.build_number.clone(),
    );
    let update_handle = update_checker.start_background_checker();
    coordinator.push("fingerprint_update", SchedulerHandle::from_join_handle(update_handle));
    tracing::info!("fingerprint update checker started");

    // ... 其他后台任务
}
```

**启动流程：**
1. 应用启动时自动创建 `UpdateChecker`
2. 立即执行首次检查（非阻塞）
3. 后台循环每 3 天检查一次
4. 发现新版本自动应用

---

## 工作流程

```
启动应用
   ↓
创建 UpdateChecker
   ↓
立即首次检查 (async)
   ↓
[每 3 天循环]
   ↓
fetch appcast.xml
   ↓
解析版本信息
   ↓
对比当前版本
   ↓
  是否有更新？
   ├─ 否 → 继续等待
   └─ 是 → 应用更新
          ├─ 写入 version-state.json
          ├─ 更新数据库 fingerprints 表
          ├─ 更新内部状态
          └─ 记录日志
```

---

## 日志输出

**启动时：**
```
[UpdateChecker] 启动后台指纹版本检查器，间隔：3d
[UpdateChecker] 首次检查失败: HTTP 请求失败: ... (可选)
```

**发现更新：**
```
[UpdateChecker] *** 发现新版本: v26.600.12345 (build 3200) — 当前: v26.519.81530 (build 3178)
[UpdateChecker] 已自动应用: v26.600.12345 (build 3200)
```

**定期检查：**
```
[UpdateChecker] 定期检查失败: 获取 appcast 失败，状态码: 503 (可选)
```

---

## 错误处理

### UpdateError 类型

```rust
pub enum UpdateError {
    Http(reqwest::Error),           // 网络请求失败
    AppcastFetch(u16),               // HTTP 非 200 响应
    AppcastParse,                    // XML 解析失败
    Json(serde_json::Error),         // JSON 序列化失败
    Io(std::io::Error),              // 文件操作失败
    Database(sqlx::Error),           // 数据库操作失败
}
```

### 容错策略

1. **网络失败** - 记录警告日志，等待下次轮询
2. **解析失败** - 记录警告日志，不影响现有版本
3. **文件写入失败** - 尽力而为（best-effort），不阻塞应用
4. **数据库失败** - 记录错误，但版本状态文件仍保留

**设计原则：**
- 更新失败不影响核心服务运行
- 所有错误仅记录日志，不抛出异常
- 下次轮询会自动重试

---

## 相关文件

1. **src/codex/gateway/fingerprint/update_checker.rs**
   - UpdateChecker 结构体
   - Appcast 解析逻辑
   - 状态持久化
   - 通过 `FingerprintRepository` 写入数据库

2. **src/codex/gateway/fingerprint/repository.rs**
   - fingerprint 历史记录写入
   - `auto_updated` 当前记录 upsert
   - 启动时加载 `auto_update` fingerprint

3. **src/platform/storage/paths.rs**
   - 数据目录路径管理
   - `data_dir()` 和 `ensure_data_dir()`

4. **修改文件：**
   - `src/codex/gateway/fingerprint/mod.rs` - 添加模块导出
   - `src/platform/storage/mod.rs` - 添加 paths 模块
   - `src/runtime/tasks/coordinator.rs` - 启动 update checker
   - `src/runtime/tasks/types.rs` - 支持 JoinHandle

---

## 与 Node.js 对比

| 特性 | Node.js | Rust | 状态 |
|------|---------|------|------|
| **Appcast URL** | `https://persistent.oaistatic.com/codex-app-prod/appcast.xml` | 同左 | ✅ 一致 |
| **轮询间隔** | 3 天 | 3 天 | ✅ 一致 |
| **XML 解析** | 支持属性/元素两种语法 | 支持属性/元素两种语法 | ✅ 一致 |
| **版本状态文件** | `data/version-state.json` | 同左 | ✅ 一致 |
| **检查状态文件** | `data/update-state.json` | 同左 | ✅ 一致 |
| **立即首次检查** | ✅ 非阻塞 | ✅ 非阻塞 | ✅ 一致 |
| **自动应用更新** | ✅ | ✅ | ✅ 一致 |
| **数据库持久化** | ❌ | ✅ | 🟢 Rust 额外功能 |
| **完整指纹提取** | ✅ (full-update.ts) | 🟡 仅匹配逻辑 | 🟡 部分实现 |

**差异说明：**
- Node.js 有 `full-update.ts` 脚本下载 Codex.app 并提取完整指纹
- Rust 版本暂不实现下载功能（复杂度高，收益低）
- Rust 版本保留匹配逻辑，支持手动提取的指纹

---

## 测试验证

### ✅ 编译验证
```bash
cargo build
# Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.29s
```

### ✅ 测试验证
```
总计：217 tests passed
```

### 手动验证步骤

1. **检查启动日志：**
   ```bash
   cargo run
   # 观察是否有 "[UpdateChecker] 启动后台指纹版本检查器" 日志
   ```

2. **模拟版本更新：**
   - 修改 `Fingerprint::default_codex_desktop()` 的版本号为旧版本
   - 重启应用
   - 观察是否自动检测到新版本

3. **验证持久化：**
   ```bash
   cat ~/.local/share/codex-proxy-rs/data/update-state.json
   cat ~/.local/share/codex-proxy-rs/data/version-state.json
   ```

4. **验证数据库：**
   ```sql
   select * from fingerprints where id = 'auto_updated';
   ```

---

## 安全考虑

### 1. 网络安全
- ✅ 使用 HTTPS（`https://persistent.oaistatic.com`）
- ✅ 30 秒请求超时
- ✅ reqwest 默认验证 TLS 证书

### 2. XML 解析安全
- ✅ 手动解析，不使用 XML 库（避免 XXE 攻击）
- ✅ 仅提取必要字段（version, build, url）
- ✅ 严格验证提取结果

### 3. 文件系统安全
- ✅ 使用 `dirs::data_local_dir()` 标准目录
- ✅ 自动创建父目录（`ensure_data_dir`）
- ✅ 权限由操作系统管理

### 4. 并发安全
- ✅ 使用 `Arc<Mutex<>>` 保护内部状态
- ✅ 单线程后台任务，无竞态条件
- ✅ 文件写入使用原子操作

---

## 维护指南

### 更新轮询间隔

修改 `POLL_INTERVAL` 常量：

```rust
const POLL_INTERVAL: Duration = Duration::from_secs(1 * 24 * 60 * 60); // 1 天
```

### 禁用自动更新

在 `coordinator.rs` 中注释相关代码：

```rust
// let update_checker = UpdateChecker::new(...);
// coordinator.push("fingerprint_update", ...);
```

### 手动触发检查

```rust
let checker = UpdateChecker::new(Some(db), version, build);
let state = checker.check_for_update().await?;
if state.update_available {
    println!("发现新版本: {}", state.latest_version.unwrap());
}
```

---

## 已知限制

1. **不下载 Codex.app**
   - Node.js 版本会下载完整应用并提取指纹
   - Rust 版本仅从 appcast 获取版本号
   - 影响：无法自动获取 `chromium_version`

2. **依赖外部提取**
   - 如需完整指纹，需手动提取并放置到 `data/extracted-fingerprint.json`
   - 或保持默认 `chromium_version = "146"`

3. **无 UI 通知**
   - 版本更新静默进行
   - 仅通过日志通知

这些限制是有意的设计选择，优先保证核心功能的简洁性和可靠性。

---

## 总结

✅ **已实现 Codex Desktop 指纹自动更新功能**

核心特性：
- ✅ 每 3 天自动检查官方更新源
- ✅ 自动解析和应用新版本
- ✅ 持久化到文件和数据库
- ✅ 非阻塞后台任务
- ✅ 完善的错误处理和日志

安全保证：
- ✅ HTTPS 通信
- ✅ 安全的 XML 解析
- ✅ 并发安全设计
- ✅ 失败不影响核心服务

与 Node.js 参考实现对齐：
- ✅ 相同的更新源和轮询间隔
- ✅ 相同的文件格式和目录结构
- ✅ 相同的版本应用逻辑

**指纹安全基石已构建完成，可投入生产使用。** 🚀
