alter table account_groups
  add column color text;

update account_groups
set color = '#2563EB';

alter table account_groups
  alter column color set not null,
  add constraint account_groups_color_ck check (color ~ '^#[0-9A-F]{6}$');
