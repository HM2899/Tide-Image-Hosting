use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::{AppError, AppResult};
use crate::models::{ImageQuery, ImageRow, RandomQuery, StorageProviderRow, TokenClaims};
use crate::services::{audit, quota, storage_registry};
use crate::storage::{StorageProvider, original_key, preview_key};
use axum::extract::Multipart;
use chrono::{Duration, Utc};
use image::GenericImageView;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};
use jsonwebtoken::{EncodingKey, Header, encode};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use tide_shared::{
    ApiErrorBody, ImageLinks, ImageSummary, Page, RandomImageResponse, UploadBatchItem,
    UploadBatchResponse, UploadResponse, UploadResult,
};
use uuid::Uuid;

pub struct UploadActor {
    pub user_id: Uuid,
    pub role: String,
    pub is_guest: bool,
    pub guest_ip: Option<String>,
    pub guest_user_agent: Option<String>,
    pub guest_fingerprint: Option<String>,
}

#[derive(Clone, Copy)]
pub enum LinkContext {
    Public,
    Authorized { image_id: Uuid },
}

struct UploadSettings {
    allowed_mime_types: Vec<String>,
    remove_exif: bool,
    webp_enabled: bool,
    webp_max_width: u32,
    webp_max_height: u32,
    webp_quality: f32,
    guest_ip_daily_limit: Option<i64>,
    guest_review_strategy: GuestReviewStrategy,
    tag_policy: TagPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GuestReviewStrategy {
    ManualRequired,
    Reject,
    Auto,
    GroupPolicy,
}

#[derive(Clone)]
pub(crate) struct TagPolicy {
    max_tags_per_image: usize,
    max_tag_length: usize,
    sensitive_words: Vec<String>,
    review_required: bool,
}

impl TagPolicy {
    pub(crate) fn tag_review_required(&self) -> bool {
        self.review_required
    }

    pub(crate) fn remaining_slots(&self, current_count: usize) -> usize {
        self.max_tags_per_image.saturating_sub(current_count)
    }
}

pub async fn handle_upload(
    state: &AppState,
    mut actor: UploadActor,
    multipart: Multipart,
) -> AppResult<UploadResult> {
    let mut form = parse_upload_form(multipart).await?;
    if form.files.is_empty() {
        return Err(AppError::BadRequest("file is required".to_string()));
    }
    if actor.guest_fingerprint.is_none() {
        actor.guest_fingerprint = form.guest_fingerprint;
    }
    let settings = load_upload_settings(state).await?;
    form.tags = normalize_tag_list(&form.tags, &settings.tag_policy)?;
    let quota_row = quota::load_quota(&state.pool, actor.user_id, &actor.role).await?;
    if form.files.len() > 1 && !quota_row.allow_batch_upload && quota_row.group_code != "admin" {
        return Err(AppError::Forbidden(
            "batch upload is disabled for this user group".to_string(),
        ));
    }
    if actor.is_guest {
        enforce_guest_ip_limit(state, &actor, &settings, form.files.len() as i64).await?;
    }
    let provider_row = storage_registry::provider_for_upload(
        state,
        actor.user_id,
        &actor.role,
        &quota_row.group_code,
        quota_row.default_storage_provider_id,
    )
    .await?;
    let provider = storage_registry::build_provider(state, &provider_row).await?;
    provider.health_check().await?;

    if form.files.len() == 1 {
        let file = form
            .files
            .pop()
            .ok_or_else(|| AppError::BadRequest("file is required".to_string()))?;
        return Ok(UploadResult::Single(
            upload_file(
                state,
                &actor,
                file,
                &form.tags,
                &settings,
                &provider_row,
                provider.as_ref(),
            )
            .await?,
        ));
    }

    let total = form.files.len();
    let mut succeeded = 0usize;
    let mut items = Vec::with_capacity(total);
    for file in form.files {
        let file_name = file.file_name.clone();
        match upload_file(
            state,
            &actor,
            file,
            &form.tags,
            &settings,
            &provider_row,
            provider.as_ref(),
        )
        .await
        {
            Ok(response) => {
                succeeded += 1;
                items.push(UploadBatchItem {
                    file_name,
                    success: true,
                    response: Some(response),
                    error: None,
                });
            }
            Err(error) => {
                items.push(UploadBatchItem {
                    file_name,
                    success: false,
                    response: None,
                    error: Some(ApiErrorBody {
                        code: error.code().to_string(),
                        message: error.to_string(),
                    }),
                });
            }
        }
    }
    Ok(UploadResult::Batch(UploadBatchResponse {
        total,
        succeeded,
        failed: total - succeeded,
        items,
    }))
}

async fn upload_file(
    state: &AppState,
    actor: &UploadActor,
    file: UploadFile,
    tags: &[String],
    settings: &UploadSettings,
    provider_row: &StorageProviderRow,
    provider: &dyn StorageProvider,
) -> AppResult<UploadResponse> {
    if file.bytes.is_empty() {
        return Err(AppError::BadRequest("file is required".to_string()));
    }
    let mime_type = infer_mime(&file.bytes, &file.file_name, settings)?;
    let quota_row = quota::load_quota(&state.pool, actor.user_id, &actor.role).await?;
    quota::ensure_upload_allowed(
        &state.pool,
        actor.user_id,
        &quota_row,
        file.bytes.len() as i64,
    )
    .await?;
    let mut bytes = file.bytes;
    let file_name = file.file_name;
    let image =
        image::load_from_memory(&bytes).map_err(|err| AppError::BadRequest(err.to_string()))?;
    if settings.remove_exif {
        bytes = strip_image_metadata(&image, &bytes, &mime_type)?;
    }
    let sha256 = hex::encode(Sha256::digest(&bytes));
    let (width, height) = image.dimensions();
    let orientation = orientation(width, height);
    let ratio = ratio(width, height);
    let preview = build_preview(&image, settings)?;
    let ext = extension_for(&file_name, &mime_type);

    let existing = sqlx::query_scalar::<_, Uuid>("SELECT id FROM file_objects WHERE sha256=$1")
        .bind(&sha256)
        .fetch_optional(&state.pool)
        .await?;
    let deduplicated = existing.is_some();
    let file_object_id = if let Some(id) = existing {
        ensure_upload_storage_objects(
            state,
            id,
            &bytes,
            &mime_type,
            &preview,
            settings,
            provider_row,
            provider,
            &sha256,
            &ext,
        )
        .await?;
        id
    } else {
        let mut tx = state.pool.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO file_objects (sha256,size,mime_type,width,height,orientation,aspect_ratio,ref_count) VALUES ($1,$2,$3,$4,$5,$6,$7,1) RETURNING id",
        )
        .bind(&sha256)
        .bind(bytes.len() as i64)
        .bind(&mime_type)
        .bind(width as i32)
        .bind(height as i32)
        .bind(orientation)
        .bind(&ratio)
        .fetch_one(&mut *tx)
        .await?;
        let original_object_key = original_key(&sha256, &ext);
        let preview_object_key = preview_key(&sha256);
        tx.commit().await?;
        let original_object = provider
            .put_object(&original_object_key, &bytes, &mime_type)
            .await?;
        let preview_object = if settings.webp_enabled {
            Some(
                provider
                    .put_object(&preview_object_key, &preview, "image/webp")
                    .await?,
            )
        } else {
            None
        };
        tx = state.pool.begin().await?;
        sqlx::query("INSERT INTO storage_objects (file_object_id,storage_provider_id,object_type,object_key,public_url,etag,size,status) VALUES ($1,$2,'original',$3,$4,$5,$6,'active')")
            .bind(id)
            .bind(provider_row.id)
            .bind(original_object.object_key)
            .bind(original_object.public_url)
            .bind(original_object.etag)
            .bind(original_object.size)
            .execute(&mut *tx)
            .await?;
        if let Some(preview_object) = preview_object {
            sqlx::query("INSERT INTO storage_objects (file_object_id,storage_provider_id,object_type,object_key,public_url,etag,size,status) VALUES ($1,$2,'preview',$3,$4,$5,$6,'active')")
                .bind(id)
                .bind(provider_row.id)
                .bind(preview_object.object_key)
                .bind(preview_object.public_url)
                .bind(preview_object.etag)
                .bind(preview_object.size)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        id
    };

    let mut tx = state.pool.begin().await?;
    if deduplicated {
        sqlx::query("UPDATE file_objects SET ref_count=ref_count+1, updated_at=now() WHERE id=$1")
            .bind(file_object_id)
            .execute(&mut *tx)
            .await?;
    }
    let manual_review_required = upload_requires_manual_review(actor, &quota_row, settings)?;
    let visibility = default_upload_visibility(actor);
    let image_id: Uuid = sqlx::query_scalar(
        "INSERT INTO images (user_id,file_object_id,original_name,title,status,visibility,is_guest_upload,guest_ip,guest_user_agent,guest_fingerprint,restore_until) VALUES ($1,$2,$3,$4,'active',$5,$6,$7,$8,$9,$10) RETURNING id",
    )
    .bind(actor.user_id)
    .bind(file_object_id)
    .bind(&file_name)
    .bind(&file_name)
    .bind(visibility)
    .bind(actor.is_guest)
    .bind(actor.guest_ip.clone())
    .bind(actor.guest_user_agent.clone())
    .bind(actor.guest_fingerprint.clone())
    .bind(Utc::now() + Duration::days(30))
    .fetch_one(&mut *tx)
    .await?;
    quota::increment_upload(&mut tx, actor.user_id, bytes.len() as i64).await?;
    tx.commit().await?;

    attach_tags(
        state,
        image_id,
        actor.user_id,
        &actor.role,
        tags,
        quota_row.allow_tag_create,
        &settings.tag_policy,
    )
    .await?;
    let task_id = audit::create_upload_audit_task(state, image_id, manual_review_required).await?;
    audit::spawn_upload_audit(state.clone(), task_id);
    let links = links_for(
        state,
        file_object_id,
        &file_name,
        LinkContext::Authorized { image_id },
    )
    .await?;
    Ok(UploadResponse {
        id: image_id,
        url: links.url,
        preview_url: links.preview_url,
        markdown: links.markdown,
        html: links.html,
        preview_markdown: links.preview_markdown,
        preview_html: links.preview_html,
        status: "active".to_string(),
        deduplicated,
        tags: tags.to_vec(),
    })
}

fn default_upload_visibility(actor: &UploadActor) -> &'static str {
    if actor.is_guest { "public" } else { "private" }
}

