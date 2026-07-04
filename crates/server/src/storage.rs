use crate::app::AppConfig;
use crate::error::{AppError, AppResult};
use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_config::Region;
use aws_credential_types::Credentials;
use aws_sdk_s3::Client as S3Client;
use aws_sdk_s3::config::Builder as S3ConfigBuilder;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use aws_smithy_runtime_api::client::result::SdkError;
use aws_smithy_types::error::metadata::ProvideErrorMetadata;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{Datelike, Duration, SecondsFormat, Utc};
use reqwest::header::{
    AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, DATE, HOST, HeaderMap, HeaderName, HeaderValue,
};
use reqwest::{Client, Method, Response, StatusCode};
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::{RsaPrivateKey, sha2::Digest, sha2::Sha256};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration as StdDuration;
use tokio::fs;

pub struct StoredObject {
    pub object_key: String,
    pub public_url: Option<String>,
    pub size: i64,
    pub etag: Option<String>,
}

#[async_trait]
#[allow(dead_code)]
pub trait StorageProvider: Send + Sync {
    async fn put_object(
        &self,
        object_key: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> AppResult<StoredObject>;
    async fn get_object(&self, object_key: &str) -> AppResult<Vec<u8>>;
    async fn delete_object(&self, object_key: &str) -> AppResult<()>;
    async fn head_object(&self, object_key: &str) -> AppResult<bool>;
    async fn get_public_url(&self, object_key: &str) -> AppResult<String>;
    async fn create_presigned_url(&self, object_key: &str) -> AppResult<String>;
    async fn health_check(&self) -> AppResult<()>;
    async fn refresh_auth(&self) -> AppResult<()>;
}

pub struct LocalStorageProvider {
    root: PathBuf,
    public_base_url: String,
    public_prefix: String,
    path_prefix: String,
}

impl LocalStorageProvider {
    pub fn from_config(config: &AppConfig, provider_config: &Value) -> Self {
        let root = config_optional_string(provider_config, "root")
            .unwrap_or_else(|| config.local_storage_root.clone());
        let public_prefix = config_optional_string(provider_config, "public_prefix")
            .unwrap_or_else(|| config.local_storage_public_prefix.clone());
        Self {
            root: PathBuf::from(&root),
            public_base_url: config.public_base_url.trim_end_matches('/').to_string(),
            public_prefix: public_prefix.trim_end_matches('/').to_string(),
            path_prefix: config_optional_string(provider_config, "path_prefix")
                .unwrap_or_default()
                .trim_matches('/')
                .to_string(),
        }
    }

    fn path_for(&self, object_key: &str) -> AppResult<PathBuf> {
        let storage_key = apply_path_prefix(&self.path_prefix, object_key);
        let path = Path::new(&storage_key);
        if path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(AppError::BadRequest("invalid object key".to_string()));
        }
        Ok(self.root.join(path))
    }
}

#[async_trait]
impl StorageProvider for LocalStorageProvider {
    async fn put_object(
        &self,
        object_key: &str,
        bytes: &[u8],
        _content_type: &str,
    ) -> AppResult<StoredObject> {
        let path = self.path_for(object_key)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&path, bytes).await?;
        Ok(StoredObject {
            object_key: object_key.to_string(),
            public_url: Some(self.get_public_url(object_key).await?),
            size: bytes.len() as i64,
            etag: None,
        })
    }

    async fn get_object(&self, object_key: &str) -> AppResult<Vec<u8>> {
        Ok(fs::read(self.path_for(object_key)?).await?)
    }

    async fn delete_object(&self, object_key: &str) -> AppResult<()> {
        let path = self.path_for(object_key)?;
        if fs::try_exists(&path).await? {
            fs::remove_file(path).await?;
        }
        Ok(())
    }

    async fn head_object(&self, object_key: &str) -> AppResult<bool> {
        Ok(fs::try_exists(self.path_for(object_key)?).await?)
    }

    async fn get_public_url(&self, object_key: &str) -> AppResult<String> {
        let storage_key = apply_path_prefix(&self.path_prefix, object_key);
        Ok(public_url(
            &self.public_base_url,
            &self.public_prefix,
            &storage_key,
        ))
    }

    async fn create_presigned_url(&self, object_key: &str) -> AppResult<String> {
        self.get_public_url(object_key).await
    }

    async fn health_check(&self) -> AppResult<()> {
        fs::create_dir_all(&self.root).await?;
        Ok(())
    }

    async fn refresh_auth(&self) -> AppResult<()> {
        Ok(())
    }
}

pub struct S3CompatibleProvider {
    provider_type: String,
    client: S3Client,
    bucket: String,
    provider_id: uuid::Uuid,
    public_domain: Option<String>,
    path_prefix: String,
    presigned_url_ttl_seconds: i64,
}

pub struct OneDriveProvider {
    client: Client,
    config: Value,
    provider_id: uuid::Uuid,
}

pub struct OracleOciNativeProvider {
    client: Client,
    config: Value,
    provider_id: uuid::Uuid,
    private_key: RsaPrivateKey,
}

impl S3CompatibleProvider {
    pub async fn from_config(
        provider_type: String,
        config: Value,
        provider_id: uuid::Uuid,
    ) -> AppResult<Self> {
        require_fields(&config, &["bucket", "access_key_id", "secret_access_key"])?;
        let endpoint = s3_endpoint(&provider_type, &config)?;
        let region = s3_region(&provider_type, &config)?;
        let bucket = config_string(&config, "bucket")?;
        let access_key_id = config_string(&config, "access_key_id")?;
        let secret_access_key = config_string(&config, "secret_access_key")?;
        let session_token = config_optional_string(&config, "session_token");
        let credentials = Credentials::new(
            access_key_id,
            secret_access_key,
            session_token,
            None,
            "tide-storage-provider",
        );
        let shared = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(region))
            .credentials_provider(credentials)
            .load()
            .await;
        let mut builder = S3ConfigBuilder::from(&shared).endpoint_url(endpoint);
        if s3_force_path_style(&provider_type, &config) {
            builder = builder.force_path_style(true);
        }
        let conf = builder.build();
        Ok(Self {
            provider_type,
            client: S3Client::from_conf(conf),
            bucket,
            provider_id,
            public_domain: config
                .get("public_domain")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToString::to_string),
            path_prefix: config_optional_string(&config, "path_prefix")
                .unwrap_or_default()
                .trim_matches('/')
                .to_string(),
            presigned_url_ttl_seconds: config_i64(&config, "presigned_url_ttl_seconds")
                .unwrap_or(3600)
                .clamp(60, 604800),
        })
    }
}

