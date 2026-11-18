# v3.52.0

## 新增功能

- 请求记录可按历史账号名称或邮箱筛选
- 内置 GPT-6.2 Astra 和 Sol 价目，Ultrafast 按常规价估算

## 优化改进

- 请求记录用双闪电标识 Ultrafast，用单闪电标识 Fast
- 请求错误来源图标改为无底色显示

## 升级须知

- 调用 `GET /api/admin/usage/records` 的客户端需将查询参数 `accountName` 改为 `accountSearch`，旧参数会返回 HTTP 400
- 自行部署时请确认 PostgreSQL 版本为 16 或更高，默认 Compose 使用 PostgreSQL 18