#[allow(clippy::too_many_arguments)]
async fn ensure_upload_storage_objects(
    state: &AppState,
    file_object_id: Uuid,
    bytes: &[u8],
    mime_type: &str,
    preview: &[u8],
    settings: &UploadSettings,
    provider_row: &StorageProviderRow,
    provider: &dyn StorageProvider,
    sha256: &str,
    ext: &str,
) -> AppResult<()> {
    let original_exists =
        active_storage_object_exists(state, provider_row.id, file_object_id, "original", provider)
            .await?;
    if !original_exists {
        let object_key = original_key(sha256, ext);
        let stored = provider.put_object(&object_key, bytes, mime_type).await?;
        upsert_storage_object(
            state,
            StorageObjectUpsert {
                file_object_id,
                provider_id: provider_row.id,
                object_type: "original",
                object_key: &stored.object_key,
                public_url: stored.public_url.as_deref(),
                etag: stored.etag.as_deref(),
                size: stored.size,
            },
        )
        .await?;
    }

    if settings.webp_enabled {
        let preview_exists = active_storage_object_exists(
            state,
            provider_row.id,
            file_object_id,
            "preview",
            provider,
        )
        .await?;
        if !preview_exists {
            let object_key = preview_key(sha256);
            let stored = provider
                .put_object(&object_key, preview, "image/webp")
                .await?;
            upsert_storage_object(
                state,
                StorageObjectUpsert {
                    file_object_id,
                    provider_id: provider_row.id,
                    object_type: "preview",
                    object_key: &stored.object_key,
                    public_url: stored.public_url.as_deref(),
                    etag: stored.etag.as_deref(),
                    size: stored.size,
                },
            )
            .await?;
        }
    }
    Ok(())
}

async fn active_storage_object_exists(
    state: &AppState,
    provider_id: Uuid,
    file_object_id: Uuid,
    object_type: &str,
    provider: &dyn StorageProvider,
) -> AppResult<bool> {
    let rows = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id,object_key
         FROM storage_objects
         WHERE file_object_id=$1 AND storage_provider_id=$2 AND object_type=$3 AND status='active'
         ORDER BY updated_at DESC, created_at DESC",
    )
    .bind(file_object_id)
    .bind(provider_id)
    .bind(object_type)
    .fetch_all(&state.pool)
    .await?;
    for row in rows {
        if provider.head_object(&row.1).await.unwrap_or(false) {
            return Ok(true);
        }
        sqlx::query("UPDATE storage_objects SET status='failed', updated_at=now() WHERE id=$1")
            .bind(row.0)
            .execute(&state.pool)
            .await?;
    }
    Ok(false)
}

struct StorageObjectUpsert<'a> {
    file_object_id: Uuid,
    provider_id: Uuid,
    object_type: &'a str,
    object_key: &'a str,
    public_url: Option<&'a str>,
    etag: Option<&'a str>,
    size: i64,
}

async fn upsert_storage_object(state: &AppState, object: StorageObjectUpsert<'_>) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO storage_objects (file_object_id,storage_provider_id,object_type,object_key,public_url,etag,size,status)
         VALUES ($1,$2,$3,$4,$5,$6,$7,'active')
         ON CONFLICT (storage_provider_id,object_key)
         DO UPDATE SET file_object_id=EXCLUDED.file_object_id,object_type=EXCLUDED.object_type,public_url=EXCLUDED.public_url,etag=EXCLUDED.etag,size=EXCLUDED.size,status='active',updated_at=now()",
    )
    .bind(object.file_object_id)
    .bind(object.provider_id)
    .bind(object.object_type)
    .bind(object.object_key)
    .bind(object.public_url)
    .bind(object.etag)
    .bind(object.size)
    .execute(&state.pool)
    .await?;
    Ok(())
}

struct UploadForm {
    files: Vec<UploadFile>,
    tags: Vec<String>,
    guest_fingerprint: Option<String>,
}

struct UploadFile {
    file_name: String,
    bytes: Vec<u8>,
}

async fn parse_upload_form(mut multipart: Multipart) -> AppResult<UploadForm> {
    let mut files = Vec::new();
    let mut tags = Vec::new();
    let mut guest_fingerprint = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| AppError::BadRequest(err.to_string()))?
    {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "file" | "files" | "image" => {
                let file_name = field.file_name().unwrap_or("image").to_string();
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|err| AppError::BadRequest(err.to_string()))?
                    .to_vec();
                files.push(UploadFile { file_name, bytes });
            }
            "tags" => {
                let value = field
                    .text()
                    .await
                    .map_err(|err| AppError::BadRequest(err.to_string()))?;
                tags.extend(parse_tags(&value));
            }
            "fingerprint" | "guest_fingerprint" => {
                let value = field
                    .text()
                    .await
                    .map_err(|err| AppError::BadRequest(err.to_string()))?;
                let value = value.trim();
                if !value.is_empty() {
                    guest_fingerprint = Some(value.to_string());
                }
            }
            "captcha_token" | "turnstile_token" => {}
            _ => {}
        }
    }
    tags.sort();
    tags.dedup();
    Ok(UploadForm {
        files,
        tags,
        guest_fingerprint,
    })
}

fn parse_tags(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect()
}

pub async fn list_images(
    state: &AppState,
    user: Option<&CurrentUser>,
    query: &ImageQuery,
    admin: bool,
) -> AppResult<Page<ImageSummary>> {
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(30).clamp(1, 100);
    let offset = (page - 1) * page_size;
    let status = if admin {
        query.status.clone().unwrap_or_default()
    } else {
        query.status.clone().unwrap_or_else(|| "active".to_string())
    };
    let tag = query.tag.as_ref().map(|value| value.trim().to_string());
    let storage_provider_id = query.storage_provider_id;
    let user_id = query.user_id;
    let is_guest_upload = query.is_guest_upload;
    let rows = if admin {
        sqlx::query_as::<_, ImageRow>(
            "SELECT DISTINCT i.id,i.user_id,i.file_object_id,i.original_name,i.title,i.description,i.status,i.visibility,i.created_at,fo.sha256,fo.size,fo.mime_type AS _mime_type,fo.width,fo.height,fo.orientation,fo.aspect_ratio,fo.ref_count FROM images i JOIN file_objects fo ON fo.id=i.file_object_id LEFT JOIN image_tags it ON it.image_id=i.id LEFT JOIN tags t ON t.id=it.tag_id LEFT JOIN storage_objects so ON so.file_object_id=i.file_object_id WHERE ($1='' OR i.status=$1) AND ($2::text IS NULL OR fo.orientation=$2) AND ($3::int IS NULL OR fo.width >= $3) AND ($4::int IS NULL OR fo.height >= $4) AND ($5::text IS NULL OR t.name=$5 OR t.slug=$5) AND ($6::uuid IS NULL OR so.storage_provider_id=$6) AND ($7::uuid IS NULL OR i.user_id=$7) AND ($8::bool IS NULL OR i.is_guest_upload=$8) ORDER BY i.created_at DESC LIMIT $9 OFFSET $10",
        )
        .bind(&status)
        .bind(&query.orientation)
        .bind(query.min_width)
        .bind(query.min_height)
        .bind(&tag)
        .bind(storage_provider_id)
        .bind(user_id)
        .bind(is_guest_upload)
        .bind(page_size)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    } else {
        let current = user.ok_or_else(|| AppError::Unauthorized("login required".to_string()))?;
        sqlx::query_as::<_, ImageRow>(
            "SELECT DISTINCT i.id,i.user_id,i.file_object_id,i.original_name,i.title,i.description,i.status,i.visibility,i.created_at,fo.sha256,fo.size,fo.mime_type AS _mime_type,fo.width,fo.height,fo.orientation,fo.aspect_ratio,fo.ref_count FROM images i JOIN file_objects fo ON fo.id=i.file_object_id LEFT JOIN image_tags it ON it.image_id=i.id LEFT JOIN tags t ON t.id=it.tag_id LEFT JOIN storage_objects so ON so.file_object_id=i.file_object_id WHERE i.user_id=$1 AND ($2='' OR i.status=$2) AND ($3::text IS NULL OR fo.orientation=$3) AND ($4::int IS NULL OR fo.width >= $4) AND ($5::int IS NULL OR fo.height >= $5) AND ($6::text IS NULL OR t.name=$6 OR t.slug=$6) AND ($7::uuid IS NULL OR so.storage_provider_id=$7) ORDER BY i.created_at DESC LIMIT $8 OFFSET $9",
        )
        .bind(current.id)
        .bind(&status)
        .bind(&query.orientation)
        .bind(query.min_width)
        .bind(query.min_height)
        .bind(&tag)
        .bind(storage_provider_id)
        .bind(page_size)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    };
    let total: i64 = if admin {
        sqlx::query_scalar("SELECT COUNT(DISTINCT i.id) FROM images i JOIN file_objects fo ON fo.id=i.file_object_id LEFT JOIN image_tags it ON it.image_id=i.id LEFT JOIN tags t ON t.id=it.tag_id LEFT JOIN storage_objects so ON so.file_object_id=i.file_object_id WHERE ($1='' OR i.status=$1) AND ($2::text IS NULL OR fo.orientation=$2) AND ($3::int IS NULL OR fo.width >= $3) AND ($4::int IS NULL OR fo.height >= $4) AND ($5::text IS NULL OR t.name=$5 OR t.slug=$5) AND ($6::uuid IS NULL OR so.storage_provider_id=$6) AND ($7::uuid IS NULL OR i.user_id=$7) AND ($8::bool IS NULL OR i.is_guest_upload=$8)")
            .bind(&status)
            .bind(&query.orientation)
            .bind(query.min_width)
            .bind(query.min_height)
            .bind(&tag)
            .bind(storage_provider_id)
            .bind(user_id)
            .bind(is_guest_upload)
            .fetch_one(&state.pool)
            .await?
    } else {
        let current = user.ok_or_else(|| AppError::Unauthorized("login required".to_string()))?;
        sqlx::query_scalar("SELECT COUNT(DISTINCT i.id) FROM images i JOIN file_objects fo ON fo.id=i.file_object_id LEFT JOIN image_tags it ON it.image_id=i.id LEFT JOIN tags t ON t.id=it.tag_id LEFT JOIN storage_objects so ON so.file_object_id=i.file_object_id WHERE i.user_id=$1 AND ($2='' OR i.status=$2) AND ($3::text IS NULL OR fo.orientation=$3) AND ($4::int IS NULL OR fo.width >= $4) AND ($5::int IS NULL OR fo.height >= $5) AND ($6::text IS NULL OR t.name=$6 OR t.slug=$6) AND ($7::uuid IS NULL OR so.storage_provider_id=$7)")
            .bind(current.id)
            .bind(&status)
            .bind(&query.orientation)
            .bind(query.min_width)
            .bind(query.min_height)
            .bind(&tag)
            .bind(storage_provider_id)
            .fetch_one(&state.pool)
            .await?
    };
    let mut items = Vec::new();
    for row in rows {
        let image_id = row.id;
        items.push(summary_from_row(state, row, LinkContext::Authorized { image_id }).await?);
    }
    Ok(Page {
        items,
        page,
        page_size,
        total,
    })
}