impl OneDriveProvider {
    pub fn new(config: Value, provider_id: uuid::Uuid) -> Self {
        Self {
            client: Client::new(),
            config,
            provider_id,
        }
    }

    async fn token(&self) -> AppResult<String> {
        require_fields(
            &self.config,
            &["client_id", "tenant_id", "client_secret", "root_dir"],
        )?;
        if config_optional_string(&self.config, "refresh_token").is_none() {
            require_fields(&self.config, &["email"])?;
        }
        let tenant_id = config_string(&self.config, "tenant_id")?;
        let url = format!("https://login.microsoftonline.com/{tenant_id}/oauth2/v2.0/token");
        let mut params = vec![
            ("client_id", config_string(&self.config, "client_id")?),
            (
                "client_secret",
                config_string(&self.config, "client_secret")?,
            ),
        ];
        if let Some(refresh_token) = config_optional_string(&self.config, "refresh_token") {
            params.extend([
                ("grant_type", "refresh_token".to_string()),
                ("refresh_token", refresh_token),
            ]);
        } else {
            params.extend([
                ("scope", "https://graph.microsoft.com/.default".to_string()),
                ("grant_type", "client_credentials".to_string()),
            ]);
        }
        let response = self.client.post(url).form(&params).send().await?;
        if !response.status().is_success() {
            return Err(onedrive_response_error("token", response).await);
        }
        let body: Value = response.json().await?;
        body.get("access_token")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| AppError::External("onedrive access_token missing".to_string()))
    }

    fn drive_base_url(&self) -> AppResult<String> {
        if config_optional_string(&self.config, "refresh_token").is_some() {
            return Ok("https://graph.microsoft.com/v1.0/me/drive".to_string());
        }
        let email = encode_path_segment(&config_string(&self.config, "email")?);
        Ok(format!(
            "https://graph.microsoft.com/v1.0/users/{email}/drive"
        ))
    }

    fn drive_path(&self, object_key: &str) -> AppResult<String> {
        let root = config_string(&self.config, "root_dir")?;
        let storage_key = apply_path_prefix(
            &config_optional_string(&self.config, "path_prefix").unwrap_or_default(),
            object_key,
        );
        Ok(join_encoded_path(
            root.trim_matches('/'),
            storage_key.trim_start_matches('/'),
        ))
    }
}

impl OracleOciNativeProvider {
    pub fn from_config(config: Value, provider_id: uuid::Uuid) -> AppResult<Self> {
        require_fields(
            &config,
            &[
                "region",
                "namespace",
                "bucket",
                "tenancy_ocid",
                "user_ocid",
                "fingerprint",
                "private_key",
            ],
        )?;
        let private_key = parse_rsa_private_key(
            &config_string(&config, "private_key")?,
            config_optional_string(&config, "private_key_passphrase").as_deref(),
        )?;
        Ok(Self {
            client: Client::new(),
            config,
            provider_id,
            private_key,
        })
    }

    fn region(&self) -> AppResult<String> {
        config_string(&self.config, "region")
    }

    fn namespace(&self) -> AppResult<String> {
        config_string(&self.config, "namespace")
    }

    fn bucket(&self) -> AppResult<String> {
        config_string(&self.config, "bucket")
    }

    fn key_id(&self) -> AppResult<String> {
        Ok(format!(
            "{}/{}/{}",
            config_string(&self.config, "tenancy_ocid")?,
            config_string(&self.config, "user_ocid")?,
            config_string(&self.config, "fingerprint")?
        ))
    }

    fn path_prefix(&self) -> String {
        config_optional_string(&self.config, "path_prefix")
            .unwrap_or_default()
            .trim_matches('/')
            .to_string()
    }

    fn object_key(&self, object_key: &str) -> String {
        apply_path_prefix(&self.path_prefix(), object_key)
    }

    fn host(&self) -> AppResult<String> {
        Ok(format!("objectstorage.{}.oraclecloud.com", self.region()?))
    }

    fn object_path(&self, object_key: &str) -> AppResult<String> {
        let namespace = encode_path_segment(&self.namespace()?);
        let bucket = encode_path_segment(&self.bucket()?);
        let encoded_key = encode_object_name(&self.object_key(object_key));
        Ok(format!("/n/{namespace}/b/{bucket}/o/{encoded_key}"))
    }

    fn bucket_path(&self) -> AppResult<String> {
        let namespace = encode_path_segment(&self.namespace()?);
        let bucket = encode_path_segment(&self.bucket()?);
        Ok(format!("/n/{namespace}/b/{bucket}"))
    }

    fn object_url(&self, object_key: &str) -> AppResult<String> {
        Ok(format!(
            "https://{}{}",
            self.host()?,
            self.object_path(object_key)?
        ))
    }

    fn bucket_url(&self) -> AppResult<String> {
        Ok(format!("https://{}{}", self.host()?, self.bucket_path()?))
    }

    fn public_or_proxy_url(&self, object_key: &str) -> String {
        let storage_key = self.object_key(object_key);
        if let Some(domain) = self.config.get("public_domain").and_then(Value::as_str)
            && !domain.trim().is_empty()
        {
            format!("{}/{}", domain.trim_end_matches('/'), storage_key)
        } else {
            format!(
                "/api/storage/proxy/{}/{}",
                self.provider_id,
                object_key.trim_start_matches('/')
            )
        }
    }

