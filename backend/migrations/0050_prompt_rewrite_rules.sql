ALTER TABLE runtime_settings
    ADD COLUMN prompt_rewrite_rules_json jsonb NOT NULL DEFAULT '[]'::jsonb
    CHECK (jsonb_typeof(prompt_rewrite_rules_json) = 'array');
