alter table account_groups
  drop constraint account_groups_color_ck;

update account_groups
set color = color || 'FF';

alter table account_groups
  add constraint account_groups_color_ck check (color ~ '^#[0-9A-F]{8}$');