    fn signing_headers(
        &self,
        method: &Method,
        path_and_query: &str,
        body: &[u8],
        content_type: Option<&str>,
    ) -> AppResult<HeaderMap> {
        let host = self.host()?;
        let date = Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        let mut headers = HeaderMap::new();
        headers.insert(
            HOST,
            HeaderValue::from_str(&host).map_err(|err| AppError::BadRequest(err.to_string()))?,
        );
        headers.insert(
            DATE,
            HeaderValue::from_str(&date).map_err(|err| AppError::BadRequest(err.to_string()))?,
        );

        let request_target = format!(
            "{} {}",
            method.as_str().to_ascii_lowercase(),
            path_and_query
        );
        let mut signed_headers = vec!["date", "(request-target)", "host"];
        let mut signing_lines = vec![
            format!("date: {date}"),
            format!("(request-target): {request_target}"),
            format!("host: {host}"),
        ];

        if method == Method::PUT || method == Method::POST {
            let body_hash = STANDARD.encode(Sha256::digest(body));
            let content_length = body.len().to_string();
            let content_type = content_type.unwrap_or("application/octet-stream");
            headers.insert(
                CONTENT_LENGTH,
                HeaderValue::from_str(&content_length)
                    .map_err(|err| AppError::BadRequest(err.to_string()))?,
            );
            headers.insert(
                CONTENT_TYPE,
                HeaderValue::from_str(content_type)
                    .map_err(|err| AppError::BadRequest(err.to_string()))?,
            );
            headers.insert(
                HeaderName::from_static("x-content-sha256"),
                HeaderValue::from_str(&body_hash)
                    .map_err(|err| AppError::BadRequest(err.to_string()))?,
            );
            signed_headers.extend(["content-length", "content-type", "x-content-sha256"]);
            signing_lines.extend([
                format!("content-length: {content_length}"),
                format!("content-type: {content_type}"),
                format!("x-content-sha256: {body_hash}"),
            ]);
        }

        let signing_string = signing_lines.join("\n");
        let signing_key = SigningKey::<Sha256>::new(self.private_key.clone());
        let signature = signing_key.sign(signing_string.as_bytes());
        let authorization = format!(
            "Signature version=\"1\",keyId=\"{}\",algorithm=\"rsa-sha256\",headers=\"{}\",signature=\"{}\"",
            self.key_id()?,
            signed_headers.join(" "),
            STANDARD.encode(signature.to_bytes())
        );
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&authorization)
                .map_err(|err| AppError::BadRequest(err.to_string()))?,
        );
        Ok(headers)
    }

    fn signed_request(
        &self,
        method: Method,
        url: String,
        path_and_query: String,
        body: Vec<u8>,
        content_type: Option<&str>,
    ) -> AppResult<reqwest::RequestBuilder> {
        let headers = self.signing_headers(&method, &path_and_query, &body, content_type)?;
        Ok(self.client.request(method, url).headers(headers).body(body))
    }

    async fn create_par_url(&self, object_key: &str) -> AppResult<String> {
        let namespace = encode_path_segment(&self.namespace()?);
        let bucket = encode_path_segment(&self.bucket()?);
        let path = format!("/n/{namespace}/b/{bucket}/p/");
        let url = format!("https://{}{}", self.host()?, path);
        let ttl_seconds = config_i64(&self.config, "par_ttl_seconds")
            .or_else(|| config_i64(&self.config, "presigned_url_ttl_seconds"))
            .unwrap_or(3600)
            .clamp(60, 604800);
        let time_expires = (Utc::now() + Duration::seconds(ttl_seconds))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let body = serde_json::json!({
            "name": format!("tide-{}", uuid::Uuid::new_v4()),
            "accessType": "ObjectRead",
            "objectName": self.object_key(object_key),
            "timeExpires": time_expires
        });
        let body_bytes =
            serde_json::to_vec(&body).map_err(|err| AppError::BadRequest(err.to_string()))?;
        let response = self
            .signed_request(
                Method::POST,
                url,
                path,
                body_bytes,
                Some("application/json"),
            )?
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AppError::External(format!(
                "oci preauthenticated request failed with {status}: {body}"
            )));
        }
        let response_body: Value = response.json().await?;
        if let Some(full_path) = response_body.get("fullPath").and_then(Value::as_str)
            && full_path.starts_with("http")
        {
            return Ok(full_path.to_string());
        }
        let access_uri = response_body
            .get("accessUri")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::External("oci accessUri missing".to_string()))?;
        if access_uri.starts_with("http") {
            Ok(access_uri.to_string())
        } else {
            Ok(format!("https://{}{}", self.host()?, access_uri))
        }
    }

    async fn signed_empty_request(
        &self,
        method: Method,
        url: String,
        path_and_query: String,
    ) -> AppResult<reqwest::Response> {
        let headers = self.signing_headers(&method, &path_and_query, &[], None)?;
        Ok(self
            .client
            .request(method, url)
            .headers(headers)
            .send()
            .await?)
    }
}

