-- 删除被主键覆盖的冗余唯一索引（docs/project-redundancy-boundary-audit.md DB-01）。
-- provider_accounts.id 已是主键，(id, provider_kind) 第二列不增加唯一性，
-- 工程内也没有外键依赖该复合索引。

drop index provider_accounts_id_kind_uq;
