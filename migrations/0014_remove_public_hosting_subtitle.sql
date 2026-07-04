UPDATE site_settings
SET value_json = jsonb_set(value_json, '{subtitle}', '""'::jsonb, true),
    updated_at = now()
WHERE key = 'site'
  AND value_json ->> 'subtitle' = '公益图片托管服务';