#[async_trait]
impl StorageProvider for OracleOciNativeProvider {
    async fn put_object(
        &self,
        object_key: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> AppResult<StoredObject> {
        let path = self.object_path(object_key)?;
        let url = format!("https://{}{}", self.host()?, path);
        let response = self
            .signed_request(Method::PUT, url, path, bytes.to_vec(), Some(content_type))?
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AppError::External(format!(
                "oci upload failed with {status}: {body}"
            )));
        }
        let etag = response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        Ok(StoredObject {
            object_key: object_key.to_string(),
            public_url: Some(self.public_or_proxy_url(object_key)),
            size: bytes.len() as i64,
            etag,
        })
    }

    async fn get_object(&self, object_key: &str) -> AppResult<Vec<u8>> {
        let path = self.object_path(object_key)?;
        let response = self
            .signed_empty_request(Method::GET, self.object_url(object_key)?, path)
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AppError::External(format!(
                "oci read failed with {status}: {body}"
            )));
        }
        Ok(response.bytes().await?.to_vec())
    }

    async fn delete_object(&self, object_key: &str) -> AppResult<()> {
        let path = self.object_path(object_key)?;
        let response = self
            .signed_empty_request(Method::DELETE, self.object_url(object_key)?, path)
            .await?;
        if response.status().is_success() || response.status().as_u16() == 404 {
            Ok(())
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            Err(AppError::External(format!(
                "oci delete failed with {status}: {body}"
            )))
        }
    }

    async fn head_object(&self, object_key: &str) -> AppResult<bool> {
        let path = self.object_path(object_key)?;
        let response = self
            .signed_empty_request(Method::HEAD, self.object_url(object_key)?, path)
            .await?;
        if response.status().is_success() {
            Ok(true)
        } else if response.status().as_u16() == 404 {
            Ok(false)
        } else {
            Err(AppError::External(format!(
                "oci head failed with {}",
                response.status()
            )))
        }
    }

    async fn get_public_url(&self, object_key: &str) -> AppResult<String> {
        Ok(self.public_or_proxy_url(object_key))
    }

    async fn create_presigned_url(&self, object_key: &str) -> AppResult<String> {
        if self
            .config
            .get("public_domain")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .is_some()
        {
            self.get_public_url(object_key).await
        } else {
            self.create_par_url(object_key).await
        }
    }

    async fn health_check(&self) -> AppResult<()> {
        let path = self.bucket_path()?;
        let response = self
            .signed_empty_request(Method::GET, self.bucket_url()?, path)
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            Err(AppError::External(format!(
                "oci health failed with {status}: {body}"
            )))
        }
    }

    async fn refresh_auth(&self) -> AppResult<()> {
        self.health_check().await
    }
}

#[async_trait]
impl StorageProvider for S3CompatibleProvider {
    async fn put_object(
        &self,
        object_key: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> AppResult<StoredObject> {
        let storage_key = apply_path_prefix(&self.path_prefix, object_key);
        let response = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(&storage_key)
            .content_type(content_type)
            .body(ByteStream::from(bytes.to_vec()))
            .send()
            .await
            .map_err(s3_sdk_error)?;
        Ok(StoredObject {
            object_key: object_key.to_string(),
            public_url: Some(self.public_or_proxy_url(object_key)),
            size: bytes.len() as i64,
            etag: response.e_tag().map(ToString::to_string),
        })
    }

    async fn get_object(&self, object_key: &str) -> AppResult<Vec<u8>> {
        let storage_key = apply_path_prefix(&self.path_prefix, object_key);
        let output = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&storage_key)
            .send()
            .await
            .map_err(s3_sdk_error)?;
        let bytes = output
            .body
            .collect()
            .await
            .map_err(|err| AppError::External(err.to_string()))?
            .into_bytes()
            .to_vec();
        Ok(bytes)
    }

    async fn delete_object(&self, object_key: &str) -> AppResult<()> {
        let storage_key = apply_path_prefix(&self.path_prefix, object_key);
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(storage_key)
            .send()
            .await
            .map_err(s3_sdk_error)?;
        Ok(())
    }

    async fn head_object(&self, object_key: &str) -> AppResult<bool> {
        let storage_key = apply_path_prefix(&self.path_prefix, object_key);
        let result = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(storage_key)
            .send()
            .await;
        match result {
            Ok(_) => Ok(true),
            Err(err) => {
                if s3_error_status(&err) == Some(404)
                    || err
                        .as_service_error()
                        .is_some_and(|error| error.code() == Some("NotFound"))
                {
                    Ok(false)
                } else {
                    Err(s3_sdk_error(err))
                }
            }
        }
    }

    async fn get_public_url(&self, object_key: &str) -> AppResult<String> {
        Ok(self.public_or_proxy_url(object_key))
    }

    async fn create_presigned_url(&self, object_key: &str) -> AppResult<String> {
        if self.public_domain.is_some() {
            return self.get_public_url(object_key).await;
        }
        let storage_key = apply_path_prefix(&self.path_prefix, object_key);
        let request = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(storage_key)
            .presigned(
                PresigningConfig::expires_in(StdDuration::from_secs(
                    self.presigned_url_ttl_seconds as u64,
                ))
                .map_err(|err| AppError::BadRequest(err.to_string()))?,
            )
            .await
            .map_err(s3_sdk_error)?;
        Ok(request.uri().to_string())
    }

    async fn health_check(&self) -> AppResult<()> {
        if matches!(
            self.provider_type.as_str(),
            "cloudflare_r2" | "oracle_s3" | "s3_compatible"
        ) {
            let object_key = format!(".tide-storage-healthcheck/{}", uuid::Uuid::new_v4());
            self.put_object(&object_key, b"ok", "text/plain").await?;
            let bytes = self.get_object(&object_key).await?;
            self.delete_object(&object_key).await?;
            if bytes == b"ok" {
                return Ok(());
            }
            return Err(AppError::External(format!(
                "{} health check readback mismatch",
                self.provider_type
            )));
        }
        self.client
            .head_bucket()
            .bucket(&self.bucket)
            .send()
            .await
            .map_err(s3_sdk_error)?;
        Ok(())
    }

    async fn refresh_auth(&self) -> AppResult<()> {
        Ok(())
    }
}

impl S3CompatibleProvider {
    fn public_or_proxy_url(&self, object_key: &str) -> String {
        let storage_key = apply_path_prefix(&self.path_prefix, object_key);
        if let Some(domain) = &self.public_domain {
            format!(
                "{}/{}",
                domain.trim_end_matches('/'),
                encode_object_name(&storage_key)
            )
        } else {
            format!(
                "/api/storage/proxy/{}/{}",
                self.provider_id,
                encode_object_name(object_key.trim_start_matches('/'))
            )
        }
    }
}

#[async_trait]
impl StorageProvider for OneDriveProvider {
    async fn put_object(
        &self,
        object_key: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> AppResult<StoredObject> {
        let token = self.token().await?;
        let path = self.drive_path(object_key)?;
        let url = format!("{}/root:/{path}:/content", self.drive_base_url()?);
        let response = self
            .client
            .put(url)
            .bearer_auth(token)
            .header("content-type", content_type)
            .body(bytes.to_vec())
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(onedrive_response_error("upload", response).await);
        }
        let body: Value = response.json().await?;
        Ok(StoredObject {
            object_key: object_key.to_string(),
            public_url: None,
            size: bytes.len() as i64,
            etag: body
                .get("eTag")
                .and_then(Value::as_str)
                .map(ToString::to_string),
        })
    }

