create table openai_client_installations (
    client_api_key_id text not null references client_api_keys (id) on delete cascade,
    provider_account_id text not null references provider_accounts (id) on delete cascade,
    installation_id uuid not null,
    primary key (client_api_key_id, provider_account_id)
);

create index openai_client_installations_provider_account_idx
    on openai_client_installations (provider_account_id);