pub async fn list_public_images(
    state: &AppState,
    query: &ImageQuery,
) -> AppResult<Page<ImageSummary>> {
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(30).clamp(1, 100);
    let offset = (page - 1) * page_size;
    let tag = query.tag.as_ref().map(|value| value.trim().to_string());
    let rows = sqlx::query_as::<_, ImageRow>(
        "SELECT DISTINCT i.id,i.user_id,i.file_object_id,i.original_name,i.title,i.description,i.status,i.visibility,i.created_at,fo.sha256,fo.size,fo.mime_type AS _mime_type,fo.width,fo.height,fo.orientation,fo.aspect_ratio,fo.ref_count FROM images i JOIN file_objects fo ON fo.id=i.file_object_id LEFT JOIN image_tags it ON it.image_id=i.id LEFT JOIN tags t ON t.id=it.tag_id WHERE i.status='active' AND i.visibility='public' AND ($1::text IS NULL OR fo.orientation=$1) AND ($2::int IS NULL OR fo.width >= $2) AND ($3::int IS NULL OR fo.height >= $3) AND ($4::text IS NULL OR t.name=$4 OR t.slug=$4) ORDER BY i.created_at DESC LIMIT $5 OFFSET $6",
    )
    .bind(&query.orientation)
    .bind(query.min_width)
    .bind(query.min_height)
    .bind(&tag)
    .bind(page_size)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(DISTINCT i.id) FROM images i JOIN file_objects fo ON fo.id=i.file_object_id LEFT JOIN image_tags it ON it.image_id=i.id LEFT JOIN tags t ON t.id=it.tag_id WHERE i.status='active' AND i.visibility='public' AND ($1::text IS NULL OR fo.orientation=$1) AND ($2::int IS NULL OR fo.width >= $2) AND ($3::int IS NULL OR fo.height >= $3) AND ($4::text IS NULL OR t.name=$4 OR t.slug=$4)")
        .bind(&query.orientation)
        .bind(query.min_width)
        .bind(query.min_height)
        .bind(&tag)
        .fetch_one(&state.pool)
        .await?;
    let mut items = Vec::new();
    for row in rows {
        items.push(summary_from_row(state, row, LinkContext::Public).await?);
    }
    Ok(Page {
        items,
        page,
        page_size,
        total,
    })
}

pub async fn admin_image_detail(
    state: &AppState,
    user: Option<&CurrentUser>,
    image_id: Uuid,
) -> AppResult<serde_json::Value> {
    let summary = get_image(state, user, image_id, true).await?;
    let storage_rows = sqlx::query_as::<_, AdminStorageObjectRow>(
        "SELECT
            so.id,
            so.storage_provider_id,
            sp.name AS storage_provider_name,
            sp.provider_type,
            so.object_type,
            so.object_key,
            so.public_url,
            so.provider_file_id,
            so.etag,
            so.size,
            so.status,
            so.created_at,
            so.updated_at
         FROM storage_objects so
         JOIN storage_providers sp ON sp.id=so.storage_provider_id
         WHERE so.file_object_id=(SELECT file_object_id FROM images WHERE id=$1)
         ORDER BY so.object_type, so.created_at",
    )
    .bind(summary.id)
    .fetch_all(&state.pool)
    .await?;
    let storage_objects = storage_rows
        .into_iter()
        .map(|row| row.into_json(state, summary.id))
        .collect::<AppResult<Vec<_>>>()?;
    Ok(serde_json::json!({
        "image": summary,
        "storage_objects": storage_objects
    }))
}