    async fn get_object(&self, object_key: &str) -> AppResult<Vec<u8>> {
        let token = self.token().await?;
        let path = self.drive_path(object_key)?;
        let url = format!("{}/root:/{path}:/content", self.drive_base_url()?);
        let response = self.client.get(url).bearer_auth(token).send().await?;
        if !response.status().is_success() {
            return Err(onedrive_response_error("read", response).await);
        }
        Ok(response.bytes().await?.to_vec())
    }

    async fn delete_object(&self, object_key: &str) -> AppResult<()> {
        let token = self.token().await?;
        let path = self.drive_path(object_key)?;
        let url = format!("{}/root:/{path}", self.drive_base_url()?);
        let response = self.client.delete(url).bearer_auth(token).send().await?;
        if !response.status().is_success() && response.status() != StatusCode::NOT_FOUND {
            return Err(onedrive_response_error("delete", response).await);
        }
        Ok(())
    }

    async fn head_object(&self, object_key: &str) -> AppResult<bool> {
        let token = self.token().await?;
        let path = self.drive_path(object_key)?;
        let url = format!("{}/root:/{path}", self.drive_base_url()?);
        let response = self.client.get(url).bearer_auth(token).send().await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if !response.status().is_success() {
            return Err(onedrive_response_error("head", response).await);
        }
        Ok(true)
    }

    async fn get_public_url(&self, object_key: &str) -> AppResult<String> {
        Ok(format!(
            "/api/storage/proxy/{}/{}",
            self.provider_id,
            object_key.trim_start_matches('/')
        ))
    }

    async fn create_presigned_url(&self, object_key: &str) -> AppResult<String> {
        self.get_public_url(object_key).await
    }

    async fn health_check(&self) -> AppResult<()> {
        let token = self.token().await?;
        let url = self.drive_base_url()?;
        let response = self.client.get(url).bearer_auth(token).send().await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(onedrive_response_error("health", response).await)
        }
    }

    async fn refresh_auth(&self) -> AppResult<()> {
        let _ = self.token().await?;
        Ok(())
    }
}

fn require_fields(config: &Value, fields: &[&str]) -> AppResult<()> {
    for field in fields {
        if config
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            return Err(AppError::BadRequest(format!(
                "missing storage config field {field}"
            )));
        }
    }
    Ok(())
}

fn s3_sdk_error<E>(err: SdkError<E, HttpResponse>) -> AppError
where
    E: ProvideErrorMetadata + std::fmt::Display,
{
    let status = s3_error_status(&err);
    let code = err
        .as_service_error()
        .and_then(ProvideErrorMetadata::code)
        .map(ToString::to_string);
    let message = err
        .as_service_error()
        .and_then(ProvideErrorMetadata::message)
        .map(ToString::to_string)
        .filter(|message| !message.trim().is_empty());
    let detail = match (status, code.as_deref(), message.as_deref()) {
        (Some(status), Some(code), Some(message)) => {
            format!("S3/R2 request failed: HTTP {status}, code {code}, {message}")
        }
        (Some(status), Some(code), None) => {
            format!("S3/R2 request failed: HTTP {status}, code {code}")
        }
        (Some(status), None, Some(message)) => {
            format!("S3/R2 request failed: HTTP {status}, {message}")
        }
        (Some(status), None, None) => format!("S3/R2 request failed: HTTP {status}"),
        (None, Some(code), Some(message)) => {
            format!("S3/R2 request failed: code {code}, {message}")
        }
        (None, Some(code), None) => format!("S3/R2 request failed: code {code}"),
        (None, None, Some(message)) => format!("S3/R2 request failed: {message}"),
        (None, None, None) => err.to_string(),
    };
    AppError::External(s3_error_hint(status, detail))
}

fn s3_error_status<E>(err: &SdkError<E, HttpResponse>) -> Option<u16> {
    err.raw_response()
        .map(|response| response.status().as_u16())
}

fn s3_error_hint(status: Option<u16>, detail: String) -> String {
    if status == Some(403) {
        format!(
            "{detail}. S3/R2 返回 403：请确认 endpoint/region、Bucket 和 Access Key ID/Secret Access Key 匹配同一个存储账号，并且密钥对该 bucket 至少有 Object Read/Write/Delete 权限。"
        )
    } else if status == Some(404) {
        format!(
            "{detail}. R2/S3 返回 404：请确认 bucket 名称、Account ID 和 endpoint/jurisdiction 配置正确。"
        )
    } else {
        detail
    }
}

fn config_string(config: &Value, field: &str) -> AppResult<String> {
    config
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| AppError::BadRequest(format!("missing storage config field {field}")))
}

fn config_optional_string(config: &Value, field: &str) -> Option<String> {
    config
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
}

fn config_i64(config: &Value, field: &str) -> Option<i64> {
    config
        .get(field)
        .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
}

fn s3_endpoint(provider_type: &str, config: &Value) -> AppResult<String> {
    if let Some(endpoint) = config_optional_string(config, "endpoint") {
        return Ok(endpoint);
    }
    if provider_type == "cloudflare_r2" {
        let account_id = config_string(config, "account_id")?;
        return cloudflare_r2_endpoint(&account_id, config_optional_string(config, "jurisdiction"));
    }
    config_string(config, "endpoint")
}

fn s3_region(provider_type: &str, config: &Value) -> AppResult<String> {
    if provider_type == "cloudflare_r2" {
        Ok(config_optional_string(config, "region").unwrap_or_else(|| "auto".to_string()))
    } else {
        config_string(config, "region")
    }
}

