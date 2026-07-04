UPDATE quota_rules qr
SET require_review = false,
    updated_at = now()
FROM user_groups ug
WHERE qr.group_id = ug.id
  AND ug.code = 'normal'
  AND qr.require_review = true;
