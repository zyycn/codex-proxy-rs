create table if not exists model_plan_snapshots (
  plan_type text primary key,
  models_json text not null,
  fetched_at text not null
);
