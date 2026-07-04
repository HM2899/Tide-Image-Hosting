ALTER TABLE storage_providers
ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_storage_providers_active_order
ON storage_providers(deleted_at, enabled, is_default DESC, priority ASC, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_storage_providers_type_active
ON storage_providers(provider_type, deleted_at, priority ASC);
