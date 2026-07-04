UPDATE storage_providers
SET name = '本地存储', updated_at = now()
WHERE provider_type = 'local' AND name = 'Local Storage';

UPDATE users
SET username = '系统管理员', updated_at = now()
WHERE role = 'super_admin' AND username = 'admin';

UPDATE user_profiles
SET display_name = '系统管理员', updated_at = now()
WHERE display_name = 'admin'
  AND user_id IN (SELECT id FROM users WHERE role = 'super_admin');

UPDATE api_tokens
SET name = '默认 Token'
WHERE name = 'Default API Token';

UPDATE theme_settings
SET value_json = jsonb_set(value_json, '{font}', '"系统圆体"'::jsonb, true),
    updated_at = now()
WHERE key = 'theme' AND value_json ->> 'font' = 'Inter Rounded';
