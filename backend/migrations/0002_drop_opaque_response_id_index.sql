-- Provider response IDs are opaque protocol values, not relational identifiers.
-- Indexing the raw text makes PostgreSQL reject otherwise valid long handles.
drop index model_requests_client_response_uq;
