alter table model_requests
  add column service_tier text,
  add constraint model_requests_service_tier_ck check (
    service_tier is null
    or (
      octet_length(service_tier) between 1 and 64
      and service_tier !~ '[[:cntrl:]]'
    )
  );
