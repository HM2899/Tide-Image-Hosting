CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

CREATE TABLE users (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  email TEXT NOT NULL UNIQUE,
  username TEXT NOT NULL,
  password_hash TEXT NOT NULL,
  avatar_url TEXT,
  role TEXT NOT NULL CHECK (role IN ('guest_account','user','trusted','supporter','admin','super_admin')),
  status TEXT NOT NULL CHECK (status IN ('pending_email','active','banned','deleted')),
  login_failed_count INTEGER NOT NULL DEFAULT 0,
  locked_until TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE user_profiles (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  user_id UUID NOT NULL UNIQUE REFERENCES users(id),
  display_name TEXT,
  bio TEXT,
  avatar_file_object_id UUID,
  settings_json JSONB NOT NULL DEFAULT '{}',
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE user_groups (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  name TEXT NOT NULL,
  code TEXT NOT NULL UNIQUE,
  description TEXT NOT NULL DEFAULT '',
  is_default BOOLEAN NOT NULL DEFAULT false,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE storage_providers (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  name TEXT NOT NULL,
  provider_type TEXT NOT NULL CHECK (provider_type IN ('local','cloudflare_r2','onedrive','oracle_s3','oracle_oci_native','s3_compatible')),
  config_json JSONB NOT NULL DEFAULT '{}',
  is_default BOOLEAN NOT NULL DEFAULT false,
  enabled BOOLEAN NOT NULL DEFAULT true,
  priority INTEGER NOT NULL DEFAULT 100,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE quota_rules (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  group_id UUID NOT NULL UNIQUE REFERENCES user_groups(id),
  daily_upload_count INTEGER NOT NULL,
  daily_upload_bytes BIGINT NOT NULL,
  max_file_size BIGINT NOT NULL,
  total_storage_bytes BIGINT NOT NULL,
  daily_api_calls INTEGER NOT NULL,
  daily_random_calls INTEGER NOT NULL,
  require_review BOOLEAN NOT NULL DEFAULT true,
  require_captcha BOOLEAN NOT NULL DEFAULT false,
  allow_batch_upload BOOLEAN NOT NULL DEFAULT true,
  allow_tag_create BOOLEAN NOT NULL DEFAULT true,
  default_storage_provider_id UUID REFERENCES storage_providers(id),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE quota_usage (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  user_id UUID NOT NULL REFERENCES users(id),
  date DATE NOT NULL,
  uploaded_count INTEGER NOT NULL DEFAULT 0,
  uploaded_bytes BIGINT NOT NULL DEFAULT 0,
  api_calls INTEGER NOT NULL DEFAULT 0,
  random_calls INTEGER NOT NULL DEFAULT 0,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (user_id, date)
);

CREATE TABLE user_quota_overrides (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  user_id UUID NOT NULL REFERENCES users(id),
  quota_json JSONB NOT NULL,
  reason TEXT NOT NULL DEFAULT '',
  created_by UUID REFERENCES users(id),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE email_verifications (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  email TEXT NOT NULL,
  code_hash TEXT NOT NULL,
  purpose TEXT NOT NULL CHECK (purpose IN ('register','login','reset_password','change_email')),
  expires_at TIMESTAMPTZ NOT NULL,
  used_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE file_objects (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  sha256 TEXT NOT NULL UNIQUE,
  size BIGINT NOT NULL,
  mime_type TEXT NOT NULL,
  width INTEGER NOT NULL DEFAULT 0,
  height INTEGER NOT NULL DEFAULT 0,
  orientation TEXT NOT NULL CHECK (orientation IN ('landscape','portrait','square','unknown')),
  aspect_ratio TEXT NOT NULL DEFAULT '',
  ref_count INTEGER NOT NULL DEFAULT 0,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE images (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  user_id UUID NOT NULL REFERENCES users(id),
  file_object_id UUID NOT NULL REFERENCES file_objects(id),
  original_name TEXT NOT NULL,
  title TEXT NOT NULL DEFAULT '',
  description TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL CHECK (status IN ('pending_review','active','rejected','trashed','deleted','blocked')),
  visibility TEXT NOT NULL CHECK (visibility IN ('public','private','unlisted','password')),
  is_guest_upload BOOLEAN NOT NULL DEFAULT false,
  guest_ip TEXT,
  guest_user_agent TEXT,
  guest_fingerprint TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  trashed_at TIMESTAMPTZ,
  deleted_at TIMESTAMPTZ,
  delete_reason TEXT,
  deleted_by UUID REFERENCES users(id),
  restore_until TIMESTAMPTZ
);

CREATE TABLE storage_objects (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  file_object_id UUID NOT NULL REFERENCES file_objects(id),
  storage_provider_id UUID NOT NULL REFERENCES storage_providers(id),
  object_type TEXT NOT NULL CHECK (object_type IN ('original','preview','avatar','backup')),
  object_key TEXT NOT NULL,
  public_url TEXT,
  provider_file_id TEXT,
  etag TEXT,
  size BIGINT NOT NULL DEFAULT 0,
  status TEXT NOT NULL CHECK (status IN ('active','deleting','deleted','failed')),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (storage_provider_id, object_key)
);

CREATE TABLE tags (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  name TEXT NOT NULL,
  slug TEXT NOT NULL UNIQUE,
  created_by UUID REFERENCES users(id),
  status TEXT NOT NULL CHECK (status IN ('normal','disabled','blocked','pending')),
  usage_count INTEGER NOT NULL DEFAULT 0,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE image_tags (
  image_id UUID NOT NULL REFERENCES images(id),
  tag_id UUID NOT NULL REFERENCES tags(id),
  created_by UUID REFERENCES users(id),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (image_id, tag_id)
);

CREATE TABLE audit_tasks (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  image_id UUID NOT NULL REFERENCES images(id),
  audit_type TEXT NOT NULL CHECK (audit_type IN ('keyword','ai','llm','manual','third_party')),
  provider TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL CHECK (status IN ('pending','running','passed','rejected','manual_required','failed')),
  retry_count INTEGER NOT NULL DEFAULT 0,
  error_message TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  started_at TIMESTAMPTZ,
  finished_at TIMESTAMPTZ
);

CREATE TABLE audit_results (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  audit_task_id UUID NOT NULL REFERENCES audit_tasks(id),
  image_id UUID NOT NULL REFERENCES images(id),
  result TEXT NOT NULL,
  risk_level TEXT NOT NULL DEFAULT '',
  reason TEXT NOT NULL DEFAULT '',
  labels_json JSONB NOT NULL DEFAULT '[]',
  categories_json JSONB NOT NULL DEFAULT '{}',
  ocr_text TEXT NOT NULL DEFAULT '',
  provider TEXT NOT NULL DEFAULT '',
  model TEXT NOT NULL DEFAULT '',
  request_payload JSONB NOT NULL DEFAULT '{}',
  response_payload JSONB NOT NULL DEFAULT '{}',
  duration_ms INTEGER NOT NULL DEFAULT 0,
  tokens_used INTEGER NOT NULL DEFAULT 0,
  cost_estimate NUMERIC NOT NULL DEFAULT 0,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE api_tokens (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  user_id UUID NOT NULL REFERENCES users(id),
  name TEXT NOT NULL,
  token_hash TEXT NOT NULL UNIQUE,
  scopes_json JSONB NOT NULL DEFAULT '[]',
  expires_at TIMESTAMPTZ,
  last_used_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  revoked_at TIMESTAMPTZ
);

CREATE TABLE migration_tasks (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  source_storage_provider_id UUID NOT NULL REFERENCES storage_providers(id),
  target_storage_provider_id UUID NOT NULL REFERENCES storage_providers(id),
  migration_mode TEXT NOT NULL CHECK (migration_mode IN ('copy','move','backup')),
  filter_json JSONB NOT NULL DEFAULT '{}',
  total_count INTEGER NOT NULL DEFAULT 0,
  success_count INTEGER NOT NULL DEFAULT 0,
  failed_count INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL CHECK (status IN ('pending','running','paused','completed','failed','cancelled')),
  created_by UUID REFERENCES users(id),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  started_at TIMESTAMPTZ,
  completed_at TIMESTAMPTZ
);

CREATE TABLE migration_task_items (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  migration_task_id UUID NOT NULL REFERENCES migration_tasks(id),
  storage_object_id UUID NOT NULL REFERENCES storage_objects(id),
  source_object_key TEXT NOT NULL,
  target_object_key TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  retry_count INTEGER NOT NULL DEFAULT 0,
  error_message TEXT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE backup_tasks (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  backup_type TEXT NOT NULL CHECK (backup_type IN ('manual','scheduled','full','incremental')),
  target_storage_provider_id UUID REFERENCES storage_providers(id),
  status TEXT NOT NULL DEFAULT 'pending',
  include_files BOOLEAN NOT NULL DEFAULT false,
  include_logs BOOLEAN NOT NULL DEFAULT true,
  backup_size BIGINT NOT NULL DEFAULT 0,
  created_by UUID REFERENCES users(id),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  started_at TIMESTAMPTZ,
  completed_at TIMESTAMPTZ,
  error_message TEXT
);

CREATE TABLE backup_files (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  backup_task_id UUID NOT NULL REFERENCES backup_tasks(id),
  storage_provider_id UUID REFERENCES storage_providers(id),
  object_key TEXT NOT NULL,
  file_name TEXT NOT NULL,
  size BIGINT NOT NULL DEFAULT 0,
  sha256 TEXT NOT NULL DEFAULT '',
  encrypted BOOLEAN NOT NULL DEFAULT false,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE restore_tasks (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  backup_task_id UUID REFERENCES backup_tasks(id),
  status TEXT NOT NULL DEFAULT 'pending',
  restore_options_json JSONB NOT NULL DEFAULT '{}',
  created_by UUID REFERENCES users(id),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  started_at TIMESTAMPTZ,
  completed_at TIMESTAMPTZ,
  error_message TEXT
);

CREATE TABLE site_settings (
  key TEXT PRIMARY KEY,
  value_json JSONB NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE theme_settings (
  key TEXT PRIMARY KEY,
  value_json JSONB NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE smtp_settings (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  name TEXT NOT NULL,
  host TEXT NOT NULL DEFAULT '',
  port INTEGER NOT NULL DEFAULT 587,
  username TEXT NOT NULL DEFAULT '',
  password_encrypted TEXT NOT NULL DEFAULT '',
  from_email TEXT NOT NULL DEFAULT '',
  from_name TEXT NOT NULL DEFAULT '',
  enabled BOOLEAN NOT NULL DEFAULT false,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE captcha_settings (
  key TEXT PRIMARY KEY,
  value_json JSONB NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE system_logs (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  level TEXT NOT NULL,
  module TEXT NOT NULL,
  message TEXT NOT NULL,
  context_json JSONB NOT NULL DEFAULT '{}',
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE admin_operation_logs (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  admin_user_id UUID REFERENCES users(id),
  action TEXT NOT NULL,
  target_type TEXT NOT NULL,
  target_id UUID,
  context_json JSONB NOT NULL DEFAULT '{}',
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_users_email ON users(email);
CREATE INDEX idx_images_user_id ON images(user_id);
CREATE INDEX idx_images_file_object_id ON images(file_object_id);
CREATE INDEX idx_images_status ON images(status);
CREATE INDEX idx_images_created_at ON images(created_at);
CREATE INDEX idx_file_objects_sha256 ON file_objects(sha256);
CREATE INDEX idx_storage_objects_file_object_id ON storage_objects(file_object_id);
CREATE INDEX idx_storage_objects_storage_provider_id ON storage_objects(storage_provider_id);
CREATE INDEX idx_tags_slug ON tags(slug);
CREATE INDEX idx_image_tags_image_id ON image_tags(image_id);
CREATE INDEX idx_image_tags_tag_id ON image_tags(tag_id);
CREATE INDEX idx_quota_usage_user_id ON quota_usage(user_id);
CREATE INDEX idx_quota_usage_date ON quota_usage(date);

INSERT INTO storage_providers (name, provider_type, config_json, is_default, enabled, priority)
VALUES ('Local Storage', 'local', '{"root":"/data/storage","public_prefix":"/files","path_prefix":""}', true, true, 1);

INSERT INTO user_groups (name, code, description, is_default)
VALUES
  ('访客用户', 'guest', '访客公用账号配额', false),
  ('普通用户', 'normal', '邮箱注册用户默认配额', true),
  ('可信用户', 'trusted', '管理员提升的可信用户', false),
  ('公益支持者', 'supporter', '公益支持者高配额用户组', false),
  ('管理员', 'admin', '管理员不受普通配额限制', false);

INSERT INTO quota_rules (group_id, daily_upload_count, daily_upload_bytes, max_file_size, total_storage_bytes, daily_api_calls, daily_random_calls, require_review, require_captcha, allow_batch_upload, allow_tag_create, default_storage_provider_id)
SELECT id,
  CASE code WHEN 'guest' THEN 20 WHEN 'admin' THEN 100000 ELSE 200 END,
  CASE code WHEN 'guest' THEN 104857600 WHEN 'admin' THEN 1099511627776 ELSE 1073741824 END,
  CASE code WHEN 'guest' THEN 5242880 WHEN 'admin' THEN 1073741824 ELSE 52428800 END,
  CASE code WHEN 'guest' THEN 1073741824 WHEN 'admin' THEN 10995116277760 ELSE 10737418240 END,
  CASE code WHEN 'guest' THEN 100 WHEN 'admin' THEN 100000 ELSE 5000 END,
  CASE code WHEN 'guest' THEN 200 WHEN 'admin' THEN 100000 ELSE 5000 END,
  code = 'guest',
  code = 'guest',
  code <> 'guest',
  code <> 'guest',
  (SELECT id FROM storage_providers WHERE is_default = true LIMIT 1)
FROM user_groups;

INSERT INTO site_settings (key, value_json)
VALUES
  ('site', '{"title":"潮汐图床","subtitle":"公益图片托管服务","guest_upload_enabled":true,"guest_review_strategy":"manual_required"}'),
  ('random', '{"enabled":true,"default_image":"preview","limit_enabled":true}'),
  ('upload', '{"allowed_mime_types":["image/jpeg","image/png","image/gif","image/webp","image/avif"],"webp_enabled":true,"webp_max_width":512,"webp_max_height":512,"webp_quality":75}');

INSERT INTO theme_settings (key, value_json)
VALUES ('theme', '{"mode":"auto","preset":"macaron","radius":14,"blur":18,"font":"Inter Rounded"}');
