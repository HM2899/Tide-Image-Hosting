# 潮汐图床

潮汐图床是一个全栈 Rust 图床系统。后端使用 Axum、SQLx 和 PostgreSQL，前端使用 Leptos CSR 编译为 WASM，AI 审核服务使用 FastAPI。系统包含上传、去重、WebP 预览、公共图库、个人图库、回收站、标签、随机图、用户配额、审核、迁移、备份恢复、多存储 provider 和存储路由策略。

前端已经重构为 macOS 风格的高斯模糊界面：顶部栏、工作区、后台表格和移动端抽屉都使用半透明材料层、稳定尺寸和可扫读布局；API 请求使用同源相对路径，图片公开链接在前后端统一归一化，避免本地部署、公网域名和反向代理场景下链接不一致。

## 快速启动

```bash
cp .env.example .env
docker compose up -d --build
```

访问地址（把 `YOUR_SERVER_IP` 换成服务器公网 IP 或域名）：

```text
http://YOUR_SERVER_IP:18080
```

首次启动会自动执行 `migrations/`，初始化默认用户组、配额规则、本地存储 provider、站点设置、主题设置和初始超级管理员。

需要重点修改的 `.env`：

- `PUBLIC_BASE_URL`：公网访问地址，用于生成图片链接，例如 `http://YOUR_SERVER_IP:18080`。
- `APP_PUBLIC_PORT`：宿主机对外暴露端口，服务器上 `8080` 被占用时可改为 `18080`、`18088` 等。
- `SESSION_SECRET`、`CONFIG_ENCRYPTION_KEY`：生产环境必须更换。
- `LOCAL_STORAGE_ROOT`：本地磁盘落盘目录，默认 `/data/storage`。
- `LOCAL_STORAGE_PUBLIC_PREFIX`：本地文件公开前缀，默认 `/files`。
- `INITIAL_ADMIN_EMAIL`、`INITIAL_ADMIN_USERNAME`、`INITIAL_ADMIN_PASSWORD`：初始管理员。

## 核心能力

- 登录、注册、JWT Bearer 认证、API Token。
- 登录上传和访客上传，访客开关、验证码和审核策略由后台控制。
- SHA-256 精确去重，`file_objects` 与 `images` 引用分离。
- 原图和 WebP 预览图分别存储，默认预览最大 512x512。
- 图片列表、详情、复制 Markdown/HTML/原图/预览图链接。
- 回收站、恢复、永久删除和管理员审核拒绝后的对象清理。
- 标签创建、绑定、筛选和随机图多条件查询。
- 用户组配额、用户配额覆盖、总容量和每日上传强校验。
- 存储 provider 管理、连接测试、上传读取回测、删除测试、健康检查。
- 存储路由策略：按用户、用户组、角色和全局把上传路由到指定磁盘。
- 迁移任务：复制、移动、备份，支持暂停、继续、取消和失败重试。
- 备份恢复：包含站点设置、主题、验证码、SMTP、存储 provider、存储路由和元数据。
- AI 审核服务：FastAPI `/ai/*` 契约，支持外部模型 URL/API Key 和规则兜底。

## 存储与路由

支持的 provider 类型：

- `local`
- `cloudflare_r2`
- `onedrive`
- `oracle_s3`
- `oracle_oci_native`
- `s3_compatible`

上传选择存储的优先级：

1. 用户路由：`scope_type=user`，`scope_value` 为用户 UUID。
2. 用户组路由：`scope_type=group`，`scope_value` 为用户组 code。
3. 角色路由：`scope_type=role`，`scope_value` 为 `user`、`trusted`、`supporter`、`admin` 等角色。
4. 全局路由：`scope_type=global`。
5. 用户组配额里的 `default_storage_provider_id`。
6. 全局默认 provider。

后台入口：`管理员后台 -> 存储与迁移`。这里可以维护磁盘、健康检查、测试上传和存储路由策略。删除被历史对象引用的 provider 时，系统会停用并隐藏 provider，同时停用相关路由；未被引用的 provider 会被直接删除。

本地存储会把对象写入 provider 的 `root` 或 `LOCAL_STORAGE_ROOT`，公开链接使用 `PUBLIC_BASE_URL + public_prefix + path_prefix + object_key` 生成。云存储可配置公开域名；没有公开域名时会回退到后端代理或预签名链接。

OneDrive 支持两种授权方式：

- 默认使用 Azure App `client_credentials`，需要填写 `client_id`、`tenant_id`、`client_secret`、账号邮箱和根目录，并在 Microsoft Graph 为应用授予管理员同意后的 `Files.ReadWrite.All`、`User.Read.All` 应用权限。
- 也可填写可选 `refresh_token` 使用 delegated 模式；此时后端通过 `/me/drive` 访问当前授权账号，适合个人 OneDrive 或租户限制 `/users/{email}/drive` 的场景。

后台的连接、上传和删除测试会透出 Microsoft Graph 返回的 `code/message`。如果看到 401/403，优先检查客户端密钥是否过期、租户和应用是否匹配、Graph 权限是否已管理员同意，或改用 delegated refresh token。

## 关键接口

常用接口：

- `POST /api/auth/register`
- `POST /api/auth/login`
- `GET /api/auth/me`
- `POST /api/images/upload`
- `POST /api/guest/images/upload`
- `GET /api/public/images`
- `GET /api/images`
- `GET /api/images/{image_id}`
- `GET /api/images/{image_id}/links`
- `DELETE /api/images/{image_id}`
- `POST /api/images/{image_id}/restore`
- `DELETE /api/images/{image_id}/permanent`
- `GET /random`

管理接口：

- `GET /api/admin/dashboard`
- `GET /api/admin/storage/providers`
- `POST /api/admin/storage/providers`
- `PUT /api/admin/storage/providers/{provider_id}`
- `POST /api/admin/storage/providers/{provider_id}/test-upload`
- `GET /api/admin/storage/routes/page`
- `POST /api/admin/storage/routes`
- `PUT /api/admin/storage/routes/{route_id}`
- `POST /api/admin/storage/routes/{route_id}/enable`
- `POST /api/admin/storage/routes/{route_id}/disable`
- `DELETE /api/admin/storage/routes/{route_id}`
- `GET /api/admin/storage/health/page`

## 本地开发

后端：

```bash
cargo run -p tide-server
```

前端检查：

```bash
cargo check -p tide-web --target wasm32-unknown-unknown --bins
```

构建前端产物：

```bash
(cd crates/web && trunk build index.html --release --dist ../../frontend)
```

## 验证

```bash
cargo fmt --check
cargo check -p tide-server
cargo check -p tide-web --target wasm32-unknown-unknown --bins
cargo test --workspace
```
