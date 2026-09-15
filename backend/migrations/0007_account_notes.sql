-- 备注由管理员维护，独立于上游身份和凭据刷新。
alter table provider_accounts
    add column notes text
    check (notes is null or char_length(notes) <= 500);
