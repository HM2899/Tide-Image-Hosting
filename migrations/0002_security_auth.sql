CREATE TABLE sessions (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  user_id UUID NOT NULL REFERENCES users(id),
  token_hash TEXT NOT NULL UNIQUE,
  ip TEXT,
  user_agent TEXT,
  expires_at TIMESTAMPTZ NOT NULL,
  revoked_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE admin_operation_logs ADD COLUMN IF NOT EXISTS detail_json JSONB NOT NULL DEFAULT '{}';
ALTER TABLE admin_operation_logs ADD COLUMN IF NOT EXISTS ip TEXT;
ALTER TABLE admin_operation_logs ADD COLUMN IF NOT EXISTS user_agent TEXT;

CREATE INDEX idx_sessions_user_id ON sessions(user_id);
CREATE INDEX idx_sessions_token_hash ON sessions(token_hash);
CREATE INDEX idx_email_verifications_email ON email_verifications(email);
CREATE INDEX idx_email_verifications_purpose ON email_verifications(purpose);

INSERT INTO site_settings (key, value_json)
VALUES
  ('auth', '{"email_verification_required":true,"session_days":30}'),
  ('security', '{"login_failed_limit":10,"login_lock_minutes":30,"api_token_default_days":365}')
ON CONFLICT (key) DO NOTHING;
