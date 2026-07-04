WITH duplicates AS (
  SELECT id,
         username,
         row_number() OVER (PARTITION BY lower(trim(username)) ORDER BY created_at, id) AS rn
  FROM users
  WHERE trim(username) <> ''
)
UPDATE users
SET username = left(trim(users.username), 48) || '-' || substr(users.id::text, 1, 8),
    updated_at = now()
FROM duplicates
WHERE users.id = duplicates.id
  AND duplicates.rn > 1;

UPDATE users
SET username = 'user-' || substr(id::text, 1, 8),
    updated_at = now()
WHERE trim(username) = '';

WITH initial_admin AS (
  SELECT id
  FROM users
  WHERE role = 'super_admin'
    AND email = 'admin@example.com'
  LIMIT 1
)
UPDATE users
SET username = left(trim(users.username), 48) || '-' || substr(users.id::text, 1, 8),
    updated_at = now()
FROM initial_admin
WHERE lower(trim(users.username)) = 'admin'
  AND users.id <> initial_admin.id;

UPDATE users
SET username = 'admin'
WHERE role = 'super_admin'
  AND email = 'admin@example.com';

CREATE UNIQUE INDEX IF NOT EXISTS idx_users_username_lower_unique ON users (lower(username));

ALTER TABLE audit_tasks
ADD COLUMN IF NOT EXISTS require_review BOOLEAN NOT NULL DEFAULT false;
