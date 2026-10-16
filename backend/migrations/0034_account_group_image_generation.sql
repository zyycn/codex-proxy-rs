-- 分组生图权限默认允许，保留已有分组及未显式设置权限的创建行为
ALTER TABLE account_groups
    ADD COLUMN allow_image_generation boolean NOT NULL DEFAULT true;
