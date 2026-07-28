-- Response IDs are opaque JSON strings. PostgreSQL text rejects NUL even though JSON accepts it.
-- Store their UTF-8 bytes without an index; Rust decodes them only at the audit/API boundary.
alter table model_requests
  alter column client_response_id type bytea
    using convert_to(client_response_id, 'UTF8'),
  alter column upstream_response_id type bytea
    using convert_to(upstream_response_id, 'UTF8');
