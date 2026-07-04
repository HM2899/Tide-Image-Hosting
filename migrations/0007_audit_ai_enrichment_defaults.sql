INSERT INTO site_settings (key, value_json)
VALUES (
  'audit',
  '{
    "ai_enabled": true,
    "service_type": "fastapi",
    "failure_strategy": "manual_required",
    "keyword_enabled": true,
    "filename_keyword_enabled": true,
    "ocr_enabled": true,
    "description_enabled": true,
    "tag_suggestions_enabled": true,
    "keywords": []
  }'::jsonb
)
ON CONFLICT (key) DO UPDATE
SET value_json = '{
    "ai_enabled": true,
    "service_type": "fastapi",
    "failure_strategy": "manual_required",
    "keyword_enabled": true,
    "filename_keyword_enabled": true,
    "ocr_enabled": true,
    "description_enabled": true,
    "tag_suggestions_enabled": true,
    "keywords": []
  }'::jsonb || site_settings.value_json,
    updated_at = now();