fn s3_force_path_style(provider_type: &str, config: &Value) -> bool {
    config
        .get("force_path_style")
        .or_else(|| config.get("path_style"))
        .and_then(|value| {
            value.as_bool().or_else(|| {
                value.as_str().map(|text| {
                    matches!(
                        text.trim().to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "path" | "path_style"
                    )
                })
            })
        })
        .unwrap_or({
            matches!(
                provider_type,
                "cloudflare_r2" | "oracle_s3" | "s3_compatible"
            )
        })
}

fn cloudflare_r2_endpoint(account_id: &str, jurisdiction: Option<String>) -> AppResult<String> {
    let account_id = account_id.trim();
    if account_id.is_empty()
        || account_id.contains("://")
        || account_id.contains('/')
        || account_id.chars().any(char::is_whitespace)
    {
        return Err(AppError::BadRequest(
            "cloudflare_r2 account_id is invalid".to_string(),
        ));
    }
    let host = match jurisdiction
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "" | "auto" | "default" | "global" => format!("{account_id}.r2.cloudflarestorage.com"),
        "eu" => format!("{account_id}.eu.r2.cloudflarestorage.com"),
        "fedramp" => format!("{account_id}.fedramp.r2.cloudflarestorage.com"),
        value => {
            return Err(AppError::BadRequest(format!(
                "unsupported cloudflare_r2 jurisdiction {value}"
            )));
        }
    };
    Ok(format!("https://{host}"))
}

fn apply_path_prefix(path_prefix: &str, object_key: &str) -> String {
    let prefix = path_prefix.trim_matches('/');
    let key = object_key.trim_start_matches('/');
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}/{key}")
    }
}

fn public_url(base_url: &str, public_prefix: &str, object_key: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let prefix = public_prefix.trim_matches('/');
    let key = object_key.trim_start_matches('/');
    let path = if prefix.is_empty() {
        format!("/{key}")
    } else {
        format!("/{prefix}/{key}")
    };
    if is_local_public_base_url(base) {
        return path;
    }
    format!("{base}{path}")
}

