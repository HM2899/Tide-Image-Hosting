UPDATE site_settings
SET value_json = value_json || '{"remove_exif":true}'::jsonb,
    updated_at = now()
WHERE key = 'upload'
  AND NOT (value_json ? 'remove_exif');
