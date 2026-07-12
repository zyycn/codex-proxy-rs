# 死代码与异味审查(backend)

> 审查于 `feat/postgres-redis-migration` 工作树(fleet 改名、dispatch 重排之后)。方法:①编译器 `dead_code` lint——CI 跑 `clippy -D warnings`,所以**私有项与 `pub(crate)` 项的死代码必然为 0**(否则 CI 挂),死代码只可能藏在裸 `pub` 且 crate 内无调用者的项里;②对全部 441 个 `pub fn` 做"全局引用计数=1(仅定义)"筛查;③codegraph 逐个验证候选的真实调用者(排除 trait/宏/route/serde 间接);④`clippy` 加 `nursery`/`cognitive_complexity`/`too_many_lines`/`significant_drop` 等非默认 lint 扫异味。

先说结论:**这是我审过纪律最干净的一处**——零 `#[allow(dead_code)]`、零 `#[allow(unused)]`、零 TODO/FIXME/HACK、零注释掉的代码块、零可疑残留命名(legacy/deprecated/old/v1)、最大文件 873 行(无失控巨型文件)。大重构没有留下常见的残骸。真正的死代码只有 **4 个**,且全是同一模式——"纯委托 wrapper,预留了但从未被采用";异味也很少,值得一提的是 **2 处锁持有过久**(与 [concurrency-review](concurrency-review.md) 呼应)。没有 P0,没有架构性坏味道。

## P1 — 死代码(4 个,建议删)

四个都是裸 `pub` 函数、crate 内(含 tests/)**零调用者**、函数体只有一行委托给另一个真正在用的函数。共同性质:**它们是"便利封装"或"变体",但调用方都直接用了被委托的原函数,wrapper 反而没人碰**。均属 binary crate 的 lib pub 项、无外部消费者,删除不影响编译与运行时行为。

1. **`openai_sse_frame`** —— `api/client/sse.rs:42`。函数体 `encode_sse_event(event, data)`,零引用;同文件的 `done_sse_frame` / `event_stream_response` / `SseResponseOptions` 都在用,只有这一个死。调用方全都直接用 `encode_sse_event`。
2. **`sse_frame_separator`** —— `upstream/openai/protocol/sse.rs:67`。函数体 `sse_frame_separator_bytes(input.as_bytes())`,零引用;真正被用的是它内部委托的 `sse_frame_separator_bytes`(2 callers)和兄弟 `sse_frame_end`(:73,活)。这个 `&str` 版从没被调。
3. **`parse_optional_rfc3339_utc`** —— `infra/time.rs:21`。`parse_rfc3339_utc` 的 `Option` 版(`value.map(...).transpose()`),零引用;需要解析可选时间的地方都自己 `map` 了。
4. **`AccountScheduler::set_weights`** —— `fleet/scheduler/mod.rs:101`。注释写"策略切换或热更时调用",但实际热更走的是**重建整个 `AccountScheduler`**(权重在 `new` 时注入),这个 setter 从没被调——是 `AccountScheduler` 唯一零引用的方法(兄弟 `reset_cursor`/`report_feedback`/`forget_feedback`/`clear_feedback` 都有引用)。删除时连同 `&mut self` 的可变借用需求一起消失,是净收益。

> 顺带:`api/client/sse.rs` 曾被怀疑是 Chat 端点删除后的整体遗留,核实后**不是**——除 `openai_sse_frame` 外其余四项均有 responses 端点在用。只死了一个函数,不是一个文件。

## P1 — 锁持有过久(与并发审查呼应)

clippy 的 `significant_drop` 系列精确命中了 [concurrency-review](concurrency-review.md) 关注的锁路径:

5. **`auth/service.rs:134`** —— `significant_drop_tightening`:登录失败计数的锁 guard 在临界区外仍被持有,可提前 drop。正是并发审查 P2 里"登录限流 check/record 分离"那处锁,收紧持有范围顺带缩小 TOCTOU 窗口。
6. **`fleet/refresh/service.rs:477`** —— `significant_drop_in_scrutinee`:`if let ... = <持锁表达式>` 的 scrutinee 里产生的锁临时量,会在整个 `if let` 块结束才 drop——锁被 if 分支体的时长白白拖长。改成先绑定、取值、显式 drop 再判断即可。

## P2 — 轻度异味(视精力顺手清)