fn is_local_public_base_url(base_url: &str) -> bool {
    let Some((_, rest)) = base_url.split_once("://") else {
        return base_url.trim().is_empty();
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

fn encode_path_segment(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

fn encode_object_name(value: &str) -> String {
    value
        .split('/')
        .map(encode_path_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn join_encoded_path(root: &str, object_key: &str) -> String {
    [root, object_key]
        .into_iter()
        .filter(|part| !part.trim_matches('/').is_empty())
        .flat_map(|part| part.trim_matches('/').split('/'))
        .filter(|part| !part.is_empty())
        .map(encode_path_segment)
        .collect::<Vec<_>>()
        .join("/")
}

async fn onedrive_response_error(operation: &str, response: Response) -> AppError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let detail = onedrive_error_detail(&body);
    let hint = onedrive_error_hint(status, &detail);
    AppError::External(format!(
        "OneDrive {}失败（{}）：{}{}",
        onedrive_operation_label(operation),
        status,
        detail,
        hint
    ))
}

fn onedrive_operation_label(operation: &str) -> &'static str {
    match operation {
        "token" => "获取令牌",
        "upload" => "上传",
        "read" => "读取",
        "delete" => "删除",
        "head" => "检查文件",
        "health" => "连接检查",
        _ => "操作",
    }
}

fn onedrive_error_detail(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return "Microsoft Graph returned an empty error body".to_string();
    }
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        if let Some(error) = value.get("error") {
            if let Some(error) = error.as_object() {
                let code = error
                    .get("code")
                    .and_then(Value::as_str)
                    .or_else(|| error.get("error").and_then(Value::as_str))
                    .unwrap_or_default();
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .or_else(|| error.get("error_description").and_then(Value::as_str))
                    .unwrap_or_default();
                return compact_external_error("Microsoft Graph", code, message);
            }
            if let Some(code) = error.as_str() {
                let message = value
                    .get("error_description")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                return compact_external_error("Microsoft identity", code, message);
            }
        }
        if let Some(message) = value.get("message").and_then(Value::as_str) {
            return truncate_for_display(message, 600);
        }
        return truncate_for_display(&value.to_string(), 600);
    }
    truncate_for_display(body, 600)
}

fn compact_external_error(prefix: &str, code: &str, message: &str) -> String {
    let code = code.trim();
    let message = message.trim();
    match (code.is_empty(), message.is_empty()) {
        (false, false) => format!("{prefix} {code}: {}", truncate_for_display(message, 520)),
        (false, true) => format!("{prefix} {code}"),
        (true, false) => truncate_for_display(message, 600),
        (true, true) => format!("{prefix} returned an unknown error"),
    }
}

fn truncate_for_display(value: &str, limit: usize) -> String {
    let trimmed = value.trim();
    let truncated = trimmed.chars().take(limit).collect::<String>();
    if trimmed.chars().count() > limit {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn onedrive_error_hint(status: StatusCode, detail: &str) -> &'static str {
    let detail = detail.to_ascii_lowercase();
    if status == StatusCode::UNAUTHORIZED {
        if detail.contains("invalid_client") {
            return " 提示：请检查租户 ID、客户端 ID、客户端密钥是否正确，尤其确认填写的是 Azure 客户端密钥的 Value，不是 Secret ID；也请确认密钥没有过期。";
        }
        if detail.contains("invalid_grant") {
            return " 提示：刷新令牌已过期或已被撤销，请重新授权并生成新的刷新令牌。";
        }
        return " 提示：请检查租户 ID、客户端 ID、客户端密钥、令牌类型，以及令牌受众是否为 Microsoft Graph。";
    }
    if status == StatusCode::FORBIDDEN {
        return " 提示：请为 Microsoft Graph 应用权限 Files.ReadWrite.All 和 User.Read.All 授予管理员同意；如果租户限制应用权限访问该 OneDrive，可改用刷新令牌模式。";
    }
    if status == StatusCode::NOT_FOUND {
        return " 提示：请检查账号邮箱是否正确，确认该账号已经开通过 OneDrive，并检查根目录配置。";
    }
    ""
}

fn parse_rsa_private_key(pem: &str, passphrase: Option<&str>) -> AppResult<RsaPrivateKey> {
    if passphrase.filter(|value| !value.is_empty()).is_some() {
        return Err(AppError::BadRequest(
            "encrypted OCI private keys are not supported by this build; provide an unencrypted PEM"
                .to_string(),
        ));
    }
    let pem = normalize_private_key_pem(pem)?;
    RsaPrivateKey::from_pkcs8_pem(&pem)
        .or_else(|_| RsaPrivateKey::from_pkcs1_pem(&pem))
        .map_err(|err| AppError::BadRequest(format!("invalid OCI private key: {err}")))
}

fn normalize_private_key_pem(pem: &str) -> AppResult<String> {
    let normalized = pem
        .replace('\0', "")
        .replace("\\r\\n", "\n")
        .replace("\\n", "\n")
        .replace('\r', "\n");
    let begin = normalized.find("-----BEGIN ").ok_or_else(|| {
        AppError::BadRequest("invalid OCI private key: missing PEM BEGIN line".to_string())
    })?;
    let end_marker = "-----END ";
    let end_start = normalized[begin..]
        .find(end_marker)
        .map(|offset| begin + offset)
        .ok_or_else(|| {
            AppError::BadRequest("invalid OCI private key: missing PEM END line".to_string())
        })?;
    let end_line_end = normalized[end_start..]
        .find("-----")
        .and_then(|offset| {
            normalized[end_start + offset + 5..]
                .find("-----")
                .map(|tail| end_start + offset + 10 + tail)
        })
        .unwrap_or(normalized.len());
    Ok(normalized[begin..end_line_end].trim().to_string())
}

pub fn original_key(sha256: &str, ext: &str) -> String {
    let now = Utc::now();
    format!(
        "images/{:04}/{:02}/{:02}/{}.{}",
        now.year(),
        now.month(),
        now.day(),
        sha256,
        ext
    )
}

pub fn preview_key(sha256: &str) -> String {
    let now = Utc::now();
    format!(
        "previews/{:04}/{:02}/{:02}/{}.webp",
        now.year(),
        now.month(),
        now.day(),
        sha256
    )
}

pub fn avatar_key(user_id: &uuid::Uuid, sha256: &str) -> String {
    format!("avatars/{user_id}/{sha256}.webp")
}

#[allow(dead_code)]
pub fn backup_key(backup_id: &str) -> String {
    let now = Utc::now();
    format!(
        "backups/{:04}/{:02}/{:02}/{}.tar.zst",
        now.year(),
        now.month(),
        now.day(),
        backup_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use rsa::pkcs8::{EncodePrivateKey, LineEnding};
    use rsa::signature::Verifier;

    fn test_config() -> AppConfig {
        AppConfig {
            host: "127.0.0.1".to_string(),
            port: 8080,
            database_url: "postgres://example".to_string(),
            public_base_url: "http://localhost:8080".to_string(),
            session_secret: "session".to_string(),
            encryption_key: "encryption".to_string(),
            local_storage_root: "/tmp/tide-default-storage".to_string(),
            local_storage_public_prefix: "/files".to_string(),
            ai_service_url: "http://localhost:8000".to_string(),
            initial_admin_email: "admin@example.com".to_string(),
            initial_admin_username: "admin".to_string(),
            initial_admin_password: "ChangeMe123!".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn path_prefix_is_applied_without_double_slashes() {
        assert_eq!(
            apply_path_prefix("albums", "images/a.jpg"),
            "albums/images/a.jpg"
        );
        assert_eq!(
            apply_path_prefix("/albums/", "/images/a.jpg"),
            "albums/images/a.jpg"
        );
        assert_eq!(apply_path_prefix("", "/images/a.jpg"), "images/a.jpg");
    }

    #[test]
    fn local_public_url_is_joined_without_double_slashes() {
        assert_eq!(
            public_url("https://img.example.com/", "/files/", "/images/a.jpg"),
            "https://img.example.com/files/images/a.jpg"
        );
        assert_eq!(
            public_url("http://localhost:8080/", "/files/", "/images/a.jpg"),
            "/files/images/a.jpg"
        );
        assert_eq!(
            public_url("http://localhost:8080", "", "images/a.jpg"),
            "/images/a.jpg"
        );
    }

    #[test]
    fn local_storage_empty_config_uses_environment_defaults() {
        let provider = LocalStorageProvider::from_config(
            &test_config(),
            &serde_json::json!({"root":"","public_prefix":"","path_prefix":""}),
        );
        assert_eq!(provider.root, PathBuf::from("/tmp/tide-default-storage"));
        assert_eq!(provider.public_prefix, "/files");
        assert_eq!(
            futures::executor::block_on(provider.get_public_url("images/a.jpg")).unwrap(),
            "/files/images/a.jpg"
        );
    }

    #[test]
    fn cloudflare_r2_endpoint_is_derived_from_account_id_and_jurisdiction() {
        assert_eq!(
            s3_endpoint(
                "cloudflare_r2",
                &serde_json::json!({"account_id": "abc123"})
            )
            .unwrap(),
            "https://abc123.r2.cloudflarestorage.com"
        );
        assert_eq!(
            s3_endpoint(
                "cloudflare_r2",
                &serde_json::json!({"account_id": "abc123", "jurisdiction": "eu"})
            )
            .unwrap(),
            "https://abc123.eu.r2.cloudflarestorage.com"
        );
        assert_eq!(
            s3_endpoint(
                "cloudflare_r2",
                &serde_json::json!({"account_id": "abc123", "jurisdiction": "fedramp"})
            )
            .unwrap(),
            "https://abc123.fedramp.r2.cloudflarestorage.com"
        );
        assert_eq!(
            s3_region("cloudflare_r2", &serde_json::json!({})).unwrap(),
            "auto"
        );
    }

    #[test]
    fn cloudflare_r2_manual_endpoint_stays_supported() {
        let config = serde_json::json!({
            "account_id": "abc123",
            "endpoint": "https://custom.example.com",
            "region": "wnam"
        });
        assert_eq!(
            s3_endpoint("cloudflare_r2", &config).unwrap(),
            "https://custom.example.com"
        );
        assert_eq!(s3_region("cloudflare_r2", &config).unwrap(), "wnam");
    }

    #[test]
    fn oci_object_name_encoding_preserves_path_segments() {
        assert_eq!(
            encode_object_name("previews/hello world/潮汐.png"),
            "previews/hello%20world/%E6%BD%AE%E6%B1%90.png"
        );
    }

    #[test]
    fn s3_public_or_proxy_url_encodes_paths() {
        let provider = S3CompatibleProvider {
            provider_type: "cloudflare_r2".to_string(),
            client: S3Client::from_conf(
                S3ConfigBuilder::new()
                    .behavior_version(aws_config::BehaviorVersion::latest())
                    .build(),
            ),
            bucket: "bucket".to_string(),
            provider_id: uuid::Uuid::nil(),
            public_domain: Some("https://cdn.example.com/base/".to_string()),
            path_prefix: "site one".to_string(),
            presigned_url_ttl_seconds: 3600,
        };
        assert_eq!(
            provider.public_or_proxy_url("previews/hello world/潮汐.png"),
            "https://cdn.example.com/base/site%20one/previews/hello%20world/%E6%BD%AE%E6%B1%90.png"
        );
        let provider = S3CompatibleProvider {
            public_domain: None,
            ..provider
        };
        assert_eq!(
            provider.public_or_proxy_url("/previews/hello world/潮汐.png"),
            "/api/storage/proxy/00000000-0000-0000-0000-000000000000/previews/hello%20world/%E6%BD%AE%E6%B1%90.png"
        );
    }

    #[test]
    fn s3_force_path_style_can_be_disabled() {
        assert!(s3_force_path_style("cloudflare_r2", &serde_json::json!({})));
        assert!(!s3_force_path_style(
            "s3_compatible",
            &serde_json::json!({"force_path_style": false})
        ));
        assert!(s3_force_path_style(
            "s3_compatible",
            &serde_json::json!({"path_style": "yes"})
        ));
    }

    #[test]
    fn onedrive_path_encoding_preserves_segments() {
        let provider = OneDriveProvider::new(
            serde_json::json!({
                "client_id": "client",
                "tenant_id": "tenant",
                "client_secret": "secret",
                "email": "images@example.com",
                "root_dir": "Tide Images",
                "path_prefix": "site one"
            }),
            uuid::Uuid::nil(),
        );
        assert_eq!(
            provider
                .drive_path("previews/hello world/潮汐.png")
                .unwrap(),
            "Tide%20Images/site%20one/previews/hello%20world/%E6%BD%AE%E6%B1%90.png"
        );
    }

    #[test]
    fn onedrive_error_detail_extracts_graph_error() {
        let body = serde_json::json!({
            "error": {
                "code": "InvalidAuthenticationToken",
                "message": "Access token is empty."
            }
        });
        assert_eq!(
            onedrive_error_detail(&body.to_string()),
            "Microsoft Graph InvalidAuthenticationToken: Access token is empty."
        );
    }

    #[test]
    fn onedrive_error_detail_extracts_identity_error() {
        let body = serde_json::json!({
            "error": "invalid_client",
            "error_description": "AADSTS7000215: Invalid client secret provided."
        });
        assert_eq!(
            onedrive_error_detail(&body.to_string()),
            "Microsoft identity invalid_client: AADSTS7000215: Invalid client secret provided."
        );
    }

    #[test]
    fn oci_signature_headers_are_verifiable() {
        let mut rng = StdRng::seed_from_u64(42);
        let generated_key = RsaPrivateKey::new(&mut rng, 2048).expect("key generates");
        let private_key_pem = generated_key
            .to_pkcs8_pem(LineEnding::LF)
            .expect("key serializes");
        let private_key =
            parse_rsa_private_key(private_key_pem.as_str(), None).expect("key parses");
        let provider = OracleOciNativeProvider {
            client: Client::new(),
            config: serde_json::json!({
                "region": "ap-singapore-1",
                "namespace": "ns",
                "bucket": "bucket",
                "tenancy_ocid": "tenancy",
                "user_ocid": "user",
                "fingerprint": "fingerprint",
                "private_key": private_key_pem.as_str(),
                "path_prefix": "site-a"
            }),
            provider_id: uuid::Uuid::nil(),
            private_key: private_key.clone(),
        };
        let headers = provider
            .signing_headers(
                &Method::PUT,
                "/n/ns/b/bucket/o/site-a/images/a.jpg",
                b"hello",
                Some("text/plain"),
            )
            .expect("headers sign");
        let authorization = headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .expect("authorization header");
        assert!(authorization.contains("keyId=\"tenancy/user/fingerprint\""));
        assert!(authorization.contains(
            "headers=\"date (request-target) host content-length content-type x-content-sha256\""
        ));
        let date = headers
            .get(DATE)
            .and_then(|value| value.to_str().ok())
            .expect("date header");
        let body_hash = headers
            .get("x-content-sha256")
            .and_then(|value| value.to_str().ok())
            .expect("hash header");
        assert_eq!(body_hash, STANDARD.encode(Sha256::digest(b"hello")));
        let signature = authorization
            .split("signature=\"")
            .nth(1)
            .and_then(|value| value.strip_suffix('"'))
            .and_then(|value| STANDARD.decode(value).ok())
            .expect("signature bytes");
        let signing_string = format!(
            "date: {date}\n(request-target): put /n/ns/b/bucket/o/site-a/images/a.jpg\nhost: objectstorage.ap-singapore-1.oraclecloud.com\ncontent-length: 5\ncontent-type: text/plain\nx-content-sha256: {body_hash}"
        );
        let verifying_key = rsa::pkcs1v15::VerifyingKey::<Sha256>::new(private_key.to_public_key());
        let signature =
            rsa::pkcs1v15::Signature::try_from(signature.as_slice()).expect("signature shape");
        verifying_key
            .verify(signing_string.as_bytes(), &signature)
            .expect("signature verifies");
    }
}