#[derive(sqlx::FromRow)]
struct AdminStorageObjectRow {
    id: Uuid,
    storage_provider_id: Uuid,
    storage_provider_name: String,
    provider_type: String,
    object_type: String,
    object_key: String,
    public_url: Option<String>,
    provider_file_id: Option<String>,
    etag: Option<String>,
    size: i64,
    status: String,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl AdminStorageObjectRow {
    fn into_json(self, state: &AppState, image_id: Uuid) -> AppResult<serde_json::Value> {
        let public_url = self
            .public_url
            .clone()
            .map(|url| normalize_public_url_for_base(&state.config.public_base_url, url));
        let mut url = storage_url(
            state,
            self.storage_provider_id,
            &self.object_key,
            self.public_url.clone(),
        );
        if self.object_type == "original" {
            url = append_file_token(state, image_id, &url)?;
        }
        Ok(serde_json::json!({
            "id": self.id,
            "storage_provider_id": self.storage_provider_id,
            "storage_provider_name": self.storage_provider_name,
            "provider_type": self.provider_type,
            "object_type": self.object_type,
            "object_key": self.object_key,
            "public_url": public_url,
            "url": url,
            "provider_file_id": self.provider_file_id,
            "etag": self.etag,
            "size": self.size,
            "status": self.status,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        }))
    }
}

pub async fn get_image(
    state: &AppState,
    user: Option<&CurrentUser>,
    image_id: Uuid,
    admin: bool,
) -> AppResult<ImageSummary> {
    let row = sqlx::query_as::<_, ImageRow>(
        "SELECT i.id,i.user_id,i.file_object_id,i.original_name,i.title,i.description,i.status,i.visibility,i.created_at,fo.sha256,fo.size,fo.mime_type AS _mime_type,fo.width,fo.height,fo.orientation,fo.aspect_ratio,fo.ref_count FROM images i JOIN file_objects fo ON fo.id=i.file_object_id WHERE i.id=$1",
    )
    .bind(image_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("image not found".to_string()))?;
    if !admin && user.map(|u| u.id) != Some(row.user_id) {
        return Err(AppError::Forbidden(
            "image belongs to another user".to_string(),
        ));
    }
    let image_id = row.id;
    summary_from_row(state, row, LinkContext::Authorized { image_id }).await
}

pub async fn trash_image(
    state: &AppState,
    user: &CurrentUser,
    image_id: Uuid,
    admin: bool,
) -> AppResult<()> {
    let owner: Option<Uuid> = sqlx::query_scalar("SELECT user_id FROM images WHERE id=$1")
        .bind(image_id)
        .fetch_optional(&state.pool)
        .await?;
    let owner = owner.ok_or_else(|| AppError::NotFound("image not found".to_string()))?;
    if !admin && owner != user.id {
        return Err(AppError::Forbidden(
            "image belongs to another user".to_string(),
        ));
    }
    sqlx::query("UPDATE images SET status='trashed', trashed_at=now(), restore_until=now()+interval '30 days', deleted_by=$2, updated_at=now() WHERE id=$1")
        .bind(image_id)
        .bind(user.id)
        .execute(&state.pool)
        .await?;
    Ok(())
}

pub async fn restore_image(
    state: &AppState,
    user: &CurrentUser,
    image_id: Uuid,
    admin: bool,
) -> AppResult<()> {
    let owner: Option<Uuid> = sqlx::query_scalar("SELECT user_id FROM images WHERE id=$1")
        .bind(image_id)
        .fetch_optional(&state.pool)
        .await?;
    let owner = owner.ok_or_else(|| AppError::NotFound("image not found".to_string()))?;
    if !admin && owner != user.id {
        return Err(AppError::Forbidden(
            "image belongs to another user".to_string(),
        ));
    }
    sqlx::query("UPDATE images SET status='active', trashed_at=NULL, deleted_by=NULL, updated_at=now() WHERE id=$1")
        .bind(image_id)
        .execute(&state.pool)
        .await?;
    Ok(())
}

pub async fn permanent_delete(
    state: &AppState,
    user: &CurrentUser,
    image_id: Uuid,
    admin: bool,
) -> AppResult<()> {
    permanent_delete_with_reason(
        state,
        Some(user),
        image_id,
        admin,
        "manual permanent delete",
    )
    .await
}

pub async fn permanent_delete_by_system(
    state: &AppState,
    image_id: Uuid,
    reason: &str,
) -> AppResult<()> {
    permanent_delete_with_reason(state, None, image_id, true, reason).await
}

pub async fn permanent_delete_with_reason(
    state: &AppState,
    user: Option<&CurrentUser>,
    image_id: Uuid,
    admin: bool,
    reason: &str,
) -> AppResult<()> {
    let mut tx = state.pool.begin().await?;
    let row: Option<(Uuid, Uuid, String)> =
        sqlx::query_as("SELECT user_id,file_object_id,status FROM images WHERE id=$1")
            .bind(image_id)
            .fetch_optional(&mut *tx)
            .await?;
    let (owner, file_object_id, status) =
        row.ok_or_else(|| AppError::NotFound("image not found".to_string()))?;
    if !admin && user.map(|user| user.id) != Some(owner) {
        return Err(AppError::Forbidden(
            "image belongs to another user".to_string(),
        ));
    }
    if status == "deleted" {
        tx.commit().await?;
        return Ok(());
    }
    let deleted_by = user.map(|user| user.id);
    sqlx::query("UPDATE images SET status='deleted', deleted_at=now(), delete_reason=$2, deleted_by=$3, updated_at=now() WHERE id=$1 AND status <> 'deleted'")
        .bind(image_id)
        .bind(reason)
        .bind(deleted_by)
        .execute(&mut *tx)
        .await?;
    let ref_count: i32 = sqlx::query_scalar("UPDATE file_objects SET ref_count=CASE WHEN EXISTS (SELECT 1 FROM images WHERE file_object_id=$1 AND status <> 'deleted') THEN GREATEST(ref_count-1,0) ELSE 0 END, updated_at=now() WHERE id=$1 RETURNING ref_count")
        .bind(file_object_id)
        .fetch_one(&mut *tx)
        .await?;
    let objects = if ref_count == 0 {
        let objects = sqlx::query_as::<_, (Uuid, Uuid, String)>(
            "SELECT id,storage_provider_id,object_key FROM storage_objects WHERE file_object_id=$1 AND status='active'",
        )
        .bind(file_object_id)
        .fetch_all(&mut *tx)
        .await?;
        sqlx::query("UPDATE storage_objects SET status='deleting', updated_at=now() WHERE file_object_id=$1")
            .bind(file_object_id)
            .execute(&mut *tx)
            .await?;
        objects
    } else {
        Vec::new()
    };
    tx.commit().await?;
    for object in objects {
        let provider_row = storage_registry::provider_by_id(state, object.1).await?;
        let provider = storage_registry::build_provider(state, &provider_row).await?;
        provider.delete_object(&object.2).await?;
        sqlx::query("UPDATE storage_objects SET status='deleted', updated_at=now() WHERE id=$1")
            .bind(object.0)
            .execute(&state.pool)
            .await?;
    }
    Ok(())
}

pub async fn links_for(
    state: &AppState,
    file_object_id: Uuid,
    original_name: &str,
    context: LinkContext,
) -> AppResult<ImageLinks> {
    let original: (Uuid, String, Option<String>) = sqlx::query_as("SELECT storage_provider_id,object_key,public_url FROM storage_objects WHERE file_object_id=$1 AND object_type='original' AND status='active' ORDER BY updated_at DESC, created_at DESC LIMIT 1")
        .bind(file_object_id)
        .fetch_one(&state.pool)
        .await?;
    let preview: Option<(Uuid, String, Option<String>)> = sqlx::query_as("SELECT storage_provider_id,object_key,public_url FROM storage_objects WHERE file_object_id=$1 AND object_type='preview' AND status='active' ORDER BY updated_at DESC, created_at DESC LIMIT 1")
        .bind(file_object_id)
        .fetch_optional(&state.pool)
        .await?;
    let url = storage_url(state, original.0, &original.1, original.2);
    let url = match context {
        LinkContext::Public => url,
        LinkContext::Authorized { image_id } => append_file_token(state, image_id, &url)?,
    };
    let preview_url = preview
        .map(|preview| storage_url(state, preview.0, &preview.1, preview.2))
        .unwrap_or_default();
    let escaped_name = html_escape(original_name);
    let (preview_markdown, preview_html) =
        preview_markup(original_name, &escaped_name, &preview_url);
    Ok(ImageLinks {
        markdown: format!("![{}]({})", original_name, url),
        html: format!("<img src=\"{}\" alt=\"{}\">", url, escaped_name),
        preview_markdown,
        preview_html,
        url,
        preview_url,
    })
}

fn preview_markup(original_name: &str, escaped_name: &str, preview_url: &str) -> (String, String) {
    if preview_url.is_empty() {
        (String::new(), String::new())
    } else {
        (
            format!("![{}]({})", original_name, preview_url),
            format!("<img src=\"{}\" alt=\"{}\">", preview_url, escaped_name),
        )
    }
}

pub async fn random_image(state: &AppState, query: &RandomQuery) -> AppResult<RandomImageResponse> {
    let candidates = sqlx::query_as::<_, ImageRow>(
        "SELECT i.id,i.user_id,i.file_object_id,i.original_name,i.title,i.description,i.status,i.visibility,i.created_at,fo.sha256,fo.size,fo.mime_type AS _mime_type,fo.width,fo.height,fo.orientation,fo.aspect_ratio,fo.ref_count FROM images i JOIN file_objects fo ON fo.id=i.file_object_id WHERE i.status='active' AND i.visibility='public' AND ($1::text IS NULL OR fo.orientation=$1) AND ($2::int IS NULL OR fo.width >= $2) AND ($3::int IS NULL OR fo.height >= $3) ORDER BY random() LIMIT 200",
    )
    .bind(&query.orientation)
    .bind(random_prefilter_min_width(query))
    .bind(random_prefilter_min_height(query))
    .fetch_all(&state.pool)
    .await?;
    let wanted_tags = requested_tags(query);
    let mut row = None;
    for candidate in candidates {
        if random_row_matches(state, &candidate, query, &wanted_tags).await {
            row = Some(candidate);
            break;
        }
    }
    let row = row.ok_or_else(|| AppError::NotFound("no matching image".to_string()))?;
    let links = links_for(
        state,
        row.file_object_id,
        &row.original_name,
        LinkContext::Public,
    )
    .await?;
    let tags = load_tags(state, row.id).await?;
    Ok(RandomImageResponse {
        id: row.id,
        url: links.url,
        preview_url: links.preview_url,
        width: row.width,
        height: row.height,
        orientation: row.orientation,
        ratio: row.aspect_ratio,
        tags,
        markdown: links.markdown,
        html: links.html,
        preview_markdown: links.preview_markdown,
        preview_html: links.preview_html,
    })
}

fn requested_tags(query: &RandomQuery) -> Vec<String> {
    let mut tags = Vec::new();
    if let Some(tag) = &query.tag {
        tags.push(tag.trim().to_string());
    }
    if let Some(values) = &query.tags {
        tags.extend(
            values
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string),
        );
    }
    tags
}

async fn random_row_matches(
    state: &AppState,
    row: &ImageRow,
    query: &RandomQuery,
    wanted_tags: &[String],
) -> bool {
    if !dimensions_match(row, query) {
        return false;
    }
    let ratio_match = query
        .ratio
        .as_ref()
        .map(|value| ratio_matches(&row.aspect_ratio, value, query))
        .unwrap_or(true);
    if !ratio_match {
        return false;
    }
    if wanted_tags.is_empty() {
        return true;
    }
    let existing = load_tags(state, row.id).await.unwrap_or_default();
    if query.mode.as_deref() == Some("all") {
        wanted_tags
            .iter()
            .all(|tag| existing.iter().any(|value| value == tag))
    } else {
        wanted_tags
            .iter()
            .any(|tag| existing.iter().any(|value| value == tag))
    }
}

fn random_prefilter_min_width(query: &RandomQuery) -> Option<i32> {
    random_prefilter_min(query.min_width, query.width, query)
}

fn random_prefilter_min_height(query: &RandomQuery) -> Option<i32> {
    random_prefilter_min(query.min_height, query.height, query)
}

fn random_prefilter_min(
    min: Option<i32>,
    exact_or_near: Option<i32>,
    query: &RandomQuery,
) -> Option<i32> {
    let from_exact = exact_or_near.map(|value| {
        if query.r#match.as_deref() == Some("near") {
            value.saturating_sub(dimension_tolerance(query))
        } else {
            value
        }
    });
    min.into_iter().chain(from_exact).max()
}

fn dimensions_match(row: &ImageRow, query: &RandomQuery) -> bool {
    if query.min_width.is_some_and(|min| row.width < min) {
        return false;
    }
    if query.min_height.is_some_and(|min| row.height < min) {
        return false;
    }
    if query
        .width
        .is_some_and(|width| !dimension_matches(row.width, width, query))
    {
        return false;
    }
    if query
        .height
        .is_some_and(|height| !dimension_matches(row.height, height, query))
    {
        return false;
    }
    true
}

fn dimension_matches(actual: i32, expected: i32, query: &RandomQuery) -> bool {
    match query.r#match.as_deref().unwrap_or("exact") {
        "gte" => actual >= expected,
        "near" => {
            i64::from(actual).saturating_sub(i64::from(expected)).abs()
                <= i64::from(dimension_tolerance(query))
        }
        _ => actual == expected,
    }
}

