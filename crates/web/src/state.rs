use serde::{Deserialize, Serialize};
use web_sys::File;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthMode {
    Login,
    Register,
    Reset,
}

#[derive(Clone)]
pub(crate) struct QueuedUpload {
    pub(crate) file: File,
    pub(crate) name: String,
    pub(crate) size: u64,
}

impl QueuedUpload {
    pub(crate) fn from_file(file: File) -> Self {
        Self {
            name: file.name(),
            size: file.size() as u64,
            file,
        }
    }

    pub(crate) fn mime_type(&self) -> String {
        self.file.type_()
    }

    pub(crate) fn is_image(&self) -> bool {
        let mime = self.mime_type();
        if mime.starts_with("image/") {
            return true;
        }
        let extension = self
            .name
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .unwrap_or_default();
        matches!(
            extension.as_str(),
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "avif"
        )
    }
}

#[derive(Clone)]
pub(crate) struct UploadProgress {
    pub(crate) name: String,
    pub(crate) percent: f64,
    pub(crate) finished: bool,
    pub(crate) success: bool,
    pub(crate) message: String,
}

impl UploadProgress {
    pub(crate) fn waiting(name: &str) -> Self {
        Self {
            name: name.to_string(),
            percent: 0.0,
            finished: false,
            success: false,
            message: "等待上传".to_string(),
        }
    }

    pub(crate) fn failed(name: &str, message: &str) -> Self {
        Self {
            name: name.to_string(),
            percent: 100.0,
            finished: true,
            success: false,
            message: message.to_string(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct LoginResponseView {
    pub(crate) token: String,
    pub(crate) user: AuthUserView,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct AuthUserView {
    pub(crate) id: String,
    pub(crate) email: String,
    pub(crate) username: String,
    pub(crate) role: String,
    pub(crate) status: String,
    pub(crate) avatar_url: Option<String>,
}

impl AuthUserView {
    pub(crate) fn is_admin(&self) -> bool {
        matches!(self.role.as_str(), "admin" | "super_admin")
    }
}
