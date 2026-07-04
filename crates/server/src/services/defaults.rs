use crate::error::AppResult;
use serde_json::{Value, json};
use sqlx::PgPool;

pub fn site_settings() -> Value {
    json!({
        "title": "潮汐图床",
        "subtitle": "",
        "guest_upload_enabled": true,
        "guest_review_strategy": "manual_required"
    })
}

pub fn upload_settings() -> Value {
    json!({
        "allowed_mime_types": ["image/jpeg", "image/png", "image/gif", "image/webp", "image/avif"],
        "webp_enabled": true,
        "webp_max_width": 512,
        "webp_max_height": 512,
        "webp_quality": 75,
        "remove_exif": true,
        "max_tags_per_image": 10,
        "max_tag_length": 32,
        "tag_sensitive_words": [],
        "tag_review_required": false
    })
}

pub fn random_settings() -> Value {
    json!({
        "enabled": true,
        "default_image": "preview",
        "limit_enabled": true,
        "allow_tag_filter": true,
        "allow_orientation_filter": true,
        "allow_resolution_filter": true,
        "no_match_strategy": "not_found"
    })
}

pub fn theme_settings() -> Value {
    json!({
        "mode": "light",
        "preset": "blue_white",
        "radius": 16,
        "blur": 18,
        "mobile_blur": 10,
        "card_opacity": 0.72,
        "primary_color": "#1d6fd8",
        "accent_color": "#58b7ff",
        "background_color": "#eef7ff",
        "surface_color": "#ffffff",
        "font": "系统圆体",
        "background_image": "",
        "simplify_mobile_effects": true
    })
}

pub async fn ensure_default_settings(pool: &PgPool) -> AppResult<()> {
    upsert_missing_site_setting(pool, "site", site_settings()).await?;
    upsert_missing_site_setting(pool, "upload", upload_settings()).await?;
    upsert_missing_site_setting(pool, "random", random_settings()).await?;
    upsert_missing_theme_setting(pool, "theme", theme_settings()).await?;
    Ok(())
}

pub async fn public_site_setting(pool: &PgPool, key: &str, fallback: Value) -> AppResult<Value> {
    let value = sqlx::query_scalar("SELECT value_json FROM site_settings WHERE key=$1")
        .bind(key)
        .fetch_optional(pool)
        .await?
        .map(|value| merge_defaults(fallback.clone(), value))
        .unwrap_or(fallback);
    Ok(value)
}

pub async fn public_theme_setting(pool: &PgPool, key: &str, fallback: Value) -> AppResult<Value> {
    let value = sqlx::query_scalar("SELECT value_json FROM theme_settings WHERE key=$1")
        .bind(key)
        .fetch_optional(pool)
        .await?
        .map(|value| merge_defaults(fallback.clone(), value))
        .unwrap_or(fallback);
    Ok(value)
}

async fn upsert_missing_site_setting(pool: &PgPool, key: &str, value: Value) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO site_settings (key,value_json) VALUES ($1,$2) ON CONFLICT (key) DO UPDATE SET value_json=EXCLUDED.value_json || site_settings.value_json, updated_at=now() WHERE site_settings.value_json IS DISTINCT FROM EXCLUDED.value_json || site_settings.value_json",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

async fn upsert_missing_theme_setting(pool: &PgPool, key: &str, value: Value) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO theme_settings (key,value_json) VALUES ($1,$2) ON CONFLICT (key) DO UPDATE SET value_json=EXCLUDED.value_json || theme_settings.value_json, updated_at=now() WHERE theme_settings.value_json IS DISTINCT FROM EXCLUDED.value_json || theme_settings.value_json",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

fn merge_defaults(mut defaults: Value, value: Value) -> Value {
    let Some(defaults_object) = defaults.as_object_mut() else {
        return value;
    };
    let Some(value_object) = value.as_object() else {
        return defaults;
    };
    for (key, value) in value_object {
        defaults_object.insert(key.clone(), value.clone());
    }
    defaults
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_defaults_cover_required_frontend_settings() {
        let site = site_settings();
        assert_eq!(site["guest_upload_enabled"], true);
        assert_eq!(site["title"], "潮汐图床");

        let upload = upload_settings();
        assert_eq!(upload["webp_enabled"], true);
        assert_eq!(upload["remove_exif"], true);

        let theme = theme_settings();
        assert_eq!(theme["preset"], "blue_white");
        assert_eq!(theme["simplify_mobile_effects"], true);
    }

    #[test]
    fn setting_defaults_merge_without_overriding_existing_values() {
        let merged = merge_defaults(
            site_settings(),
            json!({
                "title": "自定义图床"
            }),
        );

        assert_eq!(merged["title"], "自定义图床");
        assert_eq!(merged["guest_upload_enabled"], true);
    }
}