fn dimension_tolerance(query: &RandomQuery) -> i32 {
    query
        .tolerance
        .unwrap_or(100.0)
        .max(0.0)
        .min(i32::MAX as f32)
        .round() as i32
}

fn ratio_matches(actual: &str, expected: &str, query: &RandomQuery) -> bool {
    let Some(actual_value) = parse_ratio(actual) else {
        return false;
    };
    let Some(expected_value) = parse_ratio(expected) else {
        return false;
    };
    match query.r#match.as_deref().unwrap_or("exact") {
        "gte" => actual_value >= expected_value,
        "near" => {
            let tolerance = query.tolerance.unwrap_or(0.05).max(0.0);
            (actual_value - expected_value).abs() <= tolerance
        }
        _ => actual == expected,
    }
}

fn parse_ratio(value: &str) -> Option<f32> {
    let (left, right) = value.split_once(':')?;
    let width: f32 = left.parse().ok()?;
    let height: f32 = right.parse().ok()?;
    if height == 0.0 {
        None
    } else {
        Some(width / height)
    }
}

async fn summary_from_row(
    state: &AppState,
    row: ImageRow,
    context: LinkContext,
) -> AppResult<ImageSummary> {
    let links = links_for(state, row.file_object_id, &row.original_name, context).await?;
    let tags = load_tags(state, row.id).await?;
    Ok(ImageSummary {
        id: row.id,
        original_name: row.original_name,
        title: row.title,
        description: row.description,
        status: row.status,
        visibility: row.visibility,
        url: links.url,
        preview_url: links.preview_url,
        width: row.width,
        height: row.height,
        size: row.size,
        orientation: row.orientation,
        sha256: row.sha256,
        ref_count: row.ref_count,
        tags,
        created_at: row.created_at,
    })
}

async fn load_tags(state: &AppState, image_id: Uuid) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT t.name FROM tags t JOIN image_tags it ON it.tag_id=t.id WHERE it.image_id=$1 AND t.status='normal' ORDER BY t.name",
    )
    .bind(image_id)
    .fetch_all(&state.pool)
    .await?)
}

pub(crate) async fn load_tag_policy(state: &AppState) -> AppResult<TagPolicy> {
    Ok(load_upload_settings(state).await?.tag_policy)
}

pub(crate) fn normalize_tag_list(tags: &[String], policy: &TagPolicy) -> AppResult<Vec<String>> {
    let mut normalized = BTreeSet::new();
    for tag in tags {
        let tag = tag.trim();
        if tag.is_empty() {
            continue;
        }
        validate_tag_name(tag, policy)?;
        normalized.insert(tag.to_string());
    }
    if normalized.len() > policy.max_tags_per_image {
        return Err(AppError::BadRequest(
            "too many tags for this image".to_string(),
        ));
    }
    Ok(normalized.into_iter().collect())
}

pub(crate) fn validate_tag_name(tag: &str, policy: &TagPolicy) -> AppResult<()> {
    if tag.chars().count() > policy.max_tag_length {
        return Err(AppError::BadRequest("tag is too long".to_string()));
    }
    let normalized = tag.to_lowercase();
    if policy
        .sensitive_words
        .iter()
        .any(|word| !word.is_empty() && normalized.contains(word))
    {
        return Err(AppError::BadRequest(
            "tag contains sensitive word".to_string(),
        ));
    }
    Ok(())
}

pub(crate) async fn attach_tags(
    state: &AppState,
    image_id: Uuid,
    user_id: Uuid,
    role: &str,
    tags: &[String],
    allow_tag_create: bool,
    policy: &TagPolicy,
) -> AppResult<()> {
    let tags = normalize_tag_list(tags, policy)?;
    enforce_image_tag_limit(state, image_id, &tags, policy).await?;
    for tag in tags {
        let slug = slugify(&tag);
        let existing_tag: Option<(Uuid, String)> =
            sqlx::query_as("SELECT id,status FROM tags WHERE slug=$1")
                .bind(&slug)
                .fetch_optional(&state.pool)
                .await?;
        let tag_id = if let Some((tag_id, status)) = existing_tag {
            if matches!(status.as_str(), "normal" | "pending") {
                tag_id
            } else {
                return Err(AppError::BadRequest("tag is unavailable".to_string()));
            }
        } else if allow_tag_create || is_admin_role(role) {
            let status = if policy.review_required && !is_admin_role(role) {
                "pending"
            } else {
                "normal"
            };
            sqlx::query_scalar(
                "INSERT INTO tags (name,slug,created_by,status,usage_count) VALUES ($1,$2,$3,$4,0) RETURNING id",
            )
            .bind(&tag)
            .bind(&slug)
            .bind(user_id)
            .bind(status)
            .fetch_one(&state.pool)
            .await?
        } else {
            return Err(AppError::Forbidden(
                "tag creation is disabled for this user group".to_string(),
            ));
        };
        sqlx::query("INSERT INTO image_tags (image_id,tag_id,created_by) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING")
            .bind(image_id)
            .bind(tag_id)
            .bind(user_id)
            .execute(&state.pool)
            .await?;
        sqlx::query("UPDATE tags SET usage_count=(SELECT COUNT(*) FROM image_tags WHERE tag_id=$1), updated_at=now() WHERE id=$1")
            .bind(tag_id)
            .execute(&state.pool)
            .await?;
    }
    Ok(())
}

async fn enforce_image_tag_limit(
    state: &AppState,
    image_id: Uuid,
    tags: &[String],
    policy: &TagPolicy,
) -> AppResult<()> {
    let existing_slugs = sqlx::query_scalar::<_, String>(
        "SELECT t.slug FROM tags t JOIN image_tags it ON it.tag_id=t.id WHERE it.image_id=$1",
    )
    .bind(image_id)
    .fetch_all(&state.pool)
    .await?;
    let mut final_slugs = existing_slugs.into_iter().collect::<BTreeSet<_>>();
    final_slugs.extend(tags.iter().map(|tag| slugify(tag)));
    if final_slugs.len() > policy.max_tags_per_image {
        return Err(AppError::BadRequest(
            "too many tags for this image".to_string(),
        ));
    }
    Ok(())
}

fn is_admin_role(role: &str) -> bool {
    matches!(role, "admin" | "super_admin")
}

fn build_preview(image: &image::DynamicImage, settings: &UploadSettings) -> AppResult<Vec<u8>> {
    let resized = image
        .thumbnail(settings.webp_max_width, settings.webp_max_height)
        .to_rgba8();
    encode_webp_rgba(&resized, settings.webp_quality)
}

fn strip_image_metadata(
    image: &image::DynamicImage,
    original: &[u8],
    mime_type: &str,
) -> AppResult<Vec<u8>> {
    match mime_type {
        "image/jpeg" => encode_jpeg_without_metadata(image),
        "image/png" => encode_png_without_metadata(image),
        "image/webp" => encode_webp_without_metadata(image),
        _ => Ok(original.to_vec()),
    }
}

fn encode_jpeg_without_metadata(image: &image::DynamicImage) -> AppResult<Vec<u8>> {
    let rgb = image.to_rgb8();
    let mut output = Vec::new();
    JpegEncoder::new_with_quality(&mut output, 90)
        .write_image(&rgb, rgb.width(), rgb.height(), ExtendedColorType::Rgb8)
        .map_err(|err| AppError::BadRequest(err.to_string()))?;
    Ok(output)
}

fn encode_png_without_metadata(image: &image::DynamicImage) -> AppResult<Vec<u8>> {
    let rgba = image.to_rgba8();
    let mut output = Vec::new();
    PngEncoder::new_with_quality(&mut output, CompressionType::Default, FilterType::Adaptive)
        .write_image(&rgba, rgba.width(), rgba.height(), ExtendedColorType::Rgba8)
        .map_err(|err| AppError::BadRequest(err.to_string()))?;
    Ok(output)
}

fn encode_webp_without_metadata(image: &image::DynamicImage) -> AppResult<Vec<u8>> {
    let rgba = image.to_rgba8();
    encode_webp_rgba(&rgba, 90.0)
}

fn encode_webp_rgba(image: &image::RgbaImage, quality: f32) -> AppResult<Vec<u8>> {
    let quality = normalize_webp_quality(quality);
    let encoder = webp::Encoder::from_rgba(image.as_raw(), image.width(), image.height());
    Ok(encoder.encode(quality).to_vec())
}

fn normalize_webp_quality(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(1.0, 100.0)
    } else {
        75.0
    }
}

fn infer_mime(bytes: &[u8], file_name: &str, settings: &UploadSettings) -> AppResult<String> {
    validate_extension(file_name)?;
    let guessed = mime_guess::from_path(file_name)
        .first_raw()
        .unwrap_or("application/octet-stream");
    let mime = if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else {
        guessed
    };
    if !matches!(
        mime,
        "image/jpeg" | "image/png" | "image/gif" | "image/webp" | "image/avif"
    ) {
        return Err(AppError::FileType("unsupported image type".to_string()));
    }
    if !settings.allowed_mime_types.iter().any(|item| item == mime) {
        return Err(AppError::FileType("image type is not allowed".to_string()));
    }
    if !extension_matches_mime(file_name, mime) {
        return Err(AppError::FileType(
            "image extension does not match content type".to_string(),
        ));
    }
    Ok(mime.to_string())
}

fn validate_extension(file_name: &str) -> AppResult<()> {
    let Some(extension) = std::path::Path::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
    else {
        return Err(AppError::FileType(
            "image extension is required".to_string(),
        ));
    };
    if matches!(
        extension.as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "avif"
    ) {
        Ok(())
    } else {
        Err(AppError::FileType(
            "image extension is not allowed".to_string(),
        ))
    }
}