7. **31 个"仅测试用"的 pub 构造器** —— `upstream/openai/protocol/websocket_errors.rs` 里 `websocket_*_invalid_required_fields` / `websocket_*_missing_*` 共 31 个 `pub fn`,构造畸形 WS 事件供测试断言用,**只被 tests/ 引用、生产路径零调用**。它们是"测试脚手架泄漏成生产 pub API"——理想应 `#[cfg(test)]` gate 或下沉到 `tests/fixtures`。同类还有 `discarded_rows`/`imported_rows`/`normalized_rows`(import 报告 getter)、`with_request_spacing`/`with_retry_delays`(builder)、`set_service_tier` 等。注意:这些**不是死代码**(有测试引用),是可见性/放置问题;而且部分(如 import 报告 getter)作为公共结构的访问器是合理的,不必一刀切。建议至少把 websocket_errors 那 31 个 gate 起来。
8. **零散 clippy nursery 项**(逐个都是几行的小修):
   - `upstream/openai/transport/websocket_pump.rs:127` —— `needless_pass_by_ref_mut`:参数是 `&mut` 但从未可变使用,签名过度要求可变借用。
   - `dispatch/recovery/implicit_resume.rs:22` —— `derive_partial_eq_without_eq`:derive 了 `PartialEq` 可顺带 `Eq`。
   - `fleet/manage/lifecycle.rs:435` —— `suboptimal_flops`:`a * b + c` 可用 `mul_add` 提精度/性能(微优化)。
   - `api/client/responses.rs:257` —— `single_option_map`:函数只是 `map` 一下参数,可内联。
   - `api/admin/usage_routes.rs:578` —— `use_self`:结构名重复,可用 `Self`。
   - `api/admin/accounts_routes/export_routes.rs:4` —— `redundant_pub_crate`:private 模块内的 `pub(crate)` 冗余。

## 做得好的(记录一下,免得反复纠结)

- **零显式压制**:全仓无 `#[allow(dead_code)]` / `#[allow(unused)]`,唯一的 `#[allow(clippy::too_many_arguments)]` 和唯一的 `#[expect(...)]`(trace.rs 的 tower 回调按值传参)都是正当的。团队没有靠 allow 掩盖问题。
- **零技术债标记**:零 TODO/FIXME/HACK/XXX、零注释掉的代码块。
- **私有死代码被 CI 清零**:`-D warnings` 让编译器的 `dead_code` lint 成为硬门禁,私有与 `pub(crate)` 的未使用项无法进主干。这是死代码只剩 4 个的根本原因。
- **文件规模健康**:最大 873 行(`fleet/pool/mod.rs`),前 15 大文件都在 650–873 之间,没有几千行的上帝文件;`too_many_lines`/`cognitive_complexity` 在生产代码里近乎零命中。
- **无重复能力/无垃圾桶模块**:无 `utils`/`helper`/`misc`,纯 re-export 的 `mod.rs`(scheduler/strategy、quota、api…)都是正常的模块聚合,不是空壳。

## 核实过的误报

- **"441 个 pub fn 里死代码应该不少"** —— 不成立。逐个验证后只有 4 个真死;绝大多数 pub 项经 codegraph 确认有调用者,或是 axum route handler(被 `get()`/`post()` 注册)、serde 派生目标、trait 方法、`main`/bootstrap 入口。裸 pub 数量大 ≠ 死代码多。
- **"仅测试用的 pub = 死代码"** —— 不是。它们被 tests/ 引用,编译器和引用计数都能看到;归类为可见性/放置异味(P2-7),不是死代码。
- **238 条 clippy warning 听起来很多** —— 其中约 24 条是 `nursery` 元标签、大量是 `missing_const_for_fn` 一类风格项(已在扫描时 `-A` 掉主要噪音),真正值得动的生产代码异味就是 P1-2 的两处锁 + P2-8 的六个小项。默认 lint 集(CI 跑的)本就是 0。

## 方法局限(诚实说明)

- "全局引用计数=1"筛查对**通用函数名**(如 `new`/`from`/`build`/`default`)会漏报——同名符号在别处巧合出现会互相"证明存在"。所以本报告的死代码清单是**下界**,可能还有极少数通用名死函数未被这套启发式捕获;但 codegraph 对候选的验证是准的,列出的 4 个确定为死。
- 未跑 `cargo-udeps`(需 nightly)——unused 依赖另见 [dependency-review](dependency-review.md)(已用 cargo-machete 覆盖)。

## 建议动手顺序

1. **P1 死代码 4 个** —— 一个 commit 删掉,零风险(零调用者),`cargo build` 即验证。顺带 `set_weights` 删除后 `AccountScheduler` 少一个 `&mut` 方法。
2. **P1 两处锁**(auth/service.rs:134、refresh/service.rs:477)—— 与 [concurrency-review](concurrency-review.md) 的锁修复合并到同一个 PR 更合适。
3. **P2-7 的 31 个测试构造器** gate 进 `#[cfg(test)]` 或下沉 fixtures —— 机械但面稍大,单独一个 commit。
4. **P2-8 零散项** —— 顺手 `cargo clippy --fix` 能自动改掉大部分,人工确认即可。
