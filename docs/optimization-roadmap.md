# 🚀 codex-proxy-rs 优化路线图

基于 Rust 社区最佳实践审查，按优先级排序的优化计划。

---

## ✅ 已完成优化

### 1. 迁移 serde_yaml → serde_yml ✅
- **状态：** 已完成
- **提交：** 8389a40
- **时间：** 15 分钟
- **收益：** 消除技术债务，使用维护中的库
- **风险：** 低
- **测试：** 142 个测试全部通过

---

## 📋 待优化项目

### 优先级 1：性能优化（高 ROI，低风险）

#### 2. Arc<AppConfig> 包裹配置 🟡
- **问题：** 每次 `.clone()` Service 都深拷贝整个 AppConfig
- **影响：** 热路径（每个请求都克隆 Service）
- **改进：**
  ```rust
  // 当前
  pub struct AccountService {
      config: AppConfig,  // ❌ 完整拷贝
  }

  // 改进
  pub struct AccountService {
      config: Arc<AppConfig>,  // ✅ 廉价克隆
  }
  ```
- **影响范围：** 所有 Service 结构体
- **时间估计：** 30-60 分钟
- **收益：** 减少内存分配，提升请求处理性能
- **风险：** 低（只改字段类型）

#### 3. 参数改用 &str 而非 String 🟡
- **问题：** 276 处字符串分配，92 处在 codex/ 模块
- **改进：**
  ```rust
  // 当前
  pub fn some_method(&self, model: String) -> ... { }

  // 改进
  pub fn some_method(&self, model: &str) -> ... { }
  ```
- **时间估计：** 1-2 天（需要审查大量函数）
- **收益：** 减少热路径的字符串分配
- **风险：** 中等（需要仔细测试）

---

### 优先级 2：代码结构优化（可维护性）

#### 4. 拆分大文件 accounts.rs 🟡
- **问题：** 1697 行（社区建议 < 600 行）
- **拆分方案：**
  ```
  src/http/admin/accounts/
  ├── mod.rs       # 路由注册 + 公共类型 (~200 行)
  ├── list.rs      # GET /accounts (~150 行)
  ├── create.rs    # POST /accounts (~200 行)
  ├── update.rs    # PATCH /accounts/:id (~250 行)
  ├── delete.rs    # DELETE /accounts/:id (~150 行)
  ├── oauth.rs     # OAuth 登录流程 (~300 行)
  ├── quota.rs     # 配额和健康检查 (~250 行)
  └── cookies.rs   # Cookie 操作 (~200 行)
  ```
- **时间估计：** 1-2 小时
- **收益：** 提升代码可读性和可维护性
- **风险：** 中等（大量代码移动）

#### 5. AccountService 方法分组 🟢
- **问题：** 7 个 `impl AccountService` 块分散在不同文件
- **改进：** 使用 trait 组织功能
  ```rust
  trait AccountLifecycle { ... }
  trait AccountQuota { ... }
  trait AccountAuth { ... }
  ```
- **时间估计：** 2-3 天
- **收益：** API 更清晰，更容易 mock
- **风险：** 低

---

### 优先级 3：可观测性改进（按需）

#### 6. 更细粒度的错误类型 🟢
- **改进：** 为错误添加更多上下文
  ```rust
  pub enum AccountServiceError {
      DatabaseQueryFailed {
          operation: &'static str,
          #[source] source: sqlx::Error,
      },
  }
  ```
- **时间估计：** 1-2 天
- **收益：** 更好的错误追踪和调试

#### 7. 日志字段标准化 🟢
- **改进：** 定义统一的日志字段常量
- **时间估计：** 半天
- **收益：** 更容易查询日志

#### 8. 使用 tracing::instrument 🟢
- **改进：** 自动记录函数进入/退出
- **时间估计：** 1 天
- **收益：** 更好的调用链追踪

---

### 优先级 4：性能监控（生产验证后）

#### 9. AccountPool 锁优化 🟡
- **问题：** 所有请求共享一个 Mutex
- **方案：**
  - 分片锁（shard by account_id）
  - 使用 RwLock（读多写少）
- **时间估计：** 2-3 天
- **收益：** 减少高并发锁争用
- **前置条件：** 生产环境监控证明这是瓶颈

---

## 📊 优化优先级总结

| 优化项 | 优先级 | 时间 | 收益 | 风险 | 状态 |
|--------|--------|------|------|------|------|
| serde_yaml 迁移 | 🔴 高 | 15m | 消除技术债务 | 低 | ✅ 完成 |
| Arc<AppConfig> | 🔴 高 | 1h | 性能提升 | 低 | ⏭️ 待做 |
| &str 参数 | 🟡 中 | 2d | 减少分配 | 中 | ⏭️ 待做 |
| 拆分大文件 | 🟡 中 | 2h | 可维护性 | 中 | ⏭️ 待做 |
| Trait 分组 | 🟢 低 | 3d | 代码组织 | 低 | ⏭️ 待做 |
| 错误类型 | 🟢 低 | 2d | 可调试性 | 低 | ⏭️ 待做 |
| 日志标准化 | 🟢 低 | 0.5d | 可观测性 | 低 | ⏭️ 待做 |
| instrument | 🟢 低 | 1d | 调用追踪 | 低 | ⏭️ 待做 |
| Pool 优化 | 🟡 按需 | 3d | 高并发性能 | 中 | ⏭️ 待验证 |

---

## 🎯 推荐执行顺序

**第一批（本周）：**
1. ✅ serde_yaml 迁移（已完成）
2. Arc<AppConfig>（性能优化）
3. 拆分 accounts.rs（可维护性）

**第二批（本月）：**
4. &str 参数优化
5. 日志标准化

**第三批（按需）：**
6. Trait 分组
7. 错误类型细化
8. tracing::instrument

**监控驱动：**
9. AccountPool 优化（等生产数据）

---

## 📝 备注

- 优先性能优化，因为直接影响用户体验
- 代码结构优化可以逐步进行
- 高风险改动需要完整测试覆盖
- 生产部署后根据监控数据调整优先级