fn extension_matches_mime(file_name: &str, mime: &str) -> bool {
    let extension = std::path::Path::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        (extension.as_str(), mime),
        ("jpg" | "jpeg", "image/jpeg")
            | ("png", "image/png")
            | ("gif", "image/gif")
            | ("webp", "image/webp")
            | ("avif", "image/avif")
    )
}

fn extension_for(file_name: &str, mime: &str) -> String {
    std::path::Path::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_else(|| match mime {
            "image/jpeg" => "jpg".to_string(),
            "image/png" => "png".to_string(),
            "image/gif" => "gif".to_string(),
            "image/webp" => "webp".to_string(),
            "image/avif" => "avif".to_string(),
            _ => "img".to_string(),
        })
}

fn orientation(width: u32, height: u32) -> &'static str {
    match width.cmp(&height) {
        std::cmp::Ordering::Greater => "landscape",
        std::cmp::Ordering::Less => "portrait",
        std::cmp::Ordering::Equal => "square",
    }
}

fn ratio(width: u32, height: u32) -> String {
    if width == 0 || height == 0 {
        return String::new();
    }
    let gcd = gcd(width, height);
    format!("{}:{}", width / gcd, height / gcd)
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let tmp = b;
        b = a % b;
        a = tmp;
    }
    a
}

