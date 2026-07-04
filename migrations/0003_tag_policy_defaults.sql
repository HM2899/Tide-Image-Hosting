UPDATE site_settings
SET value_json = '{"max_tags_per_image":10,"max_tag_length":32,"tag_sensitive_words":[],"tag_review_required":false}'::jsonb || value_json,
    updated_at = now()
WHERE key = 'upload';

UPDATE site_settings
SET value_json = '{"allow_tag_filter":true,"allow_orientation_filter":true,"allow_resolution_filter":true,"no_match_strategy":"not_found"}'::jsonb || value_json,
    updated_at = now()
WHERE key = 'random';
