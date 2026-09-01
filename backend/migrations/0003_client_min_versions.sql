alter table runtime_settings
  add column min_codex_desktop_version text,
  add column min_codex_cli_version text,
  add constraint runtime_settings_client_versions_ck check (
    (
      min_codex_desktop_version is null
      or (
        octet_length(min_codex_desktop_version) between 1 and 64
        and min_codex_desktop_version = btrim(min_codex_desktop_version)
        and min_codex_desktop_version !~ '[[:cntrl:]]'
      )
    )
    and (
      min_codex_cli_version is null
      or (
        octet_length(min_codex_cli_version) between 1 and 64
        and min_codex_cli_version = btrim(min_codex_cli_version)
        and min_codex_cli_version !~ '[[:cntrl:]]'
      )
    )
  );