pub(crate) fn slugify(value: &str) -> String {
    let slug = value.trim().to_ascii_lowercase().replace(' ', "-");
    if slug.is_empty() {
        "tag".to_string()
    } else {
        slug
    }
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub(crate) fn storage_url(
    state: &AppState,
    provider_id: Uuid,
    object_key: &str,
    public_url: Option<String>,
) -> String {
    if let Some(public_url) = public_url {
        return normalize_public_url_for_base(&state.config.public_base_url, public_url);
    }
    join_public_base_or_path(
        &state.config.public_base_url,
        &format!(
            "/api/storage/proxy/{}/{}",
            provider_id,
            object_key.trim_start_matches('/')
        ),
    )
}

fn append_file_token(state: &AppState, image_id: Uuid, url: &str) -> AppResult<String> {
    let exp = Utc::now()
        .checked_add_signed(Duration::minutes(15))
        .ok_or_else(|| AppError::BadRequest("invalid expiration".to_string()))?
        .timestamp() as usize;
    let token = encode(
        &Header::default(),
        &TokenClaims {
            sub: image_id.to_string(),
            role: "file_read".to_string(),
            exp,
        },
        &EncodingKey::from_secret(state.config.session_secret.as_bytes()),
    )
    .map_err(|err| AppError::Unauthorized(err.to_string()))?;
    let separator = if url.contains('?') { '&' } else { '?' };
    Ok(format!(
        "{}{}token={}",
        url,
        separator,
        urlencoding::encode(&token)
    ))
}

fn normalize_public_url(value: String) -> String {
    let Some((scheme, rest)) = value.split_once("://") else {
        return value.replace("//", "/");
    };
    format!("{scheme}://{}", rest.replace("//", "/"))
}

pub(crate) fn normalize_public_url_for_base(public_base_url: &str, value: String) -> String {
    if value.starts_with("http://") || value.starts_with("https://") {
        normalize_same_site_public_url(public_base_url, &normalize_public_url(value))
    } else if value.starts_with('/') {
        join_public_base_or_path(public_base_url, &normalize_public_url(value))
    } else {
        normalize_public_url(value)
    }
}

fn normalize_same_site_public_url(public_base_url: &str, value: &str) -> String {
    let Some((scheme, rest)) = value.split_once("://") else {
        return value.to_string();
    };
    let Some((host, path)) = rest.split_once('/') else {
        return value.to_string();
    };
    let path = format!("/{path}");
    if is_local_public_host(host)
        && (path.starts_with("/files/") || path.starts_with("/api/storage/proxy/"))
    {
        return join_public_base_or_path(public_base_url, &path);
    }
    format!("{scheme}://{rest}")
}

fn join_public_base_or_path(public_base_url: &str, path: &str) -> String {
    let path = normalize_public_url(path.to_string());
    if is_local_public_base_url(public_base_url) {
        path
    } else {
        format!("{}{}", public_base_url.trim_end_matches('/'), path)
    }
}

fn is_local_public_base_url(public_base_url: &str) -> bool {
    let Some((_, rest)) = public_base_url.split_once("://") else {
        return public_base_url.trim().is_empty();
    };
    let host = rest.split('/').next().unwrap_or(rest);
    is_local_public_host(host)
}

fn is_local_public_host(host: &str) -> bool {
    let host = host.split('@').next_back().unwrap_or(host);
    let host = if let Some(value) = host.strip_prefix('[') {
        value.split(']').next().unwrap_or(value)
    } else {
        host.split(':').next().unwrap_or(host)
    };
    matches!(
        host.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "0.0.0.0" | "::1"
    )
}

async fn load_upload_settings(state: &AppState) -> AppResult<UploadSettings> {
    let value: serde_json::Value =
        sqlx::query_scalar("SELECT value_json FROM site_settings WHERE key='upload'")
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or_else(|| serde_json::json!({}));
    let site_value: serde_json::Value =
        sqlx::query_scalar("SELECT value_json FROM site_settings WHERE key='site'")
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or_else(|| serde_json::json!({}));
    let allowed_mime_types = value
        .get("allowed_mime_types")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| {
            vec![
                "image/jpeg".to_string(),
                "image/png".to_string(),
                "image/gif".to_string(),
                "image/webp".to_string(),
                "image/avif".to_string(),
            ]
        });
    Ok(UploadSettings {
        allowed_mime_types,
        remove_exif: value
            .get("remove_exif")
            .or_else(|| value.get("strip_exif"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        webp_enabled: value
            .get("webp_enabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        webp_max_width: value
            .get("webp_max_width")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(512)
            .clamp(64, 4096),
        webp_max_height: value
            .get("webp_max_height")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(512)
            .clamp(64, 4096),
        webp_quality: value
            .get("webp_quality")
            .and_then(serde_json::Value::as_f64)
            .map(|value| value as f32)
            .map(normalize_webp_quality)
            .unwrap_or(75.0),
        guest_ip_daily_limit: value
            .get("guest_ip_daily_limit")
            .or_else(|| value.get("guest_daily_ip_limit"))
            .or_else(|| value.get("guest_upload_daily_ip_limit"))
            .or_else(|| site_value.get("guest_ip_daily_limit"))
            .or_else(|| site_value.get("guest_daily_ip_limit"))
            .or_else(|| site_value.get("guest_upload_daily_ip_limit"))
            .and_then(serde_json::Value::as_i64),
        guest_review_strategy: guest_review_strategy_from_settings(&value, &site_value),
        tag_policy: tag_policy_from_upload_settings(&value),
    })
}

fn guest_review_strategy_from_settings(
    value: &serde_json::Value,
    fallback: &serde_json::Value,
) -> GuestReviewStrategy {
    value
        .get("guest_review_strategy")
        .or_else(|| value.get("guest_audit_strategy"))
        .or_else(|| value.get("guest_upload_review_strategy"))
        .or_else(|| fallback.get("guest_review_strategy"))
        .or_else(|| fallback.get("guest_audit_strategy"))
        .or_else(|| fallback.get("guest_upload_review_strategy"))
        .and_then(serde_json::Value::as_str)
        .map(normalize_guest_review_strategy)
        .unwrap_or(GuestReviewStrategy::ManualRequired)
}

fn normalize_guest_review_strategy(value: &str) -> GuestReviewStrategy {
    match value.trim().to_ascii_lowercase().as_str() {
        "reject" | "rejected" | "deny" | "block" | "拒绝" => GuestReviewStrategy::Reject,
        "auto" | "pass" | "allow" | "ai" | "ai_only" | "fastapi" | "自动" | "放行" => {
            GuestReviewStrategy::Auto
        }
        "group" | "quota" | "default" | "user_group" | "按用户组" | "默认" => {
            GuestReviewStrategy::GroupPolicy
        }
        _ => GuestReviewStrategy::ManualRequired,
    }
}

fn upload_requires_manual_review(
    actor: &UploadActor,
    quota_row: &crate::models::QuotaRow,
    settings: &UploadSettings,
) -> AppResult<bool> {
    if !actor.is_guest {
        return Ok(quota_row.require_review);
    }
    match settings.guest_review_strategy {
        GuestReviewStrategy::ManualRequired => Ok(true),
        GuestReviewStrategy::Reject => Err(AppError::Forbidden(
            "guest upload review strategy rejects uploads".to_string(),
        )),
        GuestReviewStrategy::Auto => Ok(false),
        GuestReviewStrategy::GroupPolicy => Ok(quota_row.require_review),
    }
}

fn tag_policy_from_upload_settings(value: &serde_json::Value) -> TagPolicy {
    let max_tags_per_image = value
        .get("max_tags_per_image")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(10)
        .clamp(1, 50);
    let max_tag_length = value
        .get("max_tag_length")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(32)
        .clamp(1, 128);
    let sensitive_words = value
        .get("tag_sensitive_words")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_lowercase)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let review_required = value
        .get("tag_review_required")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    TagPolicy {
        max_tags_per_image,
        max_tag_length,
        sensitive_words,
        review_required,
    }
}

async fn enforce_guest_ip_limit(
    state: &AppState,
    actor: &UploadActor,
    settings: &UploadSettings,
    incoming_files: i64,
) -> AppResult<()> {
    let Some(limit) = settings.guest_ip_daily_limit else {
        return Ok(());
    };
    let Some(ip) = actor.guest_ip.as_deref() else {
        return Ok(());
    };
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM images WHERE is_guest_upload=true AND guest_ip=$1 AND created_at >= date_trunc('day', now())",
    )
    .bind(ip)
    .fetch_one(&state.pool)
    .await?;
    if count + incoming_files > limit {
        Err(AppError::Quota(
            "guest upload ip daily limit exceeded".to_string(),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{DecodingKey, Validation, decode};

    fn quota_row() -> crate::models::QuotaRow {
        crate::models::QuotaRow {
            group_code: "normal".to_string(),
            daily_upload_count: 10,
            daily_upload_bytes: 1000,
            max_file_size: 100,
            total_storage_bytes: 5000,
            daily_api_calls: 50,
            daily_random_calls: 60,
            require_review: true,
            require_captcha: false,
            allow_batch_upload: true,
            allow_tag_create: true,
            default_storage_provider_id: None,
            used_count_today: 0,
            used_bytes_today: 0,
        }
    }

    fn upload_settings_for_tests() -> UploadSettings {
        UploadSettings {
            allowed_mime_types: vec!["image/png".to_string()],
            remove_exif: true,
            webp_enabled: true,
            webp_max_width: 512,
            webp_max_height: 512,
            webp_quality: 75.0,
            guest_ip_daily_limit: None,
            guest_review_strategy: GuestReviewStrategy::ManualRequired,
            tag_policy: TagPolicy {
                max_tags_per_image: 10,
                max_tag_length: 32,
                sensitive_words: Vec::new(),
                review_required: false,
            },
        }
    }

    #[test]
    fn orientation_and_ratio_match_expected_shapes() {
        assert_eq!(orientation(1920, 1080), "landscape");
        assert_eq!(orientation(900, 1200), "portrait");
        assert_eq!(orientation(512, 512), "square");
        assert_eq!(ratio(1920, 1080), "16:9");
        assert_eq!(ratio(1000, 1000), "1:1");
        assert_eq!(ratio(0, 1000), "");
    }

    #[test]
    fn slug_and_html_helpers_are_stable() {
        assert_eq!(slugify("Summer Sky"), "summer-sky");
        assert_eq!(slugify("  "), "tag");
        assert_eq!(
            html_escape("<img alt=\"a&b\">"),
            "&lt;img alt=&quot;a&amp;b&quot;&gt;"
        );
    }

    #[test]
    fn local_public_urls_are_rebased_to_current_public_base_url() {
        let base_url = "https://img.example.com";
        assert_eq!(
            normalize_public_url_for_base(
                base_url,
                "http://localhost:8080/files/2026/06/a.webp".to_string()
            ),
            "https://img.example.com/files/2026/06/a.webp"
        );
        assert_eq!(
            normalize_public_url_for_base(
                base_url,
                "http://127.0.0.1:8080/api/storage/proxy/provider/a.webp".to_string()
            ),
            "https://img.example.com/api/storage/proxy/provider/a.webp"
        );
        assert_eq!(
            normalize_public_url_for_base(
                base_url,
                "http://0.0.0.0:8080/files/2026/06/a.webp".to_string()
            ),
            "https://img.example.com/files/2026/06/a.webp"
        );
        assert_eq!(
            normalize_public_url_for_base(
                base_url,
                "http://[::1]:8080/api/storage/proxy/provider/a.webp".to_string()
            ),
            "https://img.example.com/api/storage/proxy/provider/a.webp"
        );
        assert_eq!(
            normalize_public_url_for_base(base_url, "/files/2026/06/a.webp".to_string()),
            "https://img.example.com/files/2026/06/a.webp"
        );
        assert_eq!(
            normalize_public_url_for_base(
                base_url,
                "https://cdn.example.com/files/2026/06/a.webp".to_string()
            ),
            "https://cdn.example.com/files/2026/06/a.webp"
        );
    }

    #[test]
    fn local_public_urls_are_returned_as_same_origin_paths_with_local_base_url() {
        let base_url = "http://localhost:8080";
        assert_eq!(
            normalize_public_url_for_base(
                base_url,
                "http://localhost:8080/files/2026/06/a.webp".to_string()
            ),
            "/files/2026/06/a.webp"
        );
        assert_eq!(
            normalize_public_url_for_base(
                base_url,
                "http://127.0.0.1:8080/api/storage/proxy/provider/a.webp".to_string()
            ),
            "/api/storage/proxy/provider/a.webp"
        );
        assert_eq!(
            normalize_public_url_for_base(base_url, "/files/2026/06/a.webp".to_string()),
            "/files/2026/06/a.webp"
        );
        assert_eq!(
            normalize_public_url_for_base(
                base_url,
                "https://cdn.example.com/files/2026/06/a.webp".to_string()
            ),
            "https://cdn.example.com/files/2026/06/a.webp"
        );
    }

    #[tokio::test]
    async fn authorized_file_urls_get_decodable_file_token() {
        let config = crate::app::AppConfig {
            host: "0.0.0.0".to_string(),
            port: 8080,
            database_url: "postgres://example".to_string(),
            public_base_url: "https://img.example.com".to_string(),
            session_secret: "test-secret".to_string(),
            encryption_key: "encryption".to_string(),
            local_storage_root: "/tmp".to_string(),
            local_storage_public_prefix: "/files".to_string(),
            ai_service_url: "http://127.0.0.1:8080".to_string(),
            initial_admin_email: "admin@example.com".to_string(),
            initial_admin_username: "admin".to_string(),
            initial_admin_password: "password".to_string(),
            ..Default::default()
        };
        let pool = sqlx::PgPool::connect_lazy("postgres://example").expect("lazy pool");
        let state = crate::app::AppState::new(pool, config);
        let image_id = Uuid::from_u128(42);

        let url = append_file_token(&state, image_id, "/files/a.jpg?download=1").unwrap();
        let token = url
            .split("token=")
            .nth(1)
            .and_then(|value| urlencoding::decode(value).ok())
            .expect("token");
        let claims = decode::<TokenClaims>(
            &token,
            &DecodingKey::from_secret(state.config.session_secret.as_bytes()),
            &Validation::default(),
        )
        .expect("decode token")
        .claims;

        assert!(url.contains("&token="));
        assert_eq!(claims.sub, image_id.to_string());
        assert_eq!(claims.role, "file_read");
    }

    #[test]
    fn empty_preview_url_has_empty_markup() {
        let (preview_markdown, preview_html) = preview_markup("a.png", "a.png", "");

        assert!(preview_markdown.is_empty());
        assert!(preview_html.is_empty());
    }

    #[test]
    fn extension_policy_requires_allowed_matching_image_extension() {
        assert!(validate_extension("photo.jpg").is_ok());
        assert!(validate_extension("archive.zip").is_err());
        assert!(validate_extension("image").is_err());
        assert!(extension_matches_mime("photo.jpeg", "image/jpeg"));
        assert!(extension_matches_mime("preview.webp", "image/webp"));
        assert!(!extension_matches_mime("photo.jpg", "image/png"));
    }

    #[test]
    fn tag_policy_normalizes_and_rejects_invalid_tags() {
        let policy = TagPolicy {
            max_tags_per_image: 2,
            max_tag_length: 4,
            sensitive_words: vec!["bad".to_string()],
            review_required: true,
        };

        assert_eq!(
            normalize_tag_list(
                &[" 风景 ".to_string(), "风景".to_string(), "壁纸".to_string()],
                &policy
            )
            .unwrap(),
            vec!["壁纸".to_string(), "风景".to_string()]
        );
        assert!(matches!(
            normalize_tag_list(&["长长长长长".to_string()], &policy),
            Err(AppError::BadRequest(_))
        ));
        assert!(matches!(
            normalize_tag_list(&["bad-tag".to_string()], &policy),
            Err(AppError::BadRequest(_))
        ));
        assert!(matches!(
            normalize_tag_list(
                &["a".to_string(), "b".to_string(), "c".to_string()],
                &policy
            ),
            Err(AppError::BadRequest(_))
        ));
        assert!(policy.tag_review_required());
    }

    #[test]
    fn upload_settings_parse_tag_policy_defaults() {
        let policy = tag_policy_from_upload_settings(&serde_json::json!({
            "max_tags_per_image": 0,
            "max_tag_length": 512,
            "tag_sensitive_words": [" Secret ", ""],
            "tag_review_required": true
        }));

        assert_eq!(policy.max_tags_per_image, 1);
        assert_eq!(policy.max_tag_length, 128);
        assert_eq!(policy.sensitive_words, vec!["secret"]);
        assert!(policy.tag_review_required());
    }

    #[test]
    fn guest_review_strategy_defaults_to_manual_and_accepts_aliases() {
        assert_eq!(
            guest_review_strategy_from_settings(&serde_json::json!({}), &serde_json::json!({})),
            GuestReviewStrategy::ManualRequired
        );
        assert_eq!(
            guest_review_strategy_from_settings(
                &serde_json::json!({
                    "guest_review_strategy": "自动"
                }),
                &serde_json::json!({"guest_review_strategy":"manual_required"})
            ),
            GuestReviewStrategy::Auto
        );
        assert_eq!(
            guest_review_strategy_from_settings(
                &serde_json::json!({}),
                &serde_json::json!({
                    "guest_audit_strategy": "group"
                })
            ),
            GuestReviewStrategy::GroupPolicy
        );
        assert_eq!(
            guest_review_strategy_from_settings(
                &serde_json::json!({
                    "guest_upload_review_strategy": "拒绝"
                }),
                &serde_json::json!({})
            ),
            GuestReviewStrategy::Reject
        );
    }

    #[test]
    fn guest_review_strategy_controls_manual_review_requirement() {
        let actor = UploadActor {
            user_id: Uuid::nil(),
            role: "guest_account".to_string(),
            is_guest: true,
            guest_ip: None,
            guest_user_agent: None,
            guest_fingerprint: None,
        };
        let mut quota = quota_row();
        quota.require_review = false;
        let mut settings = upload_settings_for_tests();

        settings.guest_review_strategy = GuestReviewStrategy::ManualRequired;
        assert!(upload_requires_manual_review(&actor, &quota, &settings).unwrap());

        settings.guest_review_strategy = GuestReviewStrategy::Auto;
        assert!(!upload_requires_manual_review(&actor, &quota, &settings).unwrap());

        settings.guest_review_strategy = GuestReviewStrategy::GroupPolicy;
        assert!(!upload_requires_manual_review(&actor, &quota, &settings).unwrap());

        settings.guest_review_strategy = GuestReviewStrategy::Reject;
        assert!(matches!(
            upload_requires_manual_review(&actor, &quota, &settings),
            Err(AppError::Forbidden(_))
        ));
    }

    #[test]
    fn default_upload_visibility_keeps_member_uploads_private() {
        let member = UploadActor {
            user_id: Uuid::nil(),
            role: "user".to_string(),
            is_guest: false,
            guest_ip: None,
            guest_user_agent: None,
            guest_fingerprint: None,
        };
        let guest = UploadActor {
            user_id: Uuid::nil(),
            role: "guest_account".to_string(),
            is_guest: true,
            guest_ip: None,
            guest_user_agent: None,
            guest_fingerprint: None,
        };

        assert_eq!(default_upload_visibility(&member), "private");
        assert_eq!(default_upload_visibility(&guest), "public");
    }

    #[test]
    fn metadata_stripping_reencodes_supported_static_images() {
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2,
            2,
            image::Rgba([12, 34, 56, 255]),
        ));
        let jpeg = strip_image_metadata(&image, b"not-a-real-jpeg", "image/jpeg").unwrap();
        let png = strip_image_metadata(&image, b"not-a-real-png", "image/png").unwrap();

        assert_ne!(jpeg, b"not-a-real-jpeg");
        assert_ne!(png, b"not-a-real-png");
        assert!(image::load_from_memory(&jpeg).is_ok());
        assert!(image::load_from_memory(&png).is_ok());
        assert_eq!(
            strip_image_metadata(&image, b"gif-bytes", "image/gif").unwrap(),
            b"gif-bytes"
        );
    }

    #[test]
    fn webp_preview_quality_is_bounded_and_affects_encoding() {
        let image = image::RgbaImage::from_fn(48, 48, |x, y| {
            image::Rgba([
                ((x * 5) % 255) as u8,
                ((y * 7) % 255) as u8,
                (((x + y) * 3) % 255) as u8,
                255,
            ])
        });
        let low = encode_webp_rgba(&image, 20.0).unwrap();
        let high = encode_webp_rgba(&image, 90.0).unwrap();

        assert_eq!(normalize_webp_quality(-10.0), 1.0);
        assert_eq!(normalize_webp_quality(999.0), 100.0);
        assert_eq!(normalize_webp_quality(f32::NAN), 75.0);
        assert_ne!(low, high);
        assert!(image::load_from_memory(&low).is_ok());
        assert!(image::load_from_memory(&high).is_ok());
    }

    #[test]
    fn random_dimension_matching_supports_exact_gte_and_near() {
        let mut query = RandomQuery {
            tag: None,
            tags: None,
            mode: None,
            orientation: None,
            min_width: None,
            min_height: None,
            width: Some(1920),
            height: Some(1080),
            ratio: None,
            r#match: None,
            tolerance: None,
            r#type: None,
            image: None,
        };

        assert!(dimension_matches(1920, 1920, &query));
        assert!(!dimension_matches(2000, 1920, &query));

        query.r#match = Some("gte".to_string());
        assert!(dimension_matches(2000, 1920, &query));
        assert!(!dimension_matches(1800, 1920, &query));

        query.r#match = Some("near".to_string());
        query.tolerance = Some(100.0);
        assert!(dimension_matches(2000, 1920, &query));
        assert!(dimension_matches(1820, 1920, &query));
        assert!(!dimension_matches(1819, 1920, &query));
        assert_eq!(random_prefilter_min_width(&query), Some(1820));
        assert_eq!(random_prefilter_min_height(&query), Some(980));
    }

    #[test]
    fn random_ratio_matching_supports_modes() {
        let mut query = RandomQuery {
            tag: None,
            tags: None,
            mode: None,
            orientation: None,
            min_width: None,
            min_height: None,
            width: None,
            height: None,
            ratio: None,
            r#match: None,
            tolerance: None,
            r#type: None,
            image: None,
        };

        assert!(ratio_matches("16:9", "16:9", &query));
        assert!(!ratio_matches("4:3", "16:9", &query));

        query.r#match = Some("gte".to_string());
        assert!(ratio_matches("16:9", "4:3", &query));
        assert!(!ratio_matches("1:1", "16:9", &query));

        query.r#match = Some("near".to_string());
        query.tolerance = Some(0.05);
        assert!(ratio_matches("177:100", "16:9", &query));
        assert!(!ratio_matches("4:3", "16:9", &query));
    }

    #[tokio::test]
    async fn storage_proxy_url_uses_provider_and_public_base() {
        let config = crate::app::AppConfig {
            host: "0.0.0.0".to_string(),
            port: 8080,
            database_url: "postgres://example".to_string(),
            public_base_url: "https://img.example.com/".to_string(),
            session_secret: "secret".to_string(),
            encryption_key: "encryption".to_string(),
            local_storage_root: "/tmp".to_string(),
            local_storage_public_prefix: "/files".to_string(),
            ai_service_url: "http://127.0.0.1:8080".to_string(),
            initial_admin_email: "admin@example.com".to_string(),
            initial_admin_username: "admin".to_string(),
            initial_admin_password: "password".to_string(),
            ..Default::default()
        };
        let pool = sqlx::PgPool::connect_lazy("postgres://example").expect("lazy pool");
        let state = crate::app::AppState::new(pool, config);
        let provider_id = Uuid::nil();

        assert_eq!(
            storage_url(&state, provider_id, "images/a.jpg", None),
            "https://img.example.com/api/storage/proxy/00000000-0000-0000-0000-000000000000/images/a.jpg"
        );
        assert_eq!(
            storage_url(
                &state,
                provider_id,
                "images/a.jpg",
                Some("https://cdn/a.jpg".to_string())
            ),
            "https://cdn/a.jpg"
        );
        assert_eq!(
            storage_url(
                &state,
                provider_id,
                "images/a.jpg",
                Some("https://cdn.example.com//files//images/a.jpg".to_string())
            ),
            "https://cdn.example.com/files/images/a.jpg"
        );
        assert_eq!(
            storage_url(
                &state,
                provider_id,
                "images/a.jpg",
                Some("/api/storage/proxy/id/images/a.jpg".to_string())
            ),
            "https://img.example.com/api/storage/proxy/id/images/a.jpg"
        );
        assert_eq!(
            storage_url(
                &state,
                provider_id,
                "previews/a.webp",
                Some("http://localhost:8080/files/previews/a.webp".to_string())
            ),
            "https://img.example.com/files/previews/a.webp"
        );
        assert_eq!(
            storage_url(
                &state,
                provider_id,
                "previews/a.webp",
                Some("http://127.0.0.1:8080/api/storage/proxy/id/previews/a.webp".to_string())
            ),
            "https://img.example.com/api/storage/proxy/id/previews/a.webp"
        );
        assert_eq!(
            storage_url(
                &state,
                provider_id,
                "previews/a.webp",
                Some("http://0.0.0.0:8080/files/previews/a.webp".to_string())
            ),
            "https://img.example.com/files/previews/a.webp"
        );
        assert_eq!(
            storage_url(
                &state,
                provider_id,
                "previews/a.webp",
                Some("http://[::1]:8080/api/storage/proxy/id/previews/a.webp".to_string())
            ),
            "https://img.example.com/api/storage/proxy/id/previews/a.webp"
        );
        assert_eq!(
            storage_url(
                &state,
                provider_id,
                "previews/a.webp",
                Some("https://cdn.example.com/files/previews/a.webp".to_string())
            ),
            "https://cdn.example.com/files/previews/a.webp"
        );
    }

    #[tokio::test]
    async fn storage_proxy_url_uses_same_origin_path_with_local_public_base() {
        let config = crate::app::AppConfig {
            host: "0.0.0.0".to_string(),
            port: 8080,
            database_url: "postgres://example".to_string(),
            public_base_url: "http://localhost:8080".to_string(),
            session_secret: "secret".to_string(),
            encryption_key: "encryption".to_string(),
            local_storage_root: "/tmp".to_string(),
            local_storage_public_prefix: "/files".to_string(),
            ai_service_url: "http://127.0.0.1:8080".to_string(),
            initial_admin_email: "admin@example.com".to_string(),
            initial_admin_username: "admin".to_string(),
            initial_admin_password: "password".to_string(),
            ..Default::default()
        };
        let pool = sqlx::PgPool::connect_lazy("postgres://example").expect("lazy pool");
        let state = crate::app::AppState::new(pool, config);
        let provider_id = Uuid::nil();

        assert_eq!(
            storage_url(&state, provider_id, "images/a.jpg", None),
            "/api/storage/proxy/00000000-0000-0000-0000-000000000000/images/a.jpg"
        );
        assert_eq!(
            storage_url(
                &state,
                provider_id,
                "previews/a.webp",
                Some("http://localhost:8080/files/previews/a.webp".to_string())
            ),
            "/files/previews/a.webp"
        );
    }
}
