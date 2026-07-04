CREATE TABLE IF NOT EXISTS storage_routes (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  name TEXT NOT NULL,
  scope_type TEXT NOT NULL CHECK (scope_type IN ('global','role','group','user')),
  scope_value TEXT NOT NULL DEFAULT '',
  storage_provider_id UUID NOT NULL REFERENCES storage_providers(id),
  enabled BOOLEAN NOT NULL DEFAULT true,
  priority INTEGER NOT NULL DEFAULT 100,
  note TEXT NOT NULL DEFAULT '',
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_storage_routes_match
ON storage_routes(enabled, scope_type, scope_value, priority ASC, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_storage_routes_provider
ON storage_routes(storage_provider_id);
