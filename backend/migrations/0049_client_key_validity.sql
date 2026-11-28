ALTER TABLE client_api_keys
    ADD COLUMN starts_at timestamptz,
    ADD COLUMN expires_at timestamptz,
    ADD CONSTRAINT client_api_keys_validity_order CHECK (
        (starts_at IS NULL AND expires_at IS NULL) OR
        (starts_at IS NOT NULL AND expires_at IS NOT NULL AND starts_at < expires_at)
    );
