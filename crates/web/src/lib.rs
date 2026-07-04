use gloo_net::http::Request;
use gloo_storage::{LocalStorage, Storage};
use leptos::ev::{paste as paste_event, Event, SubmitEvent};
use leptos::html::{Input, Select, Textarea};
use leptos::prelude::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tide_shared::{ImageLinks, ImageSummary, Page, QuotaView, TagView};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{ClipboardEvent, DataTransfer, File, FileList, FormData, HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};

mod components;
mod state;
mod theme;

use components::{NavButton, ViewPanel};
use state::{AuthMode, AuthUserView, LoginResponseView, QueuedUpload, UploadProgress};
use theme::{ThemeSettings, theme_css, theme_preset};

const REDACTED_VALUE: &str = "********";

#[component]
pub fn App() -> impl IntoView {
    let token = RwSignal::new(storage_get("tide_token").unwrap_or_default());
    let current_user = RwSignal::new(None::<AuthUserView>);
    let view = RwSignal::new(current_hash().unwrap_or_else(|| "upload".to_string()));
    let toast = RwSignal::new(String::new());
    let auth_modal = RwSignal::new(None::<AuthMode>);
    let theme = RwSignal::new(ThemeSettings::default());
    let selected_image =
        RwSignal::new(storage_get("tide_selected_image").filter(|value| !value.is_empty()));
    let admin_tab = RwSignal::new("dashboard".to_string());
    let preview_modal = RwSignal::new(None::<ImageLinkBundle>);
    let links_modal = RwSignal::new(None::<ImageLinkBundle>);
    let is_authenticated = move || !token.get().is_empty();

    let notify = move |message: String| {
        toast.set(message);
    };
    let open_auth = move |mode: AuthMode| {
        auth_modal.set(Some(mode));
    };
    let switch_view = move |next: &'static str| {
        view.set(next.to_string());
        if let Some(window) = web_sys::window() {
            let _ = window.location().set_hash(next);
        }
    };

    Effect::new(move |_| {
        let token_value = token.get();
        if token_value.is_empty() {
            current_user.set(None);
            return;
        }
        spawn_local(async move {
            match api_get::<AuthUserView>("/api/auth/me", &token_value).await {
                Ok(user) => current_user.set(Some(user)),
                Err(_) => {
                    token.set(String::new());
                    storage_delete("tide_token");
                    current_user.set(None);
                    notify("登录已过期，请重新登录".to_string());
                }
            }
        });
    });

    Effect::new(move |_| {
        if is_authenticated() && view.get() == "public" {
            switch_view("gallery");
        }
    });

    Effect::new(move |_| {
        spawn_local(async move {
            if let Ok(value) = api_get::<Value>("/api/settings/theme", "").await {
                theme.set(ThemeSettings::from_value(&value));
            }
        });
    });

    Effect::new(move |_| {
        let Some(hash) = current_hash() else {
            return;
        };
        let Some(result) = parse_oauth_hash(&hash) else {
            return;
        };

        if let Some(window) = web_sys::window() {
            let _ = window.location().set_hash("");
        }

        switch_view("upload");
        match result {
            OAuthHashResult::Token(token_value) => {
                storage_set("tide_token", &token_value);
                token.set(token_value.clone());
                notify("第三方登录成功".to_string());
                spawn_local(async move {
                    match api_get::<AuthUserView>("/api/auth/me", &token_value).await {
                        Ok(user) => current_user.set(Some(user)),
                        Err(err) => notify(format!("获取用户信息失败：{}", err)),
                    }
                });
            }
            OAuthHashResult::Error(message) => notify(format!("登录失败：{}", message)),
        }
    });

    let logout = move |_| {
        let token_value = token.get();
        spawn_local(async move {
            let _ =
                api_json::<Value, _>("/api/auth/logout", &token_value, "POST", &json!({})).await;
            token.set(String::new());
            current_user.set(None);
            storage_delete("tide_token");
            notify("已退出登录".to_string());
        });
    };

    view! {
        <style>{move || theme_css(&theme.get())}</style>
        <div class=move || if view.get() == "admin" { "app admin-mode" } else { "app" }>
        <header class="topbar">
            <a class="brand" href="#upload" on:click=move |_| switch_view("upload")>
                <span class="brand-mark">"潮"</span>
                <span class="brand-text">
                    <strong>"潮汐图床"</strong>
                    <small>"Tide · 极速图床"</small>
                </span>
            </a>
            <div class="account-actions">
                <Show when=move || current_user.get().map(|u| u.is_admin()).unwrap_or(false)>
                    <button class=move || if view.get() == "admin" { "selected" } else { "secondary" } type="button" on:click=move |_| switch_view("admin")>"管理员后台"</button>
                </Show>
                <Show
                    when=move || current_user.get().is_some()
                    fallback=move || view! {
                        <>
                            <button class="secondary" type="button" on:click=move |_| open_auth(AuthMode::Login)>"登录"</button>
                            <button type="button" on:click=move |_| open_auth(AuthMode::Register)>"注册"</button>
                        </>
                    }
                >
                    {move || current_user.get().map(|user| view! {
                        <>
                            <span class="user-pill">{user.username}</span>
                            <button class="secondary" type="button" on:click=logout>"退出"</button>
                        </>
                    })}
                </Show>
            </div>
        </header>
        <main class="shell">
            <aside class="side">
                <NavButton label="首页上传" id="upload" view=view set_view=switch_view/>
                <Show
                    when=is_authenticated
                    fallback=move || view! { <NavButton label="公共图库" id="public" view=view set_view=switch_view/> }
                >
                    <NavButton label="随机图" id="random" view=view set_view=switch_view/>
                    <NavButton label="我的图片" id="gallery" view=view set_view=switch_view/>
                    <NavButton label="我的回收站" id="trash" view=view set_view=switch_view/>
                    <NavButton label="个人资料" id="profile" view=view set_view=switch_view/>
                    <NavButton label="API Token" id="tokens" view=view set_view=switch_view/>
                </Show>
                <Show when=move || current_user.get().map(|user| user.is_admin()).unwrap_or(false)>
                    <NavButton label="管理员后台" id="admin" view=view set_view=switch_view/>
                </Show>
            </aside>
            <section class="workspace">
                <ViewPanel id="upload" view=view>
                    <UploadHome token=token notify=notify preview_modal=preview_modal links_modal=links_modal/>
                </ViewPanel>
                <ViewPanel id="public" view=view>
                    <Show
                        when=move || !is_authenticated()
                        fallback=move || view! { <LoginRequired title="公共图库" message="登录后请在我的图片中管理自己的图片。" open_auth=open_auth/> }
                    >
                        <PublicGallery notify=notify preview_modal=preview_modal links_modal=links_modal/>
                    </Show>
                </ViewPanel>
                <ViewPanel id="random" view=view>
                    <Show
                        when=move || current_user.get().is_some()
                        fallback=move || view! { <LoginRequired title="随机图" message="登录后可以按标签、方向和分辨率调用随机图。" open_auth=open_auth/> }
                    >
                        <RandomImagePage notify=notify _preview_modal=preview_modal _links_modal=links_modal/>
                    </Show>
                </ViewPanel>
                <ViewPanel id="gallery" view=view>
                    <Show
                        when=move || current_user.get().is_some()
                        fallback=move || view! { <LoginRequired title="我的图片" message="登录后可以查看、筛选、复制和删除自己的图片。" open_auth=open_auth/> }
                    >
                        <Gallery token=token selected_image=selected_image set_view=switch_view notify=notify preview_modal=preview_modal links_modal=links_modal/>
                    </Show>
                </ViewPanel>
                <ViewPanel id="detail" view=view>
                    <Show
                        when=move || current_user.get().is_some()
                        fallback=move || view! { <LoginRequired title="图片详情" message="登录后可以编辑自己的图片信息和复制链接。" open_auth=open_auth/> }
                    >
                        <ImageDetail token=token selected_image=selected_image notify=notify preview_modal=preview_modal links_modal=links_modal/>
                    </Show>
                </ViewPanel>
                <ViewPanel id="trash" view=view>
                    <Show
                        when=move || current_user.get().is_some()
                        fallback=move || view! { <LoginRequired title="我的回收站" message="回收站只显示当前登录账号删除的图片，请先登录。" open_auth=open_auth/> }
                    >
                        <Trash token=token notify=notify preview_modal=preview_modal links_modal=links_modal/>
                    </Show>
                </ViewPanel>
                <ViewPanel id="profile" view=view>
                    <Show
                        when=move || current_user.get().is_some()
                        fallback=move || view! { <LoginRequired title="个人资料" message="登录后可以维护资料、头像、密码和配额信息。" open_auth=open_auth/> }
                    >
                        <Profile token=token notify=notify/>
                    </Show>
                </ViewPanel>
                <ViewPanel id="tokens" view=view>
                    <Show
                        when=move || current_user.get().is_some()
                        fallback=move || view! { <LoginRequired title="API Token" message="登录后可以创建和管理自己的 API Token。" open_auth=open_auth/> }
                    >
                        <ApiTokens token=token notify=notify/>
                    </Show>
                </ViewPanel>
                <ViewPanel id="admin" view=view>
                    <Show
                        when=move || current_user.get().map(|user| user.is_admin()).unwrap_or(false)
                        fallback=move || view! { <LoginRequired title="管理员后台" message="请使用管理员账号登录后进入后台。" open_auth=open_auth/> }
                    >
                        <AdminConsole token=token admin_tab=admin_tab notify=notify theme=theme current_user=current_user set_view=switch_view/>
                    </Show>
                </ViewPanel>
            </section>
        </main>
        <nav class="mobile-nav">
            <NavButton label="首页" id="upload" view=view set_view=switch_view/>
            <Show
                when=is_authenticated
                fallback=move || view! { <NavButton label="公共" id="public" view=view set_view=switch_view/> }
            >
                <NavButton label="随机" id="random" view=view set_view=switch_view/>
                <NavButton label="图片" id="gallery" view=view set_view=switch_view/>
                <NavButton label="回收站" id="trash" view=view set_view=switch_view/>
                <NavButton label="我的" id="profile" view=view set_view=switch_view/>
            </Show>
        </nav>
        </div>
        <AuthModal mode=auth_modal token=token current_user=current_user notify=notify/>
        <PreviewModal preview=preview_modal links=links_modal notify=notify/>
        <LinksModal links=links_modal notify=notify/>
        <div class=move || if toast.get().is_empty() { "toast" } else { "toast show" }>
            {move || toast.get()}
        </div>
    }
}

#[derive(Clone, Debug)]
struct ImageLinkBundle {
    title: String,
    alt: String,
    url: String,
    preview_url: String,
    markdown: String,
    html: String,
    preview_markdown: String,
    preview_html: String,
}

impl ImageLinkBundle {
    fn from_summary(image: &ImageSummary) -> Self {
        image_bundle(ImageLinkParts {
            title: &image.title,
            alt: &image.original_name,
            url: &image.url,
            preview_url: &image.preview_url,
            markdown: None,
            html: None,
            preview_markdown: None,
            preview_html: None,
        })
    }

    fn from_links(title: &str, alt: &str, links: &ImageLinks) -> Self {
        image_bundle(ImageLinkParts {
            title,
            alt,
            url: &links.url,
            preview_url: &links.preview_url,
            markdown: Some(&links.markdown),
            html: Some(&links.html),
            preview_markdown: Some(&links.preview_markdown),
            preview_html: Some(&links.preview_html),
        })
    }

    // from_random is no longer used after the random image page redesign.
    // Kept here for reference; can be removed if no other caller appears.
    #[allow(dead_code)]
    fn from_random(response: &tide_shared::RandomImageResponse) -> Self {
        let title = response.id.to_string();
        image_bundle(ImageLinkParts {
            title: &title,
            alt: "随机图片",
            url: &response.url,
            preview_url: &response.preview_url,
            markdown: Some(&response.markdown),
            html: Some(&response.html),
            preview_markdown: Some(&response.preview_markdown),
            preview_html: Some(&response.preview_html),
        })
    }

    fn from_upload(response: &Value) -> Self {
        let title = json_string(response, "id");
        let alt = if title.is_empty() {
            "上传图片".to_string()
        } else {
            title.clone()
        };
        let url = json_string(response, "url");
        let preview_url = json_string(response, "preview_url");
        let markdown = json_string(response, "markdown");
        let html = json_string(response, "html");
        let preview_markdown = json_string(response, "preview_markdown");
        let preview_html = json_string(response, "preview_html");
        image_bundle(ImageLinkParts {
            title: if title.is_empty() {
                "上传成功"
            } else {
                &title
            },
            alt: &alt,
            url: &url,
            preview_url: &preview_url,
            markdown: Some(&markdown),
            html: Some(&html),
            preview_markdown: Some(&preview_markdown),
            preview_html: Some(&preview_html),
        })
    }

    fn preview_src(&self) -> String {
        display_url(&self.preview_url)
    }
}

struct ImageLinkParts<'a> {
    title: &'a str,
    alt: &'a str,
    url: &'a str,
    preview_url: &'a str,
    markdown: Option<&'a str>,
    html: Option<&'a str>,
    preview_markdown: Option<&'a str>,
    preview_html: Option<&'a str>,
}

fn image_bundle(parts: ImageLinkParts<'_>) -> ImageLinkBundle {
    let url = public_url(parts.url);
    let preview_url = public_url(parts.preview_url);
    let markdown_value = parts
        .markdown
        .filter(|value| !value.is_empty())
        .map(rewrite_embedded_public_urls)
        .unwrap_or_else(|| markdown_for(parts.alt, &url));
    let html_value = parts
        .html
        .filter(|value| !value.is_empty())
        .map(rewrite_embedded_public_urls)
        .unwrap_or_else(|| html_for(parts.alt, &url));
    let preview_markdown_value = parts
        .preview_markdown
        .filter(|value| !value.is_empty())
        .map(rewrite_embedded_public_urls)
        .unwrap_or_else(|| markdown_for(parts.alt, &preview_url));
    let preview_html_value = parts
        .preview_html
        .filter(|value| !value.is_empty())
        .map(rewrite_embedded_public_urls)
        .unwrap_or_else(|| html_for(parts.alt, &preview_url));
    ImageLinkBundle {
        title: if parts.title.is_empty() {
            parts.alt.to_string()
        } else {
            parts.title.to_string()
        },
        alt: parts.alt.to_string(),
        url,
        preview_url,
        markdown: markdown_value,
        html: html_value,
        preview_markdown: preview_markdown_value,
        preview_html: preview_html_value,
    }
}

fn markdown_for(alt: &str, url: &str) -> String {
    if url.is_empty() {
        String::new()
    } else {
        format!("![{}]({})", alt, url)
    }
}

fn html_for(alt: &str, url: &str) -> String {
    if url.is_empty() {
        String::new()
    } else {
        format!("<img src=\"{}\" alt=\"{}\">", url, html_escape_attr(alt))
    }
}

#[component]
fn PreviewModal(
    preview: RwSignal<Option<ImageLinkBundle>>,
    links: RwSignal<Option<ImageLinkBundle>>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let close = move |_| preview.set(None);
    view! {
        <Show when=move || preview.get().is_some()>
            <div class="modal-backdrop" on:click=close></div>
            {move || preview.get().map(|image| {
                let link_image = image.clone();
                let copy_url = image.url.clone();
                view! {
                    <section class="preview-modal panel">
                        <header class="section-head">
                            <div>
                                <p class="eyebrow">"图片预览"</p>
                                <h2>{image.title.clone()}</h2>
                            </div>
                            <button class="secondary icon-button" type="button" on:click=close aria-label="关闭">"×"</button>
                        </header>
                        <div class="preview-stage">
                            <ImageThumb src=image.preview_src() alt=image.alt.clone()/>
                        </div>
                        <div class="row-actions">
                            <button type="button" on:click=move |_| {
                                links.set(Some(link_image.clone()));
                            }>"部署引用"</button>
                            <button class="secondary" type="button" on:click=move |_| {
                                if copy_to_clipboard(&copy_url) {
                                    notify("已复制原图链接".to_string());
                                } else {
                                    notify("复制失败，请手动复制".to_string());
                                }
                            }>"复制原图"</button>
                        </div>
                    </section>
                }
            })}
        </Show>
    }
}

#[component]
fn LinksModal(
    links: RwSignal<Option<ImageLinkBundle>>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let close = move |_| links.set(None);
    view! {
        <Show when=move || links.get().is_some()>
            <div class="modal-backdrop" on:click=close></div>
            {move || links.get().map(|image| view! {
                <section class="links-modal panel">
                    <header class="section-head">
                        <div>
                            <p class="eyebrow">"部署引用"</p>
                            <h2>{image.title.clone()}</h2>
                        </div>
                        <button class="secondary icon-button" type="button" on:click=close aria-label="关闭">"×"</button>
                    </header>
                    <div class="link-list">
                        <LinkValue label="原图 URL" value=image.url.clone() notify=notify/>
                        <LinkValue label="WebP URL" value=image.preview_url.clone() notify=notify/>
                        <LinkValue label="原图 Markdown" value=image.markdown.clone() notify=notify/>
                        <LinkValue label="WebP Markdown" value=image.preview_markdown.clone() notify=notify/>
                        <LinkValue label="原图 HTML" value=image.html.clone() notify=notify/>
                        <LinkValue label="WebP HTML" value=image.preview_html.clone() notify=notify/>
                    </div>
                </section>
            })}
        </Show>
    }
}

#[component]
fn LinkValue(
    label: &'static str,
    value: String,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let copy_value = value.clone();
    view! {
        <article class="link-value">
            <span>{label}</span>
            <span class="link-text">{if value.is_empty() { "不可用".to_string() } else { value }}</span>
            <button class="secondary" type="button" disabled=copy_value.is_empty() on:click=move |_| {
                if !copy_value.is_empty() {
                    if copy_to_clipboard(&copy_value) {
                        notify("已复制".to_string());
                    } else {
                        notify("复制失败，请手动复制".to_string());
                    }
                }
            }>"复制"</button>
        </article>
    }
}

#[component]
fn ImageThumb(src: String, alt: String) -> impl IntoView {
    let src = display_url(&src);
    if src.is_empty() {
        view! { <div class="image-placeholder">"无预览"</div> }.into_any()
    } else {
        view! { <img src=src alt=alt/> }.into_any()
    }
}

#[component]
fn AuthModal(
    mode: RwSignal<Option<AuthMode>>,
    token: RwSignal<String>,
    current_user: RwSignal<Option<AuthUserView>>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let email = NodeRef::<Input>::new();
    let username = NodeRef::<Input>::new();
    let password = NodeRef::<Input>::new();
    let code = NodeRef::<Input>::new();
    let close = move |_| mode.set(None);
    let set_mode = move |next: AuthMode| mode.set(Some(next));
    let send_code = move |_| {
        let Some(email_value) = input_value(email).filter(|value| !value.is_empty()) else {
            notify("请输入邮箱".to_string());
            return;
        };
        let purpose = if mode.get() == Some(AuthMode::Reset) {
            "reset_password"
        } else {
            "register"
        };
        spawn_local(async move {
            let body = json!({"email": email_value, "purpose": purpose});
            match api_json::<Value, _>("/api/auth/email/send-code", "", "POST", &body).await {
                Ok(_) => notify("验证码已发送或写入日志".to_string()),
                Err(err) => notify(err),
            }
        });
    };
    let submit = move |event: SubmitEvent| {
        event.prevent_default();
        match mode.get().unwrap_or(AuthMode::Login) {
            AuthMode::Login => {
                let body = json!({
                    "identifier": input_value(email).unwrap_or_default(),
                    "password": input_value(password).unwrap_or_default(),
                });
                spawn_local(async move {
                    match api_json::<LoginResponseView, _>("/api/auth/login", "", "POST", &body)
                        .await
                    {
                        Ok(data) => {
                            storage_set("tide_token", &data.token);
                            token.set(data.token);
                            current_user.set(Some(data.user));
                            mode.set(None);
                            notify("已登录".to_string());
                        }
                        Err(err) => notify(err),
                    }
                });
            }
            AuthMode::Register => {
                let body = json!({
                    "email": input_value(email).unwrap_or_default(),
                    "username": input_value(username).unwrap_or_default(),
                    "password": input_value(password).unwrap_or_default(),
                    "code": input_value(code).filter(|value| !value.is_empty()),
                });
                spawn_local(async move {
                    match api_json::<LoginResponseView, _>("/api/auth/register", "", "POST", &body)
                        .await
                    {
                        Ok(data) => {
                            storage_set("tide_token", &data.token);
                            token.set(data.token);
                            current_user.set(Some(data.user));
                            mode.set(None);
                            notify("注册成功，已登录".to_string());
                        }
                        Err(err) => notify(err),
                    }
                });
            }
            AuthMode::Reset => {
                let body = json!({
                    "email": input_value(email).unwrap_or_default(),
                    "code": input_value(code).unwrap_or_default(),
                    "password": input_value(password).unwrap_or_default(),
                });
                spawn_local(async move {
                    match api_json::<Value, _>(
                        "/api/auth/password/reset/confirm",
                        "",
                        "POST",
                        &body,
                    )
                    .await
                    {
                        Ok(_) => {
                            mode.set(Some(AuthMode::Login));
                            notify("密码已重置，请重新登录".to_string());
                        }
                        Err(err) => notify(err),
                    }
                });
            }
        }
    };
    let oauth_login = move |provider: &'static str| {
        if let Some(window) = web_sys::window() {
            let _ = window
                .location()
                .set_href(&format!("/api/auth/oauth/{provider}"));
        }
    };
    view! {
        <Show when=move || mode.get().is_some()>
            <div class="modal-backdrop" on:click=close></div>
            <section class="auth-modal panel">
                <header class="section-head">
                    <div>
                        <h2>{move || auth_mode_title(mode.get())}</h2>
                    </div>
                    <button class="secondary icon-button" type="button" on:click=close aria-label="关闭">"×"</button>
                </header>
                <form class="form" on:submit=submit>
                    <input
                        node_ref=email
                        placeholder=move || if mode.get() == Some(AuthMode::Login) { "邮箱或用户名" } else { "邮箱" }
                        autocomplete=move || if mode.get() == Some(AuthMode::Login) { "username" } else { "email" }
                    />
                    <Show when=move || mode.get() == Some(AuthMode::Register)>
                        <input node_ref=username placeholder="用户名" autocomplete="username"/>
                    </Show>
                    <input
                        node_ref=password
                        placeholder=move || if mode.get() == Some(AuthMode::Reset) { "新密码" } else { "密码" }
                        type="password"
                        autocomplete=move || if mode.get() == Some(AuthMode::Register) { "new-password" } else { "current-password" }
                    />
                    <Show when=move || mode.get() != Some(AuthMode::Login)>
                        <div class="inline">
                            <input node_ref=code placeholder="邮箱验证码"/>
                            <button class="secondary" type="button" on:click=send_code>"发送验证码"</button>
                        </div>
                    </Show>
                    <button type="submit">{move || auth_submit_label(mode.get())}</button>
                </form>
                <div class="auth-divider"><span>"或使用第三方账号"</span></div>
                <div class="oauth-actions">
                    <button class="oauth-button github" type="button" on:click=move |_| oauth_login("github")>
                        <span class="oauth-mark">"GH"</span>
                        <span>"GitHub 登录"</span>
                    </button>
                    <button class="oauth-button linuxdo" type="button" on:click=move |_| oauth_login("linuxdo")>
                        <span class="oauth-mark">"L"</span>
                        <span>"Linux.do 登录"</span>
                    </button>
                </div>
                <div class="auth-switcher">
                    <button class="secondary" type="button" on:click=move |_| set_mode(AuthMode::Login)>"登录"</button>
                    <button class="secondary" type="button" on:click=move |_| set_mode(AuthMode::Register)>"注册"</button>
                    <button class="secondary" type="button" on:click=move |_| set_mode(AuthMode::Reset)>"重置密码"</button>
                </div>
            </section>
        </Show>
    }
}

#[component]
fn LoginRequired(
    title: &'static str,
    message: &'static str,
    open_auth: impl Fn(AuthMode) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    view! {
        <section class="panel login-required">
            <p class="eyebrow">"需要登录"</p>
            <h2>{title}</h2>
            <p>{message}</p>
            <div class="actions">
                <button type="button" on:click=move |_| open_auth(AuthMode::Login)>"立即登录"</button>
                <button class="secondary" type="button" on:click=move |_| open_auth(AuthMode::Register)>"注册账号"</button>
            </div>
        </section>
    }
}

#[component]
fn UploadHome(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
    preview_modal: RwSignal<Option<ImageLinkBundle>>,
    links_modal: RwSignal<Option<ImageLinkBundle>>,
) -> impl IntoView {
    let quota = RwSignal::new(None::<QuotaView>);
    let upload_result = RwSignal::new(None::<Value>);
    let queued_files = RwSignal::new(Vec::<QueuedUpload>::new());
    let upload_progress = RwSignal::new(Vec::<UploadProgress>::new());
    let is_dragging = RwSignal::new(false);
    let upload_file = NodeRef::<Input>::new();
    let upload_tags = NodeRef::<Input>::new();

    let append_files = move |files: FileList| {
        let mut incoming = Vec::new();
        for index in 0..files.length() {
            if let Some(file) = files.get(index) {
                incoming.push(QueuedUpload::from_file(file));
            }
        }
        if incoming.is_empty() {
            notify("请选择图片".to_string());
        } else {
            let count = incoming.len();
            queued_files.update(|items| items.extend(incoming));
            notify(format!("已加入 {count} 张图片"));
        }
    };

    let paste_files = queued_files;
    Effect::new(move |_| {
        let handle = window_event_listener(paste_event, move |event| {
            let event: ClipboardEvent = event.unchecked_into();
            if let Some(data) = event.clipboard_data()
                && let Some(files) = collect_paste_files(&data)
            {
                event.prevent_default();
                let notify_fn = notify;
                let queued = paste_files;
                let incoming: Vec<QueuedUpload> =
                    files.into_iter().map(QueuedUpload::from_file).collect();
                if !incoming.is_empty() {
                    let count = incoming.len();
                    queued.update(|items| items.extend(incoming));
                    notify_fn(format!("已从剪贴板加入 {count} 张图片"));
                }
            }
        });
        on_cleanup(move || handle.remove());
    });

    Effect::new(move |_| {
        let token_value = token.get();
        if token_value.is_empty() {
            quota.set(None);
            return;
        }
        spawn_local(async move {
            if let Ok(value) = api_get::<QuotaView>("/api/user/quota", &token_value).await {
                quota.set(Some(value));
            }
        });
    });

    let choose_files = move |_| {
        let Some(input) = upload_file.get() else {
            notify("请选择图片".to_string());
            return;
        };
        let Some(files) = input.files() else {
            notify("请选择图片".to_string());
            return;
        };
        append_files(files);
    };

    let upload = move |event: SubmitEvent| {
        event.prevent_default();
        let token_value = token.get();
        let files = queued_files.get();
        if files.is_empty() {
            notify("请选择图片".to_string());
            return;
        }
        if let Err(message) =
            validate_queued_files(&files, quota.get().map(|value| value.max_file_size))
        {
            notify(message);
            return;
        }
        let tags = input_value(upload_tags).unwrap_or_default();
        upload_progress.set(
            files
                .iter()
                .map(|item| UploadProgress::waiting(&item.name))
                .collect(),
        );
        spawn_local(async move {
            if token_value.is_empty()
                && let Err(message) = ensure_guest_upload_allowed(&files).await
            {
                mark_uploads_failed(upload_progress, &files, &message);
                notify(message);
                return;
            }
            match upload_queued_files(
                &token_value,
                files,
                tags,
                upload_progress,
            )
            .await
            {
                Ok(value) => {
                    upload_result.set(Some(value));
                    notify("上传完成".to_string());
                    if !token_value.is_empty()
                        && let Ok(value) =
                            api_get::<QuotaView>("/api/user/quota", &token_value).await
                    {
                        quota.set(Some(value));
                    }
                }
                Err(err) => notify(err),
            }
        });
    };

    let clear_queue = move |_| {
        queued_files.set(Vec::new());
        upload_progress.set(Vec::new());
        upload_result.set(None);
        if let Some(input) = upload_file.get() {
            input.set_value("");
        }
        notify("已清空上传队列".to_string());
    };

    view! {
        <div class="grid">
            <form
                class=move || if is_dragging.get() { "panel form upload-box drag-over" } else { "panel form upload-box" }
                on:dragover=move |event| {
                    event.prevent_default();
                    is_dragging.set(true);
                }
                on:dragleave=move |_| is_dragging.set(false)
                on:drop=move |event| {
                    event.prevent_default();
                    is_dragging.set(false);
                    if let Some(data) = event.data_transfer()
                        && let Some(files) = data.files()
                    {
                        append_files(files);
                    }
                }
                on:submit=upload
            >
                <header class="upload-card-head">
                    <div class="upload-card-title">
                        <span class="upload-badge">"⬆ 上传"</span>
                        <h2>"上传图片"</h2>
                    </div>
                    <p class="upload-hint">"拖拽 · 粘贴 · 选择 — 支持多张同时上传，完成后一键复制链接"</p>
                </header>
                <label class="drop-zone">
                    <input node_ref=upload_file type="file" accept="image/*" multiple on:change=choose_files/>
                    <span class="drop-zone-mark">"⬆"</span>
                    <strong>"拖拽图片到这里，或点击选择"</strong>
                    <span class="drop-zone-sub">"也可在此页面直接 Ctrl+V 粘贴截图"</span>
                </label>
                <div class="upload-queue">
                    {move || if queued_files.get().is_empty() {
                        view! { <span class="empty-inline">"尚未选择图片"</span> }.into_any()
                    } else {
                        queued_files.get().into_iter().map(|item| view! {
                            <span class="queue-chip">
                                <span class="queue-chip-name">{item.name.clone()}</span>
                                <span class="queue-chip-size">{format_bytes(item.size as i64)}</span>
                                <button type="button" class="queue-chip-remove" aria-label="移除" on:click=move |_| {
                                    queued_files.update(|files| {
                                        files.retain(|queued| queued.name != item.name);
                                    });
                                }>"×"</button>
                            </span>
                        }).collect_view().into_any()
                    }}
                </div>
                <div class="upload-fields">
                    <input node_ref=upload_tags placeholder="标签，逗号分隔"/>
                </div>
                <div class="actions">
                    <button type="submit">"上传"</button>
                    <button type="button" on:click=clear_queue>"清空"</button>
                </div>
                <div class="progress-list">
                    {move || upload_progress.get().into_iter().map(|item| view! {
                        <div class=if item.success { "progress-row done" } else if item.finished { "progress-row failed" } else { "progress-row" }>
                            <span>{item.name}</span>
                            <meter min="0" max="100" value=item.percent></meter>
                            <small>{item.message}</small>
                        </div>
                    }).collect_view()}
                </div>
                <UploadResultView result=upload_result notify=notify preview_modal=preview_modal links_modal=links_modal/>
            </form>
        </div>
    }
}

#[component]
fn UploadResultView(
    result: RwSignal<Option<Value>>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
    preview_modal: RwSignal<Option<ImageLinkBundle>>,
    links_modal: RwSignal<Option<ImageLinkBundle>>,
) -> impl IntoView {
    view! {
        <div class="upload-results">
            {move || match result.get() {
                Some(value) => upload_result_view(value, notify, preview_modal, links_modal).into_any(),
                None => ().into_any(),
            }}
        </div>
    }
}

#[component]
fn PublicGallery(
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
    preview_modal: RwSignal<Option<ImageLinkBundle>>,
    links_modal: RwSignal<Option<ImageLinkBundle>>,
) -> impl IntoView {
    let images = RwSignal::new(Vec::<ImageSummary>::new());
    let tag = NodeRef::<Input>::new();
    let load = move || {
        let mut path = "/api/public/images?page_size=60".to_string();
        if let Some(value) = input_value(tag).filter(|value| !value.is_empty()) {
            path.push_str("&tag=");
            path.push_str(&urlencoding::encode(&value));
        }
        spawn_local(async move {
            match api_get::<Page<ImageSummary>>(&path, "").await {
                Ok(page) => images.set(page.items),
                Err(err) => notify(err),
            }
        });
    };
    Effect::new(move |_| load());
    view! {
        <div class="panel">
            <header class="section-head">
                <div>
                    <h2>"公共图库"</h2>
                </div>
                <div class="filters">
                    <input node_ref=tag placeholder="标签"/>
                    <button type="button" on:click=move |_| load()>"刷新"</button>
                </div>
            </header>
            <div class="image-grid">
                {move || if images.get().is_empty() {
                    view! { <div class="empty">"暂无公开图片"</div> }.into_any()
                } else {
                    images.get().into_iter().map(|image| {
                        let bundle = ImageLinkBundle::from_summary(&image);
                        let preview_bundle = bundle.clone();
                        let links_bundle_1 = bundle.clone();
                        let links_bundle_2 = bundle.clone();
                        let original_url = bundle.url.clone();
                        let image_ratio = image_aspect_ratio(image.width, image.height);
                        view! {
                            <article class="image-card">
                                <button class="image-pick" type="button" style=image_ratio on:click=move |_| {
                                    preview_modal.set(Some(preview_bundle.clone()));
                                }>
                                    <ImageThumb src=bundle.preview_src() alt=bundle.alt.clone()/>
                                    <div class="image-overlay">
                                        <div class="image-overlay-actions">
                                            <button type="button" class="overlay-btn" on:click=move |event| {
                                                event.stop_propagation();
                                                links_modal.set(Some(links_bundle_1.clone()));
                                            }>"链接"</button>
                                        </div>
                                        <div class="image-overlay-info">
                                            {format!("{}×{}", image.width, image.height)}
                                            <span class="dot-sep">"·"</span>
                                            {format_bytes(image.size)}
                                        </div>
                                    </div>
                                </button>
                                <div class="image-meta">
                                    <strong>{image.title.clone()}</strong>
                                    <span class="image-tags">{if image.tags.is_empty() { "无标签".to_string() } else { image.tags.join(" · ") }}</span>
                                    <div class="row-actions">
                                        <button type="button" class="chip-btn" on:click=move |_| {
                                            if copy_to_clipboard(&original_url) {
                                                notify("已复制原图链接".to_string());
                                            } else {
                                                notify("复制失败，请手动复制".to_string());
                                            }
                                        }>"复制链接"</button>
                                        <button type="button" class="chip-btn" on:click=move |_| links_modal.set(Some(links_bundle_2.clone()))>"部署引用"</button>
                                    </div>
                                </div>
                            </article>
                        }
                    }).collect_view().into_any()
                }}
            </div>
        </div>
    }
}

#[component]
fn RandomImagePage(
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
    _preview_modal: RwSignal<Option<ImageLinkBundle>>,
    _links_modal: RwSignal<Option<ImageLinkBundle>>,
) -> impl IntoView {
    let tags = RwSignal::new(Vec::<TagView>::new());
    let selected_tag = RwSignal::new(String::new());
    let seed = RwSignal::new(0u64);

    Effect::new(move |_| {
        spawn_local(async move {
            match api_get::<Vec<TagView>>("/api/tags", "").await {
                Ok(value) => tags.set(value),
                Err(err) => notify(err),
            }
        });
    });

    let random_url = move || {
        let mut url = format!("{}/random?type=redirect&image=preview", current_origin());
        let tag = selected_tag.get();
        if !tag.is_empty() {
            url.push_str("&tags=");
            url.push_str(&urlencoding::encode(&tag));
        }
        let _ = seed.get();
        url
    };

    let refresh = move || {
        seed.update(|value| *value = value.wrapping_add(1));
    };

    view! {
        <div class="panel">
            <header class="section-head">
                <div>
                    <h2>"随机图"</h2>
                    <p class="upload-hint">"选择标签后点击链接即可刷新，每次访问都会返回一张新图。"</p>
                </div>
            </header>
            <div class="random-controls">
                <label class="random-tag-label">
                    "标签筛选"
                    <select
                        class="random-tag-select"
                        on:change=move |event: leptos::ev::Event| {
                            selected_tag.set(event_target_value(&event));
                            refresh();
                        }
                    >
                        <option value="" selected>"全部标签"</option>
                        {move || tags.get().into_iter().map(|tag| {
                            let name = tag.name.clone();
                            view! {
                                <option value={tag.name.clone()}>{name}</option>
                            }
                        }).collect_view()}
                    </select>
                </label>
                <a
                    class="random-url-card"
                    href=move || random_url()
                    target="_blank"
                    rel="noopener noreferrer"
                    on:click=move |_| {
                        refresh();
                        notify("已在新标签页打开随机图".to_string());
                    }
                >
                    <span class="random-url-icon">"🎲"</span>
                    <span class="random-url-text">{move || random_url()}</span>
                    <span class="random-url-hint">"点击刷新"</span>
                </a>
                <div class="random-actions">
                    <button type="button" on:click=move |_| {
                        let url = random_url();
                        if copy_to_clipboard(&url) {
                            notify("已复制随机图链接".to_string());
                        } else {
                            notify("复制失败，请手动复制".to_string());
                        }
                    }>"复制链接"</button>
                    <button class="secondary" type="button" on:click=move |_| refresh()>"刷新 URL"</button>
                </div>
                <p class="hint">"提示：每次刷新或重新访问 URL 都会随机返回符合标签条件的图片。"</p>
            </div>
        </div>
    }
}

#[component]
fn Gallery(
    token: RwSignal<String>,
    selected_image: RwSignal<Option<String>>,
    set_view: impl Fn(&'static str) + Copy + Send + Sync + 'static,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
    preview_modal: RwSignal<Option<ImageLinkBundle>>,
    links_modal: RwSignal<Option<ImageLinkBundle>>,
) -> impl IntoView {
    let images = RwSignal::new(Vec::<ImageSummary>::new());
    let status = RwSignal::new("active".to_string());
    let tag = NodeRef::<Input>::new();

    let load = move || {
        let token_value = token.get();
        if token_value.is_empty() {
            notify("请先登录".to_string());
            return;
        }
        let mut path = format!(
            "/api/images?page_size=60&status={}",
            urlencoding::encode(&status.get())
        );
        if let Some(value) = input_value(tag).filter(|value| !value.is_empty()) {
            path.push_str("&tag=");
            path.push_str(&urlencoding::encode(&value));
        }
        spawn_local(async move {
            match api_get::<Page<ImageSummary>>(&path, &token_value).await {
                Ok(page) => images.set(page.items),
                Err(err) => notify(err),
            }
        });
    };

    let trash = move |id: String| {
        if !confirm("确认删除到回收站？") {
            return;
        }
        let token_value = token.get();
        spawn_local(async move {
            match api_empty(&format!("/api/images/{id}"), &token_value, "DELETE").await {
                Ok(_) => load(),
                Err(err) => notify(err),
            }
        });
    };

    view! {
        <div class="panel">
            <header class="section-head">
                <h2>"我的图片"</h2>
                <div class="filters">
                    <input node_ref=tag placeholder="标签"/>
                    <select on:change=move |event| status.set(event_target_value(&event))>
                        <option value="active">"正常"</option>
                        <option value="pending_review">"待审核"</option>
                        <option value="rejected">"已拒绝"</option>
                        <option value="">"全部"</option>
                    </select>
                    <button type="button" on:click=move |_| load()>"刷新"</button>
                </div>
            </header>
            <div class="image-grid">
                {move || if images.get().is_empty() {
                    view! { <div class="empty">"没有符合条件的图片"</div> }.into_any()
                } else {
                    images.get().into_iter().map(|image| {
                        let detail_id = image.id.to_string();
                        let open_detail_id = detail_id.clone();
                        let trash_id = detail_id.clone();
                        let bundle = ImageLinkBundle::from_summary(&image);
                        let preview_bundle = bundle.clone();
                        let links_bundle_overlay = bundle.clone();
                        let links_bundle_meta = bundle.clone();
                        let original_url = bundle.url.clone();
                        let image_ratio = image_aspect_ratio(image.width, image.height);
                        view! {
                            <article class="image-card">
                                <button class="image-pick" type="button" style=image_ratio on:click=move |_| {
                                    preview_modal.set(Some(preview_bundle.clone()));
                                }>
                                    <ImageThumb src=bundle.preview_src() alt=bundle.alt.clone()/>
                                    <div class="image-overlay">
                                        <div class="image-overlay-actions">
                                            <button type="button" class="overlay-btn" on:click=move |event| {
                                                event.stop_propagation();
                                                selected_image.set(Some(open_detail_id.clone()));
                                                storage_set("tide_selected_image", &open_detail_id);
                                                set_view("detail");
                                            }>"详情"</button>
                                            <button type="button" class="overlay-btn" on:click=move |event| {
                                                event.stop_propagation();
                                                links_modal.set(Some(links_bundle_overlay.clone()));
                                            }>"链接"</button>
                                        </div>
                                        <div class="image-overlay-info">
                                            {format!("{}×{}", image.width, image.height)}
                                            <span class="dot-sep">"·"</span>
                                            {format_bytes(image.size)}
                                        </div>
                                    </div>
                                </button>
                                <div class="image-meta">
                                    <strong>{image.title.clone()}</strong>
                                    <span class="image-tags">{if image.tags.is_empty() { "无标签".to_string() } else { image.tags.join(" · ") }}</span>
                                    <div class="row-actions">
                                        <button type="button" class="chip-btn" on:click=move |_| {
                                            if copy_to_clipboard(&original_url) {
                                                notify("已复制原图链接".to_string());
                                            } else {
                                                notify("复制失败，请手动复制".to_string());
                                            }
                                        }>"复制链接"</button>
                                        <button type="button" class="chip-btn" on:click=move |_| links_modal.set(Some(links_bundle_meta.clone()))>"部署引用"</button>
                                        <button type="button" class="chip-btn danger" on:click=move |_| trash(trash_id.clone())>"删除"</button>
                                    </div>
                                </div>
                            </article>
                        }
                    }).collect_view().into_any()
                }}
            </div>
        </div>
    }
}

#[component]
fn ImageDetail(
    token: RwSignal<String>,
    selected_image: RwSignal<Option<String>>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
    preview_modal: RwSignal<Option<ImageLinkBundle>>,
    links_modal: RwSignal<Option<ImageLinkBundle>>,
) -> impl IntoView {
    let detail = RwSignal::new(None::<ImageSummary>);
    let links = RwSignal::new(None::<ImageLinks>);
    let title = NodeRef::<Input>::new();
    let description = NodeRef::<Textarea>::new();
    let visibility = NodeRef::<Select>::new();
    let tags = NodeRef::<Input>::new();

    let load = move || {
        let token_value = token.get();
        let Some(id) = selected_image.get() else {
            return;
        };
        spawn_local(async move {
            match api_get::<ImageSummary>(&format!("/api/images/{id}"), &token_value).await {
                Ok(image) => {
                    title_value(title, &image.title);
                    textarea_value(description, &image.description);
                    select_set_value(visibility, &image.visibility);
                    title_value(tags, &image.tags.join(","));
                    detail.set(Some(image));
                }
                Err(err) => notify(err),
            }
            if let Ok(value) =
                api_get::<ImageLinks>(&format!("/api/images/{id}/links"), &token_value).await
            {
                links.set(Some(value));
            }
        });
    };

    Effect::new(move |_| {
        selected_image.get();
        load();
    });

    let save = move |event: SubmitEvent| {
        event.prevent_default();
        let token_value = token.get();
        let Some(id) = selected_image.get() else {
            notify("请先选择图片".to_string());
            return;
        };
        let body = json!({
            "title": input_value(title).unwrap_or_default(),
            "description": textarea_current_value(description).unwrap_or_default(),
            "visibility": select_value(visibility).unwrap_or_else(|| "public".to_string()),
        });
        let tag_body = json!({
            "tags": input_value(tags)
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
        });
        spawn_local(async move {
            match api_json::<Value, _>(&format!("/api/images/{id}"), &token_value, "PUT", &body)
                .await
            {
                Ok(_) => {
                    let _ = api_json::<Value, _>(
                        &format!("/api/images/{id}/tags"),
                        &token_value,
                        "POST",
                        &tag_body,
                    )
                    .await;
                    notify("已保存".to_string());
                    load();
                }
                Err(err) => notify(err),
            }
        });
    };

    view! {
        <div class="panel">
            <header class="section-head">
                <h2>"图片详情"</h2>
                <button type="button" on:click=move |_| load()>"刷新"</button>
            </header>
            {move || match detail.get() {
                Some(image) => view! {
                    <div class="detail-grid">
                        <button class="detail-image-button" type="button" on:click={
                            let image = image.clone();
                            move |_| preview_modal.set(Some(ImageLinkBundle::from_summary(&image)))
                        }>
                            <ImageThumb src=image.preview_url.clone() alt=image.original_name.clone()/>
                        </button>
                        <form class="form" on:submit=save>
                            <input node_ref=title placeholder="标题"/>
                            <textarea node_ref=description placeholder="描述"></textarea>
                            <select node_ref=visibility>
                                <option value="public">"公开"</option>
                                <option value="private">"私有"</option>
                                <option value="unlisted">"隐藏"</option>
                            </select>
                            <input node_ref=tags placeholder="标签"/>
                            <div class="actions">
                                <button type="submit">"保存"</button>
                                <button type="button" on:click=move |_| {
                                    if let (Some(value), Some(image)) = (links.get(), detail.get()) {
                                        links_modal.set(Some(ImageLinkBundle::from_links(&image.title, &image.original_name, &value)));
                                        notify("已打开部署引用".to_string());
                                    }
                                }>"部署引用"</button>
                            </div>
                            <div class="info-grid">
                                <span><small>"图片编号"</small><strong>{image.id.to_string()}</strong></span>
                                <span><small>"文件名"</small><strong>{image.original_name.clone()}</strong></span>
                                <span><small>"状态"</small><strong>{status_label(&image.status)}</strong></span>
                                <span><small>"可见性"</small><strong>{visibility_label(&image.visibility)}</strong></span>
                                <span><small>"尺寸"</small><strong>{format!("{} x {}", image.width, image.height)}</strong></span>
                                <span><small>"大小"</small><strong>{format_bytes(image.size)}</strong></span>
                                <span><small>"方向"</small><strong>{orientation_label(&image.orientation)}</strong></span>
                                <span><small>"引用数"</small><strong>{image.ref_count}</strong></span>
                                <span class="wide"><small>"SHA256"</small><strong>{image.sha256.clone()}</strong></span>
                                <span class="wide"><small>"标签"</small><strong>{if image.tags.is_empty() { "无标签".to_string() } else { image.tags.join("，") }}</strong></span>
                            </div>
                        </form>
                    </div>
                }.into_any(),
                None => view! { <div class="detail-empty">"从图片列表选择一张图片。"</div> }.into_any(),
            }}
        </div>
    }
}

#[component]
fn Trash(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
    preview_modal: RwSignal<Option<ImageLinkBundle>>,
    links_modal: RwSignal<Option<ImageLinkBundle>>,
) -> impl IntoView {
    let images = RwSignal::new(Vec::<ImageSummary>::new());
    let load = move || {
        let token_value = token.get();
        spawn_local(async move {
            match api_get::<Page<ImageSummary>>(
                "/api/images?status=trashed&page_size=60",
                &token_value,
            )
            .await
            {
                Ok(page) => images.set(page.items),
                Err(err) => notify(err),
            }
        });
    };
    Effect::new(move |_| load());
    let restore = move |id: String| {
        let token_value = token.get();
        spawn_local(async move {
            match api_empty(&format!("/api/images/{id}/restore"), &token_value, "POST").await {
                Ok(_) => load(),
                Err(err) => notify(err),
            }
        });
    };
    let permanent = move |id: String| {
        if !confirm("确认永久删除？") {
            return;
        }
        let token_value = token.get();
        spawn_local(async move {
            match api_empty(
                &format!("/api/images/{id}/permanent"),
                &token_value,
                "DELETE",
            )
            .await
            {
                Ok(_) => load(),
                Err(err) => notify(err),
            }
        });
    };
    view! {
        <div class="panel">
            <header class="section-head">
                <h2>"我的回收站"</h2>
                <button type="button" on:click=move |_| load()>"刷新"</button>
            </header>
            <div class="image-grid">
                {move || if images.get().is_empty() {
                    view! { <div class="empty">"回收站为空"</div> }.into_any()
                } else {
                    images.get().into_iter().map(|image| {
                        let id = image.id.to_string();
                        let restore_id = id.clone();
                        let bundle = ImageLinkBundle::from_summary(&image);
                        let preview_bundle = bundle.clone();
                        let links_bundle = bundle.clone();
                        let image_ratio = image_aspect_ratio(image.width, image.height);
                        view! {
                            <article class="image-card">
                                <button class="image-pick" type="button" style=image_ratio on:click=move |_| {
                                    preview_modal.set(Some(preview_bundle.clone()));
                                }>
                                    <ImageThumb src=bundle.preview_src() alt=bundle.alt.clone()/>
                                    <div class="image-overlay">
                                        <div class="image-overlay-info">
                                            {format!("{}×{}", image.width, image.height)}
                                            <span class="dot-sep">"·"</span>
                                            {status_label(&image.status)}
                                        </div>
                                    </div>
                                </button>
                                <div class="image-meta">
                                    <strong>{image.original_name.clone()}</strong>
                                    <div class="row-actions">
                                        <button type="button" class="chip-btn" on:click=move |_| links_modal.set(Some(links_bundle.clone()))>"部署引用"</button>
                                        <button type="button" class="chip-btn" on:click=move |_| restore(restore_id.clone())>"恢复"</button>
                                        <button type="button" class="chip-btn danger" on:click=move |_| permanent(id.clone())>"永久删除"</button>
                                    </div>
                                </div>
                            </article>
                        }
                    }).collect_view().into_any()
                }}
            </div>
        </div>
    }
}

#[component]
fn Profile(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let quota_summary = RwSignal::new(None::<QuotaView>);
    let username = NodeRef::<Input>::new();
    let display_name = NodeRef::<Input>::new();
    let bio = NodeRef::<Textarea>::new();
    let avatar_file = NodeRef::<Input>::new();
    let avatar_url = NodeRef::<Input>::new();
    let old_password = NodeRef::<Input>::new();
    let new_password = NodeRef::<Input>::new();
    let avatar_preview = RwSignal::new(None::<String>);
    let load = move || {
        let token_value = token.get();
        if token_value.is_empty() {
            return;
        }
        spawn_local(async move {
            let profile = api_get::<Value>("/api/user/profile", &token_value).await;
            let quota = api_get::<QuotaView>("/api/user/quota", &token_value).await;
            if let (Ok(profile), Ok(quota)) = (profile, quota) {
                let avatar = profile
                    .get("avatar_url")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(|value| value.to_string());
                avatar_preview.set(avatar.clone());
                title_value(
                    username,
                    profile
                        .get("username")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                );
                title_value(
                    display_name,
                    profile
                        .get("profile")
                        .and_then(|value| value.get("display_name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                );
                textarea_value(
                    bio,
                    profile
                        .get("profile")
                        .and_then(|value| value.get("bio"))
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                );
                title_value(
                    avatar_url,
                    &avatar.unwrap_or_default(),
                );
                quota_summary.set(Some(quota));
            }
        });
    };
    Effect::new(move |_| load());

    let on_file_selected = move |_| {
        if let Some(input) = avatar_file.get()
            && let Some(files) = input.files()
            && files.length() > 0
        {
            if let Some(file) = files.get(0) {
                if let Ok(url) = web_sys::Url::create_object_url_with_blob(&file) {
                    avatar_preview.set(Some(url));
                }
            }
        }
    };

    let save = move |event: SubmitEvent| {
        event.prevent_default();
        let token_value = token.get();
        let body = json!({
            "username": input_value(username).unwrap_or_default(),
            "display_name": input_value(display_name).unwrap_or_default(),
            "bio": textarea_current_value(bio).unwrap_or_default(),
        });
        spawn_local(async move {
            match api_json::<Value, _>("/api/user/profile", &token_value, "PUT", &body).await {
                Ok(_) => {
                    if let Some(input) = avatar_file.get()
                        && let Some(files) = input.files()
                        && files.length() > 0
                    {
                        let _ = upload_avatar(&token_value, files).await;
                    } else if let Some(value) =
                        input_value(avatar_url).filter(|value| !value.is_empty())
                    {
                        let _ = api_json::<Value, _>(
                            "/api/user/avatar",
                            &token_value,
                            "POST",
                            &json!({"avatar_url": value}),
                        )
                        .await;
                    }
                    notify("资料已保存".to_string());
                    load();
                }
                Err(err) => notify(err),
            }
        });
    };

    let change_password = move |event: SubmitEvent| {
        event.prevent_default();
        let token_value = token.get();
        let body = json!({
            "old_password": input_value(old_password).unwrap_or_default(),
            "new_password": input_value(new_password).unwrap_or_default(),
        });
        spawn_local(async move {
            match api_json::<Value, _>("/api/user/password", &token_value, "PUT", &body).await {
                Ok(_) => notify("密码已修改".to_string()),
                Err(err) => notify(err),
            }
        });
    };

    view! {
        <div class="grid two">
            <form class="panel form" on:submit=save>
                <h2>"个人资料"</h2>
                <div class="avatar-section">
                    <label class="avatar-upload-area" for="avatar-file">
                        {move || match avatar_preview.get() {
                            Some(src) => view! {
                                <img class="avatar-preview" src=src alt="头像预览"/>
                            }.into_any(),
                            None => view! {
                                <div class="avatar-preview avatar-placeholder">"👤"</div>
                            }.into_any(),
                        }}
                        <span class="avatar-overlay">
                            <span>"点击更换头像"</span>
                        </span>
                    </label>
                    <input
                        id="avatar-file"
                        node_ref=avatar_file
                        type="file"
                        accept="image/*"
                        on:change=on_file_selected
                    />
                    <p class="avatar-hint">"上传的图片会作为你的账号头像显示在管理后台右上角。"</p>
                </div>
                <input node_ref=username placeholder="用户名"/>
                <input node_ref=display_name placeholder="显示名"/>
                <textarea node_ref=bio placeholder="简介"></textarea>
                <input node_ref=avatar_url placeholder="或填写头像链接"/>
                <button type="submit">"保存资料"</button>
            </form>
            <form class="panel form" on:submit=change_password>
                <h2>"密码与配额"</h2>
                <input node_ref=old_password type="password" placeholder="旧密码"/>
                <input node_ref=new_password type="password" placeholder="新密码"/>
                <button type="submit">"修改密码"</button>
                {move || match quota_summary.get() {
                    Some(quota) => view! {
                        <div class="info-grid">
                            <span><small>"用户组"</small><strong>{role_label(&quota.group_code)}</strong></span>
                            <span><small>"今日剩余"</small><strong>{format!("{} 张", quota.remaining_count_today)}</strong></span>
                            <span><small>"今日容量"</small><strong>{format_bytes(quota.remaining_bytes_today)}</strong></span>
                            <span><small>"总容量余量"</small><strong>{format_bytes(quota.remaining_storage_bytes)}</strong></span>
                            <span><small>"单文件上限"</small><strong>{format_bytes(quota.max_file_size)}</strong></span>
                            <span><small>"审核策略"</small><strong>{if quota.require_review { "需要审核" } else { "免审核" }}</strong></span>
                            <span><small>"批量上传"</small><strong>{if quota.allow_batch_upload { "允许" } else { "关闭" }}</strong></span>
                        </div>
                    }.into_any(),
                    None => view! { <div class="empty-inline">"配额信息加载中"</div> }.into_any(),
                }}
            </form>
        </div>
    }
}

#[component]
fn ApiTokens(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let list = RwSignal::new(Vec::<Value>::new());
    let name = NodeRef::<Input>::new();
    let ip_whitelist = NodeRef::<Input>::new();
    let expires_at = NodeRef::<Input>::new();
    let scope_upload = NodeRef::<Input>::new();
    let scope_read = NodeRef::<Input>::new();
    let scope_delete = NodeRef::<Input>::new();
    let scope_random = NodeRef::<Input>::new();
    let load = move || {
        let token_value = token.get();
        spawn_local(async move {
            match api_get::<Vec<Value>>("/api/user/api-tokens", &token_value).await {
                Ok(value) => list.set(value),
                Err(err) => notify(err),
            }
        });
    };
    Effect::new(move |_| load());
    let create = move |event: SubmitEvent| {
        event.prevent_default();
        let token_value = token.get();
        let scopes = selected_token_scopes(
            checked_value(scope_upload),
            checked_value(scope_read),
            checked_value(scope_delete),
            checked_value(scope_random),
        );
        let body = json!({
            "name": input_value(name).filter(|value| !value.is_empty()).unwrap_or_else(|| "API Token".to_string()),
            "scopes": scopes,
            "ip_whitelist": parse_comma_values(&input_value(ip_whitelist).unwrap_or_default()),
            "expires_at": datetime_local_to_rfc3339(&input_value(expires_at).unwrap_or_default()),
        });
        spawn_local(async move {
            match api_json::<Value, _>("/api/user/api-tokens", &token_value, "POST", &body).await {
                Ok(value) => {
                    if let Some(value) = value.get("token").and_then(Value::as_str) {
                        if copy_to_clipboard(value) {
                            notify("Token 已创建并复制".to_string());
                        } else {
                            notify("Token 已创建但复制失败".to_string());
                        }
                    } else {
                        notify("Token 已创建".to_string());
                    }
                    load();
                }
                Err(err) => notify(err),
            }
        });
    };
    let delete_token = move |id: String| {
        if !confirm("确认删除这个 API Token？") {
            return;
        }
        let token_value = token.get();
        spawn_local(async move {
            match api_empty(
                &format!("/api/user/api-tokens/{id}"),
                &token_value,
                "DELETE",
            )
            .await
            {
                Ok(_) => load(),
                Err(err) => notify(err),
            }
        });
    };
    view! {
        <div class="panel">
            <header class="section-head">
                <div>
                    <h2>"API Token"</h2>
                </div>
                <button type="button" on:click=move |_| load()>"刷新"</button>
            </header>
            <div class="grid">
                <section>
                    <h3>"创建 Token"</h3>
                    <form class="token-form" on:submit=create>
                        <input node_ref=name placeholder="Token 名称"/>
                        <input node_ref=ip_whitelist placeholder="地址白名单，逗号分隔，可留空"/>
                        <input node_ref=expires_at type="datetime-local" placeholder="过期时间"/>
                        <label><input node_ref=scope_upload type="checkbox" checked/>"上传"</label>
                        <label><input node_ref=scope_read type="checkbox" checked/>"读取"</label>
                        <label><input node_ref=scope_delete type="checkbox" checked/>"删除"</label>
                        <label><input node_ref=scope_random type="checkbox" checked/>"随机图"</label>
                        <button type="submit">"创建 Token"</button>
                    </form>
                    <div class="token-examples">
                        <strong>"可授权能力"</strong>
                        <span>"上传图片"</span>
                        <span>"读取图片与随机图"</span>
                        <span>"删除本人图片"</span>
                    </div>
                </section>
                <section>
                    <h3>"Token 列表"</h3>
                    <div class="list">
                        {move || if list.get().is_empty() {
                            view! { <div class="empty">"还没有 API Token"</div> }.into_any()
                        } else {
                            list.get().into_iter().map(|item| {
                                let id = json_string(&item, "id");
                                view! {
                                    <article class="list-row">
                                        <span>
                                            <strong>{json_string(&item, "name")}</strong>
                                            <small>{format!(
                                                "权限 {} · 地址 {} · 过期 {}",
                                                token_scope_label(item.get("scopes").unwrap_or(&Value::Null)),
                                                token_ip_label(item.get("ip_whitelist").unwrap_or(&Value::Null)),
                                                token_expiration_label(item.get("expires_at").unwrap_or(&Value::Null)),
                                            )}</small>
                                        </span>
                                        <button type="button" on:click=move |_| delete_token(id.clone())>"删除"</button>
                                    </article>
                                }
                            }).collect_view().into_any()
                        }}
                    </div>
                </section>
            </div>
        </div>
    }
}

#[component]
fn AdminConsole(
    token: RwSignal<String>,
    admin_tab: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
    theme: RwSignal<ThemeSettings>,
    current_user: RwSignal<Option<AuthUserView>>,
    set_view: impl Fn(&'static str) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let drawer_open = RwSignal::new(false);
    let tab = move |key: &'static str, title: &'static str| {
        view! {
            <button
                data-admin=key
                class=move || if admin_tab.get() == key { "selected" } else { "" }
                on:click=move |_| {
                    admin_tab.set(key.to_string());
                    drawer_open.set(false);
                }
            >
                {title}
            </button>
        }
    };
    view! {
        <div class=move || if drawer_open.get() { "admin-shell drawer-open" } else { "admin-shell" }>
            <header class="admin-topbar">
                <button class="admin-brand-button" type="button" aria-label="返回首页" on:click=move |_| set_view("upload")>
                    <span class="brand-mark">"潮"</span>
                </button>
                <div>
                    <strong>"管理后台"</strong>
                    <small>{move || admin_title(&admin_tab.get())}</small>
                </div>
                <div class="admin-top-actions">
                    {move || current_user.get().map(|user| view! {
                        <span class="user-pill admin-user-pill">
                            {if let Some(url) = user.avatar_url.as_ref().filter(|value| !value.is_empty()) {
                                view! { <img class="admin-avatar" src=url.clone() alt="头像"/> }.into_any()
                            } else {
                                view! { <span class="admin-avatar-placeholder">"👤"</span> }.into_any()
                            }}
                            <span class="admin-user-name">{user.username.clone()}</span>
                        </span>
                    })}
                    <button id="adminMenuToggle" class="admin-menu-toggle" type="button" on:click=move |_| drawer_open.update(|value| *value = !*value)>"菜单"</button>
                </div>
            </header>
            <button
                id="adminBackdrop"
                class="admin-backdrop"
                type="button"
                aria-label="关闭后台菜单"
                on:click=move |_| drawer_open.set(false)
            ></button>
            <div class="admin-layout">
                <nav class="admin-tabs">
                    {tab("dashboard", "仪表盘")}
                    {tab("images", "图片与标签")}
                    {tab("users", "用户与配额")}
                    {tab("audit", "审核管理")}
                    {tab("storage", "存储与迁移")}
                    {tab("settings", "系统设置")}
                </nav>
                <section class="admin-main panel">
                    <header class="section-head admin-section-head">
                        <div>
                            <h2 id="adminTitle">{move || admin_title(&admin_tab.get())}</h2>
                            <p>{move || admin_subtitle(&admin_tab.get())}</p>
                        </div>
                    </header>
                    {move || match admin_tab.get().as_str() {
                        "dashboard" => view! { <AdminDashboard token=token notify=notify/> }.into_any(),
                        "images" => view! { <AdminContentHub token=token notify=notify/> }.into_any(),
                        "users" => view! { <AdminUsersHub token=token admin_tab=admin_tab notify=notify/> }.into_any(),
                        "audit" => view! { <AdminAudit token=token notify=notify/> }.into_any(),
                        "storage" => view! { <AdminStorageHub token=token notify=notify/> }.into_any(),
                        "settings" => view! { <AdminSystemSettings token=token notify=notify theme=theme/> }.into_any(),
                        _ => view! { <div class="empty">"请选择后台功能"</div> }.into_any(),
                    }}
                </section>
            </div>
        </div>
    }
}

#[component]
fn AdminContentHub(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let section = RwSignal::new("images".to_string());
    let item = move |key: &'static str, title: &'static str| {
        view! {
            <button
                type="button"
                class=move || if section.get() == key { "selected" } else { "" }
                on:click=move |_| section.set(key.to_string())
            >
                {title}
            </button>
        }
    };
    view! {
        <div class="admin-hub">
            <div class="admin-subtabs">
                {item("images", "图片管理")}
                {item("tags", "标签策略")}
            </div>
            {move || match section.get().as_str() {
                "tags" => view! {
                    <>
                        <AdminHubHeader title="标签策略" description="标签能力保留在内容运营中，支持查看使用量、禁用违规标签，并继续用于图库筛选。"/>
                        <AdminTags token=token notify=notify/>
                    </>
                }.into_any(),
                _ => view! {
                    <>
                        <AdminHubHeader title="图片管理" description="统一处理全站图片、访客上传、审核状态、存储位置、标签筛选和隔离操作。"/>
                        <AdminImages token=token notify=notify/>
                    </>
                }.into_any(),
            }}
        </div>
    }
}

#[component]
fn AdminUsersHub(
    token: RwSignal<String>,
    admin_tab: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let section = RwSignal::new("users".to_string());
    let item = move |key: &'static str, title: &'static str| {
        view! {
            <button
                type="button"
                class=move || if section.get() == key { "selected" } else { "" }
                on:click=move |_| section.set(key.to_string())
            >
                {title}
            </button>
        }
    };
    view! {
        <div class="admin-hub">
            <div class="admin-subtabs">
                {item("users", "用户")}
                {item("quota", "用户组与配额")}
            </div>
            {move || match section.get().as_str() {
                "quota" => view! {
                    <>
                        <AdminHubHeader title="用户组与配额" description="集中维护用户组、上传额度、API 调用、随机图调用、审核策略和默认存储位置。"/>
                        <AdminQuota token=token notify=notify/>
                    </>
                }.into_any(),
                _ => view! {
                    <>
                        <AdminHubHeader title="用户管理" description="搜索用户、创建账号、调整用户组、封禁解封，并可跳转查看用户图片。"/>
                        <AdminUsers token=token admin_tab=admin_tab notify=notify/>
                    </>
                }.into_any(),
            }}
        </div>
    }
}

#[component]
fn AdminStorageHub(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let section = RwSignal::new("providers".to_string());
    let item = move |key: &'static str, title: &'static str| {
        view! {
            <button
                type="button"
                class=move || if section.get() == key { "selected" } else { "" }
                on:click=move |_| section.set(key.to_string())
            >
                {title}
            </button>
        }
    };
    view! {
        <div class="admin-hub">
            <div class="admin-subtabs">
                {item("providers", "存储配置")}
                {item("migrations", "迁移任务")}
            </div>
            {move || match section.get().as_str() {
                "migrations" => view! {
                    <>
                        <AdminHubHeader title="图片迁移" description="创建、暂停、继续、取消和追踪跨存储迁移任务，失败项可按任务重试。"/>
                        <AdminMigrations token=token notify=notify/>
                    </>
                }.into_any(),
                _ => view! {
                    <>
                        <AdminHubHeader title="存储配置" description="维护本地、R2、OneDrive、Oracle 和 S3 兼容 provider，并配置上传路由与健康检查。"/>
                        <AdminStorage token=token notify=notify/>
                    </>
                }.into_any(),
            }}
        </div>
    }
}

#[component]
fn AdminHubHeader(title: &'static str, description: &'static str) -> impl IntoView {
    view! {
        <div class="admin-hub-header">
            <h3>{title}</h3>
            <p>{description}</p>
        </div>
    }
}

#[component]
fn AdminDashboard(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let data = RwSignal::new(json!({}));
    let loading = RwSignal::new(false);
    let load = move || {
        let token_value = token.get();
        loading.set(true);
        spawn_local(async move {
            match api_get::<Value>("/api/admin/dashboard", &token_value).await {
                Ok(value) => data.set(value),
                Err(_) => notify("仪表盘加载失败".to_string()),
            }
            loading.set(false);
        });
    };
    Effect::new(move |_| {
        load();
    });
    view! {
        <div class="admin-stack">
            <div class="admin-toolbar">
                <button type="button" on:click=move |_| load()>"刷新仪表盘"</button>
                <span class="muted">{move || if loading.get() { "加载中..." } else { "" }}</span>
            </div>
            <Show when=move || loading.get() && data.get().as_object().is_some_and(|object| object.is_empty())>
                <div class="dashboard-skeleton">
                    <span class="skeleton"></span>
                    <span class="skeleton"></span>
                    <span class="skeleton"></span>
                    <span class="skeleton"></span>
                    <span class="skeleton"></span>
                    <span class="skeleton"></span>
                </div>
            </Show>
            <div class="stats admin-stats">
                <span><strong>{move || dashboard_users(&data.get()).to_string()}</strong>"用户总数"</span>
                <span><strong>{move || dashboard_today_users(&data.get()).to_string()}</strong>"今日注册"</span>
                <span><strong>{move || dashboard_images_total(&data.get()).to_string()}</strong>"图片总数"</span>
                <span><strong>{move || dashboard_today_images(&data.get()).to_string()}</strong>"今日上传"</span>
                <span><strong>{move || format_bytes(dashboard_storage_bytes(&data.get()))}</strong>"存储占用"</span>
                <span><strong>{move || dashboard_pending_audit(&data.get()).to_string()}</strong>"待审核"</span>
                <span><strong>{move || dashboard_storage_health(&data.get())}</strong>"存储健康"</span>
            </div>
            <div class="grid two">
                <section class="admin-section">
                    <h3>"最近上传"</h3>
                    <div class="admin-table-wrap">
                        <table class="admin-table">
                            <thead><tr><th>"图片"</th><th>"状态"</th><th>"尺寸"</th><th>"时间"</th></tr></thead>
                            <tbody>
                                {move || {
                                    let items = json_array(&data.get(), "recent_images");
                                    if items.is_empty() {
                                        view! { <tr><td colspan="4">"暂无近期上传"</td></tr> }.into_any()
                                    } else {
                                        items.into_iter().take(8).map(|item| view! {
                                    <tr>
                                        <td data-label="图片">{json_string(&item, "original_name")}</td>
                                        <td data-label="状态"><StatusBadge value=json_string(&item, "status")/></td>
                                        <td data-label="尺寸">{format!("{} x {}", json_i64(&item, "width"), json_i64(&item, "height"))}</td>
                                        <td data-label="时间">{time_label(&json_string(&item, "created_at"))}</td>
                                    </tr>
                                        }).collect_view().into_any()
                                    }
                                }}
                            </tbody>
                        </table>
                    </div>
                </section>
                <section class="admin-section">
                    <h3>"最近封禁"</h3>
                    <div class="admin-table-wrap">
                        <table class="admin-table">
                            <thead><tr><th>"用户"</th><th>"邮箱"</th><th>"用户组"</th><th>"时间"</th></tr></thead>
                            <tbody>
                                {move || {
                                    let items = json_array(&data.get(), "recent_bans");
                                    if items.is_empty() {
                                        view! { <tr><td colspan="4">"暂无近期封禁"</td></tr> }.into_any()
                                    } else {
                                        items.into_iter().map(|item| view! {
                                    <tr>
                                        <td data-label="用户">{json_string(&item, "username")}</td>
                                        <td data-label="邮箱">{json_string(&item, "email")}</td>
                                        <td data-label="用户组">{role_label(&json_string(&item, "role"))}</td>
                                        <td data-label="时间">{time_label(&json_string(&item, "updated_at"))}</td>
                                    </tr>
                                        }).collect_view().into_any()
                                    }
                                }}
                            </tbody>
                        </table>
                    </div>
                </section>
                <section class="admin-section">
                    <h3>"存储健康"</h3>
                    <div class="admin-table-wrap">
                        <table class="admin-table">
                            <thead><tr><th>"名称"</th><th>"类型"</th><th>"状态"</th></tr></thead>
                            <tbody>
                                {move || {
                                    let items = json_array(&data.get(), "storage");
                                    if items.is_empty() {
                                        view! { <tr><td colspan="3">"暂无存储健康数据"</td></tr> }.into_any()
                                    } else {
                                        items.into_iter().map(|item| view! {
                                    <tr>
                                        <td data-label="名称">{json_string(&item, "name")}</td>
                                        <td data-label="类型">{storage_type_label(&json_string(&item, "provider_type"))}</td>
                                        <td data-label="状态">{if item.get("healthy").and_then(Value::as_bool).unwrap_or(false) { "健康" } else { "异常" }}</td>
                                    </tr>
                                        }).collect_view().into_any()
                                    }
                                }}
                            </tbody>
                        </table>
                    </div>
                </section>
            </div>
        </div>
    }
}

#[component]
fn AdminUsers(
    token: RwSignal<String>,
    admin_tab: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let users = RwSignal::new(Vec::<Value>::new());
    let page = RwSignal::new(1_i64);
    let page_size = RwSignal::new(40_i64);
    let total = RwSignal::new(0_i64);
    let loading = RwSignal::new(false);
    let detail = RwSignal::new(None::<Value>);
    let email = NodeRef::<Input>::new();
    let username = NodeRef::<Input>::new();
    let password = NodeRef::<Input>::new();
    let role = NodeRef::<Select>::new();
    let filter_q = NodeRef::<Input>::new();
    let filter_role = NodeRef::<Select>::new();
    let filter_status = NodeRef::<Select>::new();
    let target_user = NodeRef::<Input>::new();
    let target_role = NodeRef::<Select>::new();
    let quota_daily_upload_count = NodeRef::<Input>::new();
    let quota_daily_upload_bytes = NodeRef::<Input>::new();
    let quota_max_file_size = NodeRef::<Input>::new();
    let quota_total_storage_bytes = NodeRef::<Input>::new();
    let quota_daily_api_calls = NodeRef::<Input>::new();
    let quota_daily_random_calls = NodeRef::<Input>::new();
    let quota_require_review = NodeRef::<Input>::new();
    let load = move || {
        let token_value = token.get();
        let path = admin_page_path(
            "/api/admin/users/page",
            page.get(),
            page_size.get(),
            &[
                ("q", input_value(filter_q).unwrap_or_default()),
                ("role", select_value(filter_role).unwrap_or_default()),
                ("status", select_value(filter_status).unwrap_or_default()),
            ],
        );
        loading.set(true);
        spawn_local(async move {
            match api_get::<Page<Value>>(&path, &token_value).await {
                Ok(value) => {
                    total.set(value.total);
                    users.set(value.items);
                }
                Err(err) => notify(err),
            }
            loading.set(false);
        });
    };
    Effect::new(move |_| load());
    let apply_filters = move |_| {
        page.set(1);
        load();
    };
    let select_user = move |id: String| {
        title_value(target_user, &id);
        let token_value = token.get();
        spawn_local(async move {
            match api_get::<Value>(&format!("/api/admin/users/{id}"), &token_value).await {
                Ok(value) => detail.set(Some(value)),
                Err(err) => notify(err),
            }
        });
    };
    let create = move |event: SubmitEvent| {
        event.prevent_default();
        let token_value = token.get();
        let body = json!({
            "email": input_value(email).unwrap_or_default(),
            "username": input_value(username).unwrap_or_default(),
            "password": input_value(password).unwrap_or_default(),
            "role": select_value(role).unwrap_or_else(|| "user".to_string()),
        });
        spawn_local(async move {
            match api_json::<Value, _>("/api/admin/users", &token_value, "POST", &body).await {
                Ok(_) => load(),
                Err(err) => notify(err),
            }
        });
    };
    let user_action = move |id: String, action: &'static str| {
        if matches!(action, "ban" | "delete") && !confirm("确认执行这个用户操作？") {
            return;
        }
        let token_value = token.get();
        let path = if action == "delete" {
            format!("/api/admin/users/{id}")
        } else {
            format!("/api/admin/users/{id}/{action}")
        };
        let method = if action == "delete" { "DELETE" } else { "POST" };
        spawn_local(async move {
            match api_empty(&path, &token_value, method).await {
                Ok(_) => load(),
                Err(err) => notify(err),
            }
        });
    };
    let change_group = move |event: SubmitEvent| {
        event.prevent_default();
        let id = input_value(target_user).unwrap_or_default();
        if id.is_empty() {
            notify("请输入用户编号".to_string());
            return;
        }
        let token_value = token.get();
        let body = json!({"role": select_value(target_role).unwrap_or_else(|| "user".to_string())});
        spawn_local(async move {
            match api_json::<Value, _>(
                &format!("/api/admin/users/{id}/group"),
                &token_value,
                "PUT",
                &body,
            )
            .await
            {
                Ok(_) => {
                    notify("用户组已更新".to_string());
                    load();
                }
                Err(err) => notify(err),
            }
        });
    };
    let save_quota_override = move |event: SubmitEvent| {
        event.prevent_default();
        let id = input_value(target_user).unwrap_or_default();
        if id.is_empty() {
            notify("请输入用户编号".to_string());
            return;
        }
        let token_value = token.get();
        let quota = json!({
            "daily_upload_count": input_i32(quota_daily_upload_count),
            "daily_upload_bytes": input_i64(quota_daily_upload_bytes),
            "max_file_size": input_i64(quota_max_file_size),
            "total_storage_bytes": input_i64(quota_total_storage_bytes),
            "daily_api_calls": input_i32(quota_daily_api_calls),
            "daily_random_calls": input_i32(quota_daily_random_calls),
            "require_review": checked_value(quota_require_review),
        });
        let body = json!({"quota": quota, "reason": "后台手动覆盖"});
        spawn_local(async move {
            match api_json::<Value, _>(
                &format!("/api/admin/users/{id}/quota"),
                &token_value,
                "PUT",
                &body,
            )
            .await
            {
                Ok(_) => notify("用户配额覆盖已保存".to_string()),
                Err(err) => notify(err),
            }
        });
    };
    view! {
        <div class="admin-stack">
            <div class="grid two">
                <section class="admin-section">
                    <h3>"创建用户"</h3>
                    <form class="admin-form" on:submit=create>
                        <input node_ref=email placeholder="邮箱"/>
                        <input node_ref=username placeholder="用户名"/>
                        <input node_ref=password type="password" placeholder="密码"/>
                        <select node_ref=role>
                            <option value="user">"普通用户"</option>
                            <option value="trusted">"可信用户"</option>
                            <option value="supporter">"公益支持者"</option>
                            <option value="admin">"管理员"</option>
                        </select>
                        <button type="submit">"创建用户"</button>
                    </form>
                </section>
                <section class="admin-section">
                    <h3>"用户组与配额"</h3>
                    <form class="admin-form" on:submit=change_group>
                        <input node_ref=target_user placeholder="用户编号"/>
                        <select node_ref=target_role>
                            <option value="user">"普通用户"</option>
                            <option value="trusted">"可信用户"</option>
                            <option value="supporter">"公益支持者"</option>
                            <option value="admin">"管理员"</option>
                        </select>
                        <button type="submit">"修改用户组"</button>
                    </form>
                    <form class="admin-form compact-form" on:submit=save_quota_override>
                        <input node_ref=quota_daily_upload_count type="number" placeholder="每日上传数量"/>
                        <input node_ref=quota_daily_upload_bytes type="number" placeholder="每日上传容量，字节"/>
                        <input node_ref=quota_max_file_size type="number" placeholder="单文件大小，字节"/>
                        <input node_ref=quota_total_storage_bytes type="number" placeholder="账号总容量，字节"/>
                        <input node_ref=quota_daily_api_calls type="number" placeholder="每日接口调用"/>
                        <input node_ref=quota_daily_random_calls type="number" placeholder="每日随机图调用"/>
                        <label class="checkbox-row"><input node_ref=quota_require_review type="checkbox"/>"需要审核"</label>
                        <button type="submit">"覆盖用户配额"</button>
                    </form>
                </section>
            </div>
            <div class="admin-toolbar">
                <input node_ref=filter_q placeholder="搜索邮箱 / 用户名 / 编号"/>
                <select node_ref=filter_role>
                    <option value="">"全部用户组"</option>
                    <option value="guest_account">"访客"</option>
                    <option value="user">"普通用户"</option>
                    <option value="trusted">"可信用户"</option>
                    <option value="supporter">"公益支持者"</option>
                    <option value="admin">"管理员"</option>
                    <option value="super_admin">"超级管理员"</option>
                </select>
                <select node_ref=filter_status>
                    <option value="">"全部状态"</option>
                    <option value="active">"正常"</option>
                    <option value="banned">"已封禁"</option>
                    <option value="pending_email">"待验证"</option>
                    <option value="deleted">"已删除"</option>
                </select>
                <button type="button" on:click=apply_filters>"筛选"</button>
                <button type="button" on:click=move |_| load()>"刷新用户"</button>
                <span class="muted">{move || page_summary(page.get(), page_size.get(), total.get(), loading.get())}</span>
            </div>
            <div class="admin-table-wrap">
                <table class="admin-table">
                    <thead>
                        <tr><th>"邮箱"</th><th>"用户名"</th><th>"用户组"</th><th>"状态"</th><th>"创建时间"</th><th>"操作"</th></tr>
                    </thead>
                    <tbody>
                        {move || users.get().into_iter().map(|item| {
                            let id = json_string(&item, "id");
                            let ban_id = id.clone();
                            let unban_id = id.clone();
                            let delete_id = id.clone();
                            let detail_id = id.clone();
                            let images_id = id.clone();
                            view! {
                                <tr>
                                    <td data-label="邮箱"><strong>{json_string(&item, "email")}</strong></td>
                                    <td data-label="用户名">{json_string(&item, "username")}</td>
                                    <td data-label="用户组">{role_label(&json_string(&item, "role"))}</td>
                                    <td data-label="状态"><StatusBadge value=json_string(&item, "status")/></td>
                                    <td data-label="创建时间">{time_label(&json_string(&item, "created_at"))}</td>
                                    <td data-label="操作">
                                        <div class="row-actions">
                                            <button type="button" on:click=move |_| user_action(ban_id.clone(), "ban")>"封禁"</button>
                                            <button type="button" on:click=move |_| user_action(unban_id.clone(), "unban")>"解封"</button>
                                            <button type="button" on:click=move |_| select_user(detail_id.clone())>"详情"</button>
                                            <button type="button" on:click=move |_| {
                                                storage_set("tide_admin_image_user", &images_id);
                                                admin_tab.set("images".to_string());
                                            }>"图片"</button>
                                            <button type="button" on:click=move |_| user_action(delete_id.clone(), "delete")>"删除"</button>
                                        </div>
                                    </td>
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            </div>
            <AdminPager page=page page_size=page_size total=total loading=loading load=load/>
            <Show when=move || users.get().is_empty()>
                <div class="empty">"暂无用户"</div>
            </Show>
            {move || detail.get().map(|user| view! {
                <section class="admin-detail">
                    <header class="section-head">
                        <h3>"用户详情"</h3>
                        <button class="secondary" type="button" on:click=move |_| detail.set(None)>"收起"</button>
                    </header>
                    <div class="detail-fields">
                        <span><small>"编号"</small><strong>{json_string(&user, "id")}</strong></span>
                        <span><small>"邮箱"</small><strong>{json_string(&user, "email")}</strong></span>
                        <span><small>"用户名"</small><strong>{json_string(&user, "username")}</strong></span>
                        <span><small>"用户组"</small><strong>{role_label(&json_string(&user, "role"))}</strong></span>
                        <span><small>"状态"</small><strong>{status_label(&json_string(&user, "status"))}</strong></span>
                    </div>
                </section>
            }).into_any()}
        </div>
    }
}

#[component]
fn AdminImages(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let images = RwSignal::new(Vec::<ImageSummary>::new());
    let detail = RwSignal::new(None::<Value>);
    let page = RwSignal::new(1_i64);
    let page_size = RwSignal::new(40_i64);
    let total = RwSignal::new(0_i64);
    let loading = RwSignal::new(false);
    let status = RwSignal::new(String::new());
    let orientation = RwSignal::new(String::new());
    let guest = RwSignal::new(String::new());
    let tag = NodeRef::<Input>::new();
    let user_id = NodeRef::<Input>::new();
    let storage_id = NodeRef::<Input>::new();
    let min_width = NodeRef::<Input>::new();
    let min_height = NodeRef::<Input>::new();
    let load = move || {
        let token_value = token.get();
        let mut user_filter = input_value(user_id);
        if user_filter.as_deref().unwrap_or_default().is_empty()
            && let Some(value) = storage_get("tide_admin_image_user")
        {
            title_value(user_id, &value);
            user_filter = Some(value);
            storage_delete("tide_admin_image_user");
        }
        let path = admin_image_query_path(AdminImageFilterValues {
            status: status.get(),
            orientation: orientation.get(),
            guest: guest.get(),
            tag: input_value(tag),
            user_id: user_filter,
            storage_id: input_value(storage_id),
            min_width: input_value(min_width),
            min_height: input_value(min_height),
            page: page.get(),
            page_size: page_size.get(),
        });
        loading.set(true);
        spawn_local(async move {
            match api_get::<Page<ImageSummary>>(&path, &token_value).await {
                Ok(value) => {
                    total.set(value.total);
                    images.set(value.items);
                }
                Err(err) => notify(err),
            }
            loading.set(false);
        });
    };
    let show_detail = move |id: String| {
        let token_value = token.get();
        spawn_local(async move {
            match api_get::<Value>(&format!("/api/admin/images/{id}"), &token_value).await {
                Ok(value) => detail.set(Some(localize_admin_image_detail(value))),
                Err(err) => notify(err),
            }
        });
    };
    Effect::new(move |_| load());
    let action = move |id: String, action: &'static str| {
        if matches!(action, "reject" | "block") && !confirm("确认执行图片审核操作？") {
            return;
        }
        let token_value = token.get();
        spawn_local(async move {
            match api_empty(
                &format!("/api/admin/images/{id}/{action}"),
                &token_value,
                "POST",
            )
            .await
            {
                Ok(_) => load(),
                Err(err) => notify(err),
            }
        });
    };
    view! {
        <div class="admin-stack">
            <div class="filters">
                <select on:change=move |event| status.set(event_target_value(&event))>
                    <option value="">"全部状态"</option>
                    <option value="active">"正常"</option>
                    <option value="pending_review">"待审核"</option>
                    <option value="rejected">"已拒绝"</option>
                    <option value="blocked">"已隔离"</option>
                    <option value="trashed">"回收站"</option>
                </select>
                <select on:change=move |event| orientation.set(event_target_value(&event))>
                    <option value="">"全部方向"</option>
                    <option value="landscape">"横图"</option>
                    <option value="portrait">"竖图"</option>
                    <option value="square">"方图"</option>
                </select>
                <select on:change=move |event| guest.set(event_target_value(&event))>
                    <option value="">"全部来源"</option>
                    <option value="true">"访客上传"</option>
                    <option value="false">"登录用户"</option>
                </select>
                <input node_ref=tag placeholder="标签"/>
                <input node_ref=user_id placeholder="用户编号"/>
                <input node_ref=storage_id placeholder="存储编号"/>
                <input node_ref=min_width placeholder="最小宽度"/>
                <input node_ref=min_height placeholder="最小高度"/>
                <button on:click=move |_| {
                    page.set(1);
                    load();
                }>"筛选"</button>
            </div>
            <div class="admin-toolbar">
                <button type="button" on:click=move |_| load()>"刷新图片"</button>
                <span class="muted">{move || page_summary(page.get(), page_size.get(), total.get(), loading.get())}</span>
            </div>
            <div class="admin-table-wrap">
                <table class="admin-table admin-image-table">
                    <thead>
                        <tr><th>"预览"</th><th>"图片"</th><th>"状态"</th><th>"来源"</th><th>"尺寸/大小"</th><th>"哈希/引用"</th><th>"操作"</th></tr>
                    </thead>
                    <tbody>
                        {move || images.get().into_iter().map(|image| {
                            let id = image.id.to_string();
                            let approve_id = id.clone();
                            let reject_id = id.clone();
                            let block_id = id.clone();
                            let detail_id = id.clone();
                            let tags = if image.tags.is_empty() { "无标签".to_string() } else { image.tags.join("，") };
                            view! {
                                <tr>
                                    <td data-label="预览">
                                        <div class="admin-thumb">
                                            <ImageThumb src=image.preview_url.clone() alt=image.original_name.clone()/>
                                        </div>
                                    </td>
                                    <td data-label="图片">
                                        <strong>{image.original_name.clone()}</strong>
                                        <small>{tags}</small>
                                    </td>
                                    <td data-label="状态"><StatusBadge value=image.status.clone()/></td>
                                    <td data-label="来源">{visibility_label(&image.visibility)}</td>
                                    <td data-label="尺寸/大小">{format!("{} x {} · {}", image.width, image.height, format_bytes(image.size))}</td>
                                    <td data-label="哈希/引用">{format!("{} · 引用 {}", short_text(&image.sha256, 12), image.ref_count)}</td>
                                    <td data-label="操作">
                                        <div class="row-actions">
                                            <button type="button" on:click=move |_| action(approve_id.clone(), "approve")>"通过"</button>
                                            <button type="button" on:click=move |_| action(reject_id.clone(), "reject")>"拒绝"</button>
                                            <button type="button" on:click=move |_| action(block_id.clone(), "block")>"隔离"</button>
                                            <button type="button" on:click=move |_| show_detail(detail_id.clone())>"详情"</button>
                                        </div>
                                    </td>
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            </div>
            <Show when=move || images.get().is_empty()>
                <div class="empty">"暂无符合条件的图片"</div>
            </Show>
            <AdminPager page=page page_size=page_size total=total loading=loading load=load/>
            {move || detail.get().map(|value| {
                let image = value.get("image").cloned().unwrap_or_else(|| json!({}));
                let storage_objects = value.get("storage_objects").and_then(Value::as_array).cloned().unwrap_or_default();
                view! {
                    <section class="admin-detail">
                        <header class="section-head">
                            <h3>"图片详情"</h3>
                            <button class="secondary" type="button" on:click=move |_| detail.set(None)>"收起"</button>
                        </header>
                        <div class="detail-fields">
                            <span><small>"编号"</small><strong>{json_string(&image, "id")}</strong></span>
                            <span><small>"文件名"</small><strong>{json_string(&image, "original_name")}</strong></span>
                            <span><small>"状态"</small><strong>{status_label(&json_string(&image, "status"))}</strong></span>
                            <span><small>"可见性"</small><strong>{visibility_label(&json_string(&image, "visibility"))}</strong></span>
                            <span><small>"尺寸"</small><strong>{format!("{} x {}", json_i64(&image, "width"), json_i64(&image, "height"))}</strong></span>
                            <span><small>"大小"</small><strong>{format_bytes(json_i64(&image, "size"))}</strong></span>
                            <span><small>"方向"</small><strong>{orientation_label(&json_string(&image, "orientation"))}</strong></span>
                            <span><small>"SHA256"</small><strong>{json_string(&image, "sha256")}</strong></span>
                        </div>
                        <h3>"存储对象"</h3>
                        <div class="admin-table-wrap">
                            <table class="admin-table">
                                <thead><tr><th>"存储"</th><th>"类型"</th><th>"对象"</th><th>"状态"</th><th>"大小"</th></tr></thead>
                                <tbody>
                                    {storage_objects.into_iter().map(|object| view! {
                                        <tr>
                                            <td data-label="存储">{json_string(&object, "provider_name")}</td>
                                            <td data-label="类型">{object_type_label(&json_string(&object, "object_type"))}</td>
                                            <td data-label="对象">{json_string(&object, "object_key")}</td>
                                            <td data-label="状态"><StatusBadge value=json_string(&object, "status")/></td>
                                            <td data-label="大小">{format_bytes(json_i64(&object, "size"))}</td>
                                        </tr>
                                    }).collect_view()}
                                </tbody>
                            </table>
                        </div>
                    </section>
                }
            }).into_any()}
        </div>
    }
}

#[component]
fn AdminQuota(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let data = RwSignal::new(json!({}));
    let group_name = NodeRef::<Input>::new();
    let group_code = NodeRef::<Input>::new();
    let group_description = NodeRef::<Input>::new();
    let rule_group_id = NodeRef::<Input>::new();
    let daily_upload_count = NodeRef::<Input>::new();
    let daily_upload_bytes = NodeRef::<Input>::new();
    let max_file_size = NodeRef::<Input>::new();
    let total_storage_bytes = NodeRef::<Input>::new();
    let daily_api_calls = NodeRef::<Input>::new();
    let daily_random_calls = NodeRef::<Input>::new();
    let default_storage_provider_id = NodeRef::<Select>::new();
    let require_review = NodeRef::<Input>::new();
    let allow_batch_upload = NodeRef::<Input>::new();
    let allow_tag_create = NodeRef::<Input>::new();
    let load = move || {
        let token_value = token.get();
        spawn_local(async move {
            let groups = api_get::<Vec<Value>>("/api/admin/user-groups", &token_value).await;
            let rules = api_get::<Vec<Value>>("/api/admin/quota-rules", &token_value).await;
            let usage = api_get::<Vec<Value>>("/api/admin/quota-usage", &token_value).await;
            let providers =
                api_get::<Vec<Value>>("/api/admin/storage/providers", &token_value).await;
            match (groups, rules, usage, providers) {
                (Ok(groups), Ok(rules), Ok(usage), Ok(providers)) => {
                    data.set(json!({"groups": groups, "rules": rules, "usage": usage, "providers": providers}))
                }
                _ => notify("用户组配额加载失败".to_string()),
            }
        });
    };
    Effect::new(move |_| load());
    let create_group = move |event: SubmitEvent| {
        event.prevent_default();
        let token_value = token.get();
        let body = json!({
            "name": input_value(group_name).unwrap_or_default(),
            "code": input_value(group_code).unwrap_or_default(),
            "description": input_value(group_description).unwrap_or_default(),
            "is_default": false,
        });
        spawn_local(async move {
            match api_json::<Value, _>("/api/admin/user-groups", &token_value, "POST", &body).await
            {
                Ok(_) => {
                    notify("用户组已创建".to_string());
                    load();
                }
                Err(err) => notify(err),
            }
        });
    };
    let save_rule = move |event: SubmitEvent| {
        event.prevent_default();
        let group_id = input_value(rule_group_id).unwrap_or_default();
        if group_id.is_empty() {
            notify("请输入用户组编号".to_string());
            return;
        }
        let token_value = token.get();
        let body = json!({
            "daily_upload_count": input_i32(daily_upload_count),
            "daily_upload_bytes": input_i64(daily_upload_bytes),
            "max_file_size": input_i64(max_file_size),
            "total_storage_bytes": input_i64(total_storage_bytes),
            "daily_api_calls": input_i32(daily_api_calls),
            "daily_random_calls": input_i32(daily_random_calls),
            "require_review": checked_value(require_review),
            "allow_batch_upload": checked_value(allow_batch_upload),
            "allow_tag_create": checked_value(allow_tag_create),
            "default_storage_provider_id": select_value(default_storage_provider_id).filter(|value| !value.is_empty()),
        });
        spawn_local(async move {
            match api_json::<Value, _>(
                &format!("/api/admin/quota-rules/{group_id}"),
                &token_value,
                "PUT",
                &body,
            )
            .await
            {
                Ok(_) => {
                    notify("配额规则已保存".to_string());
                    load();
                }
                Err(err) => notify(err),
            }
        });
    };
    let load_rule_to_form = move |rule: Value| {
        title_value(rule_group_id, &json_string(&rule, "group_id"));
        title_value(
            daily_upload_count,
            &json_i64(&rule, "daily_upload_count").to_string(),
        );
        title_value(
            daily_upload_bytes,
            &json_i64(&rule, "daily_upload_bytes").to_string(),
        );
        title_value(max_file_size, &json_i64(&rule, "max_file_size").to_string());
        title_value(
            total_storage_bytes,
            &json_i64(&rule, "total_storage_bytes").to_string(),
        );
        title_value(
            daily_api_calls,
            &json_i64(&rule, "daily_api_calls").to_string(),
        );
        title_value(
            daily_random_calls,
            &json_i64(&rule, "daily_random_calls").to_string(),
        );
        select_set_value(
            default_storage_provider_id,
            &json_string(&rule, "default_storage_provider_id"),
        );
        checked_set_value(
            require_review,
            rule.get("require_review")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        );
        checked_set_value(
            allow_batch_upload,
            rule.get("allow_batch_upload")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        );
        checked_set_value(
            allow_tag_create,
            rule.get("allow_tag_create")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        );
        notify("已载入配额规则".to_string());
    };
    view! {
        <div class="admin-stack">
            <div class="grid two">
                <section class="admin-section">
                    <h3>"创建用户组"</h3>
                    <form class="admin-form" on:submit=create_group>
                        <input node_ref=group_name placeholder="用户组名称"/>
                        <input node_ref=group_code placeholder="用户组代码"/>
                        <input node_ref=group_description placeholder="说明"/>
                        <button type="submit">"创建用户组"</button>
                    </form>
                </section>
                <section class="admin-section">
                    <h3>"保存配额规则"</h3>
                    <form class="admin-form compact-form" on:submit=save_rule>
                        <input node_ref=rule_group_id placeholder="用户组编号"/>
                        <input node_ref=daily_upload_count type="number" placeholder="每日上传数量"/>
                        <input node_ref=daily_upload_bytes type="number" placeholder="每日上传容量，字节"/>
                        <input node_ref=max_file_size type="number" placeholder="单文件大小，字节"/>
                        <input node_ref=total_storage_bytes type="number" placeholder="账号总容量，字节"/>
                        <input node_ref=daily_api_calls type="number" placeholder="每日接口调用"/>
                        <input node_ref=daily_random_calls type="number" placeholder="每日随机图调用"/>
                        <select node_ref=default_storage_provider_id>
                            <option value="">"沿用存储路由 / 全局默认"</option>
                            {move || json_array(&data.get(), "providers").into_iter().map(|provider| {
                                let id = json_string(&provider, "id");
                                view! {
                                    <option value=id.clone()>
                                        {format!("{} · {}", json_string(&provider, "name"), storage_type_label(&json_string(&provider, "provider_type")))}
                                    </option>
                                }
                            }).collect_view()}
                        </select>
                        <label class="checkbox-row"><input node_ref=require_review type="checkbox"/>"需要审核"</label>
                        <label class="checkbox-row"><input node_ref=allow_batch_upload type="checkbox" checked/>"允许批量上传"</label>
                        <label class="checkbox-row"><input node_ref=allow_tag_create type="checkbox" checked/>"允许创建标签"</label>
                        <button type="submit">"保存配额规则"</button>
                    </form>
                </section>
            </div>
            <div class="admin-toolbar">
                <button type="button" on:click=move |_| load()>"刷新配额"</button>
            </div>
            <div class="admin-table-wrap">
                <table class="admin-table">
                    <thead>
                        <tr><th>"用户组"</th><th>"上传数量"</th><th>"上传容量"</th><th>"单文件"</th><th>"总容量"</th><th>"默认存储"</th><th>"策略"</th><th>"操作"</th></tr>
                    </thead>
                    <tbody>
                        {move || json_array(&data.get(), "rules").into_iter().map(|rule| {
                            let form_rule = rule.clone();
                            view! {
                                <tr>
                                    <td data-label="用户组"><strong>{role_label(&json_string(&rule, "code"))}</strong><small>{json_string(&rule, "group_id")}</small></td>
                                    <td data-label="上传数量">{json_i64(&rule, "daily_upload_count")}</td>
                                    <td data-label="上传容量">{format_bytes(json_i64(&rule, "daily_upload_bytes"))}</td>
                                    <td data-label="单文件">{format_bytes(json_i64(&rule, "max_file_size"))}</td>
                                    <td data-label="总容量">{format_bytes(json_i64(&rule, "total_storage_bytes"))}</td>
                                    <td data-label="默认存储">{provider_name_from_id(&json_array(&data.get(), "providers"), &json_string(&rule, "default_storage_provider_id"))}</td>
                                    <td data-label="策略">{format!(
                                        "{} / {} · {} · {} · {}",
                                        json_i64(&rule, "daily_api_calls"),
                                        json_i64(&rule, "daily_random_calls"),
                                        bool_label(rule.get("require_review").and_then(Value::as_bool).unwrap_or(false), "需要审核", "免审核"),
                                        bool_label(rule.get("allow_batch_upload").and_then(Value::as_bool).unwrap_or(false), "允许批量", "禁止批量"),
                                        bool_label(rule.get("allow_tag_create").and_then(Value::as_bool).unwrap_or(false), "允许标签", "禁止标签"),
                                    )}</td>
                                    <td data-label="操作"><button type="button" on:click=move |_| load_rule_to_form(form_rule.clone())>"载入"</button></td>
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            </div>
            <section class="admin-section">
                <h3>"今日用量"</h3>
                <div class="admin-table-wrap">
                    <table class="admin-table">
                        <thead><tr><th>"用户"</th><th>"日期"</th><th>"上传数量"</th><th>"上传容量"</th><th>"接口"</th><th>"随机图"</th></tr></thead>
                        <tbody>
                            {move || json_array(&data.get(), "usage").into_iter().take(120).map(|item| view! {
                                <tr>
                                    <td data-label="用户">{json_string(&item, "user_id")}</td>
                                    <td data-label="日期">{json_string(&item, "date")}</td>
                                    <td data-label="上传数量">{json_i64(&item, "uploaded_count")}</td>
                                    <td data-label="上传容量">{format_bytes(json_i64(&item, "uploaded_bytes"))}</td>
                                    <td data-label="接口">{json_i64(&item, "api_calls")}</td>
                                    <td data-label="随机图">{json_i64(&item, "random_calls")}</td>
                                </tr>
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
            </section>
        </div>
    }
}

#[component]
fn AdminTags(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let tags = RwSignal::new(Vec::<TagView>::new());
    let load = move || {
        let token_value = token.get();
        spawn_local(async move {
            match api_get::<Vec<TagView>>("/api/tags", &token_value).await {
                Ok(value) => tags.set(value),
                Err(err) => notify(err),
            }
        });
    };
    Effect::new(move |_| load());
    let disable = move |id: String| {
        if !confirm("确认禁用这个标签？") {
            return;
        }
        let token_value = token.get();
        spawn_local(async move {
            match api_empty(&format!("/api/tags/{id}"), &token_value, "DELETE").await {
                Ok(_) => load(),
                Err(err) => notify(err),
            }
        });
    };
    view! {
        <div class="admin-stack">
            <div class="admin-toolbar">
                <button type="button" on:click=move |_| load()>"刷新标签"</button>
            </div>
            <div class="admin-table-wrap">
                <table class="admin-table">
                    <thead><tr><th>"标签"</th><th>"状态"</th><th>"使用次数"</th><th>"操作"</th></tr></thead>
                    <tbody>
                        {move || tags.get().into_iter().map(|tag| {
                            let id = tag.id.to_string();
                            view! {
                                <tr>
                                    <td data-label="标签"><strong>{tag.name}</strong></td>
                                    <td data-label="状态"><StatusBadge value=tag.status.clone()/></td>
                                    <td data-label="使用次数">{tag.usage_count}</td>
                                    <td data-label="操作"><button type="button" on:click=move |_| disable(id.clone())>"禁用"</button></td>
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            </div>
            <Show when=move || tags.get().is_empty()>
                <div class="empty">"暂无标签"</div>
            </Show>
        </div>
    }
}

#[component]
fn AdminAudit(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let tasks = RwSignal::new(Vec::<Value>::new());
    let logs = RwSignal::new(Vec::<Value>::new());
    let task_page = RwSignal::new(1_i64);
    let task_page_size = RwSignal::new(40_i64);
    let task_total = RwSignal::new(0_i64);
    let task_loading = RwSignal::new(false);
    let log_page = RwSignal::new(1_i64);
    let log_page_size = RwSignal::new(40_i64);
    let log_total = RwSignal::new(0_i64);
    let log_loading = RwSignal::new(false);
    let settings = RwSignal::new(json!({}));
    let detail = RwSignal::new(None::<Value>);
    let task_status = NodeRef::<Select>::new();
    let task_q = NodeRef::<Input>::new();
    let log_status = NodeRef::<Select>::new();
    let log_q = NodeRef::<Input>::new();
    let ai_enabled = NodeRef::<Input>::new();
    let service_url = NodeRef::<Input>::new();
    let api_token = NodeRef::<Input>::new();
    let failure_strategy = NodeRef::<Select>::new();
    let keyword_enabled = NodeRef::<Input>::new();
    let ocr_enabled = NodeRef::<Input>::new();
    let description_enabled = NodeRef::<Input>::new();
    let keywords = NodeRef::<Input>::new();
    let load = move || {
        let token_value = token.get();
        let tasks_path = admin_page_path(
            "/api/admin/audit/tasks/page",
            task_page.get(),
            task_page_size.get(),
            &[
                ("status", select_value(task_status).unwrap_or_default()),
                ("q", input_value(task_q).unwrap_or_default()),
            ],
        );
        let logs_path = admin_page_path(
            "/api/admin/audit/logs/page",
            log_page.get(),
            log_page_size.get(),
            &[
                ("status", select_value(log_status).unwrap_or_default()),
                ("q", input_value(log_q).unwrap_or_default()),
            ],
        );
        task_loading.set(true);
        log_loading.set(true);
        spawn_local(async move {
            match api_get::<Page<Value>>(&tasks_path, &token_value).await {
                Ok(value) => {
                    task_total.set(value.total);
                    tasks.set(value.items);
                }
                Err(err) => notify(err),
            }
            task_loading.set(false);
            match api_get::<Page<Value>>(&logs_path, &token_value).await {
                Ok(value) => {
                    log_total.set(value.total);
                    logs.set(value.items);
                }
                Err(err) => notify(err),
            }
            log_loading.set(false);
            if let Ok(value) = api_get::<Value>("/api/admin/audit/settings", &token_value).await {
                settings.set(value.clone());
                fill_audit_form(
                    &value,
                    ai_enabled,
                    service_url,
                    api_token,
                    failure_strategy,
                    keyword_enabled,
                    ocr_enabled,
                    description_enabled,
                    keywords,
                );
            }
        });
    };
    Effect::new(move |_| load());
    let save = move |event: SubmitEvent| {
        event.prevent_default();
        let token_value = token.get();
        let body = audit_settings_body(
            &settings.get(),
            AuditSettingsValues {
                ai_enabled: checked_value(ai_enabled),
                service_url: input_value(service_url).unwrap_or_default(),
                api_token: input_value(api_token).unwrap_or_default(),
                failure_strategy: select_value(failure_strategy)
                    .unwrap_or_else(|| "manual_required".to_string()),
                keyword_enabled: checked_value(keyword_enabled),
                ocr_enabled: checked_value(ocr_enabled),
                description_enabled: checked_value(description_enabled),
                keywords: input_value(keywords).unwrap_or_default(),
            },
        );
        spawn_local(async move {
            match api_json::<Value, _>("/api/admin/audit/settings", &token_value, "PUT", &body)
                .await
            {
                Ok(_) => {
                    notify("审核设置已保存".to_string());
                    load();
                }
                Err(err) => notify(err),
            }
        });
    };
    let task_action = move |id: String, action: &'static str| {
        if action == "reject" && !confirm("确认拒绝这个审核任务？") {
            return;
        }
        let token_value = token.get();
        spawn_local(async move {
            match api_empty(
                &format!("/api/admin/audit/tasks/{id}/{action}"),
                &token_value,
                "POST",
            )
            .await
            {
                Ok(_) => load(),
                Err(err) => notify(err),
            }
        });
    };
    let show_detail = move |id: String| {
        let token_value = token.get();
        spawn_local(async move {
            match api_get::<Value>(&format!("/api/admin/audit/tasks/{id}"), &token_value).await {
                Ok(value) => detail.set(Some(value)),
                Err(err) => notify(err),
            }
        });
    };
    view! {
        <div class="admin-stack">
            <section class="admin-section">
                <h3>"审核设置"</h3>
                <form class="form admin-settings-form" on:submit=save>
                    <label class="checkbox-row"><input node_ref=ai_enabled type="checkbox"/>"启用智能审核"</label>
                    <label>"智能审核服务地址"<input node_ref=service_url placeholder="http://127.0.0.1:8080"/></label>
                    <label>"访问令牌"<input node_ref=api_token type="password" placeholder="留空或 ******** 表示保留"/></label>
                    <label>"失败策略"<select node_ref=failure_strategy>
                        <option value="manual_required">"转人工处理"</option>
                        <option value="reject">"直接拒绝"</option>
                        <option value="pass">"临时放行"</option>
                    </select></label>
                    <label class="checkbox-row"><input node_ref=keyword_enabled type="checkbox"/>"启用关键词审核"</label>
                    <label class="checkbox-row"><input node_ref=ocr_enabled type="checkbox"/>"启用 OCR"</label>
                    <label class="checkbox-row"><input node_ref=description_enabled type="checkbox"/>"生成描述与辅助信息"</label>
                    <label>"拦截关键词"<input node_ref=keywords placeholder="逗号分隔"/></label>
                    <button type="submit">"保存审核设置"</button>
                </form>
            </section>
            <section class="admin-section">
                <div class="section-head">
                    <h3>"审核任务"</h3>
                    <button type="button" on:click=move |_| load()>"刷新"</button>
                </div>
                <div class="filters">
                    <select node_ref=task_status>
                        <option value="">"全部状态"</option>
                        <option value="pending">"等待中"</option>
                        <option value="running">"运行中"</option>
                        <option value="manual_required">"待人工"</option>
                        <option value="passed">"已通过"</option>
                        <option value="rejected">"已拒绝"</option>
                        <option value="failed">"失败"</option>
                    </select>
                    <input node_ref=task_q placeholder="搜索任务 / 图片 / 服务"/>
                    <button type="button" on:click=move |_| {
                        task_page.set(1);
                        load();
                    }>"筛选任务"</button>
                    <span class="muted">{move || page_summary(task_page.get(), task_page_size.get(), task_total.get(), task_loading.get())}</span>
                </div>
                <div class="admin-table-wrap">
                    <table class="admin-table">
                        <thead><tr><th>"类型"</th><th>"状态"</th><th>"服务"</th><th>"重试"</th><th>"创建时间"</th><th>"操作"</th></tr></thead>
                        <tbody>
                            {move || tasks.get().into_iter().map(|task| {
                                let id = json_string(&task, "id");
                                let detail_id = id.clone();
                                let approve_id = id.clone();
                                let reject_id = id.clone();
                                let retry_id = id.clone();
                                view! {
                                    <tr>
                                        <td data-label="类型"><strong>{audit_type_label(&json_string(&task, "audit_type"))}</strong><small>{json_string(&task, "image_id")}</small></td>
                                        <td data-label="状态"><StatusBadge value=json_string(&task, "status")/></td>
                                        <td data-label="服务">{provider_label(&json_string(&task, "provider"))}</td>
                                        <td data-label="重试">{json_i64(&task, "retry_count")}</td>
                                        <td data-label="创建时间">{time_label(&json_string(&task, "created_at"))}</td>
                                        <td data-label="操作">
                                            <div class="row-actions">
                                                <button type="button" on:click=move |_| show_detail(detail_id.clone())>"详情"</button>
                                                <button type="button" on:click=move |_| task_action(approve_id.clone(), "approve")>"通过"</button>
                                                <button type="button" on:click=move |_| task_action(reject_id.clone(), "reject")>"拒绝"</button>
                                                <button type="button" on:click=move |_| task_action(retry_id.clone(), "retry")>"重试"</button>
                                            </div>
                                        </td>
                                    </tr>
                                }
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
                <AdminPager page=task_page page_size=task_page_size total=task_total loading=task_loading load=load/>
            </section>
            {move || detail.get().map(|value| {
                let task = value.get("task").cloned().unwrap_or_else(|| json!({}));
                let results = value.get("results").and_then(Value::as_array).cloned().unwrap_or_default();
                view! {
                    <section class="admin-detail">
                        <header class="section-head">
                            <h3>"审核详情"</h3>
                            <button class="secondary" type="button" on:click=move |_| detail.set(None)>"收起"</button>
                        </header>
                        <div class="detail-fields">
                            <span><small>"任务编号"</small><strong>{json_string(&task, "id")}</strong></span>
                            <span><small>"图片编号"</small><strong>{json_string(&task, "image_id")}</strong></span>
                            <span><small>"类型"</small><strong>{audit_type_label(&json_string(&task, "audit_type"))}</strong></span>
                            <span><small>"状态"</small><strong>{status_label(&json_string(&task, "status"))}</strong></span>
                            <span><small>"错误"</small><strong>{display_or_dash(&json_string(&task, "error_message"))}</strong></span>
                        </div>
                        <div class="admin-table-wrap">
                            <table class="admin-table">
                                <thead><tr><th>"结果"</th><th>"风险"</th><th>"服务"</th><th>"原因"</th><th>"耗时"</th></tr></thead>
                                <tbody>
                                    {results.into_iter().map(|result| view! {
                                        <tr>
                                            <td data-label="结果"><StatusBadge value=json_string(&result, "result")/></td>
                                            <td data-label="风险">{risk_label(&json_string(&result, "risk_level"))}</td>
                                            <td data-label="服务">{provider_label(&json_string(&result, "provider"))}</td>
                                            <td data-label="原因">{display_or_dash(&json_string(&result, "reason"))}</td>
                                            <td data-label="耗时">{format!("{} ms", json_i64(&result, "duration_ms"))}</td>
                                        </tr>
                                    }).collect_view()}
                                </tbody>
                            </table>
                        </div>
                    </section>
                }
            }).into_any()}
            <section class="audit-results admin-section">
                <h3>"审核日志"</h3>
                <div class="filters">
                    <select node_ref=log_status>
                        <option value="">"全部结果"</option>
                        <option value="passed">"已通过"</option>
                        <option value="rejected">"已拒绝"</option>
                        <option value="low">"低风险"</option>
                        <option value="medium">"中风险"</option>
                        <option value="high">"高风险"</option>
                    </select>
                    <input node_ref=log_q placeholder="搜索图片 / 服务 / 原因"/>
                    <button type="button" on:click=move |_| {
                        log_page.set(1);
                        load();
                    }>"筛选日志"</button>
                    <span class="muted">{move || page_summary(log_page.get(), log_page_size.get(), log_total.get(), log_loading.get())}</span>
                </div>
                <div class="admin-table-wrap">
                    <table class="admin-table">
                        <thead><tr><th>"结果"</th><th>"风险"</th><th>"服务"</th><th>"原因"</th><th>"时间"</th></tr></thead>
                        <tbody>
                            {move || logs.get().into_iter().map(|item| view! {
                                <tr>
                                    <td data-label="结果"><StatusBadge value=json_string(&item, "result")/></td>
                                    <td data-label="风险">{risk_label(&json_string(&item, "risk_level"))}</td>
                                    <td data-label="服务">{provider_label(&json_string(&item, "provider"))}</td>
                                    <td data-label="原因">{display_or_dash(&json_string(&item, "reason"))}</td>
                                    <td data-label="时间">{time_label(&json_string(&item, "created_at"))}</td>
                                </tr>
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
                <AdminPager page=log_page page_size=log_page_size total=log_total loading=log_loading load=load/>
            </section>
        </div>
    }
}

#[component]
fn AdminStorage(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let providers = RwSignal::new(Vec::<Value>::new());
    let routes = RwSignal::new(Vec::<Value>::new());
    let route_summary = RwSignal::new(json!({}));
    let health = RwSignal::new(Vec::<Value>::new());
    let selected = RwSignal::new(Vec::<String>::new());
    let page = RwSignal::new(1_i64);
    let page_size = RwSignal::new(20_i64);
    let total = RwSignal::new(0_i64);
    let route_page = RwSignal::new(1_i64);
    let route_page_size = RwSignal::new(20_i64);
    let route_total = RwSignal::new(0_i64);
    let route_loading = RwSignal::new(false);
    let loading = RwSignal::new(false);
    let health_loading = RwSignal::new(false);
    let deleting = RwSignal::new(false);
    let saving_provider = RwSignal::new(false);
    let provider_action = RwSignal::new(None::<String>);
    let editing_provider = RwSignal::new(None::<String>);
    let editing_route = RwSignal::new(None::<String>);
    let provider_kind = RwSignal::new("local".to_string());
    let filter_q = NodeRef::<Input>::new();
    let filter_type = NodeRef::<Select>::new();
    let filter_status = NodeRef::<Select>::new();
    let route_filter_q = NodeRef::<Input>::new();
    let route_filter_scope = NodeRef::<Select>::new();
    let route_filter_status = NodeRef::<Select>::new();
    let name = NodeRef::<Input>::new();
    let provider_type = NodeRef::<Select>::new();
    let enabled = NodeRef::<Input>::new();
    let priority = NodeRef::<Input>::new();
    let route_name = NodeRef::<Input>::new();
    let route_scope_type = NodeRef::<Select>::new();
    let route_scope_value = NodeRef::<Input>::new();
    let route_provider_id = NodeRef::<Select>::new();
    let route_enabled = NodeRef::<Input>::new();
    let route_priority = NodeRef::<Input>::new();
    let route_note = NodeRef::<Input>::new();
    let local_root = NodeRef::<Input>::new();
    let local_public_prefix = NodeRef::<Input>::new();
    let endpoint = NodeRef::<Input>::new();
    let region = NodeRef::<Input>::new();
    let bucket = NodeRef::<Input>::new();
    let r2_account_id = NodeRef::<Input>::new();
    let r2_jurisdiction = NodeRef::<Select>::new();
    let access_mode = NodeRef::<Select>::new();
    let presigned_url_ttl_seconds = NodeRef::<Input>::new();
    let access_key_id = NodeRef::<Input>::new();
    let secret_access_key = NodeRef::<Input>::new();
    let session_token = NodeRef::<Input>::new();
    let public_domain = NodeRef::<Input>::new();
    let path_prefix = NodeRef::<Input>::new();
    let client_id = NodeRef::<Input>::new();
    let tenant_id = NodeRef::<Input>::new();
    let client_secret = NodeRef::<Input>::new();
    let refresh_token = NodeRef::<Input>::new();
    let drive_email = NodeRef::<Input>::new();
    let root_dir = NodeRef::<Input>::new();
    let namespace = NodeRef::<Input>::new();
    let tenancy_ocid = NodeRef::<Input>::new();
    let user_ocid = NodeRef::<Input>::new();
    let fingerprint = NodeRef::<Input>::new();
    let private_key = NodeRef::<Textarea>::new();
    let load_providers = move || {
        let Some(page_value) = page.try_get_untracked() else {
            return;
        };
        let Some(page_size_value) = page_size.try_get_untracked() else {
            return;
        };
        let token_value = token.get_untracked();
        let path = admin_page_path(
            "/api/admin/storage/providers/page",
            page_value,
            page_size_value,
            &[
                ("q", input_value(filter_q).unwrap_or_default()),
                ("role", select_value(filter_type).unwrap_or_default()),
                ("status", select_value(filter_status).unwrap_or_default()),
            ],
        );
        if loading.try_set(true).is_some() {
            return;
        }
        spawn_local(async move {
            match api_get::<Page<Value>>(&path, &token_value).await {
                Ok(value) => {
                    let _ = total.try_set(value.total);
                    let _ = providers.try_set(value.items);
                    let _ = selected.try_set(Vec::new());
                }
                Err(err) => notify(err),
            }
            let _ = loading.try_set(false);
        });
    };
    let load_routes = move || {
        let Some(route_page_value) = route_page.try_get_untracked() else {
            return;
        };
        let Some(route_page_size_value) = route_page_size.try_get_untracked() else {
            return;
        };
        let token_value = token.get_untracked();
        let path = admin_page_path(
            "/api/admin/storage/routes/page",
            route_page_value,
            route_page_size_value,
            &[
                ("q", input_value(route_filter_q).unwrap_or_default()),
                ("role", select_value(route_filter_scope).unwrap_or_default()),
                (
                    "status",
                    select_value(route_filter_status).unwrap_or_default(),
                ),
            ],
        );
        if route_loading.try_set(true).is_some() {
            return;
        }
        spawn_local(async move {
            match api_get::<Page<Value>>(&path, &token_value).await {
                Ok(value) => {
                    let _ = route_total.try_set(value.total);
                    let _ = routes.try_set(value.items);
                }
                Err(err) => notify(err),
            }
            if let Ok(value) =
                api_get::<Value>("/api/admin/storage/routes/summary", &token_value).await
            {
                let _ = route_summary.try_set(value);
            }
            let _ = route_loading.try_set(false);
        });
    };
    let check_health = move || {
        let token_value = token.get_untracked();
        let ids = providers
            .try_get_untracked()
            .unwrap_or_default()
            .into_iter()
            .map(|item| json_string(&item, "id"))
            .filter(|id| !id.is_empty())
            .collect::<Vec<_>>()
            .join(",");
        if ids.is_empty() {
            notify("当前页没有可检查的存储".to_string());
            return;
        }
        if health_loading.try_set(true).is_some() {
            return;
        }
        spawn_local(async move {
            match api_get::<Vec<Value>>(
                &format!(
                    "/api/admin/storage/health/page?ids={}",
                    urlencoding::encode(&ids)
                ),
                &token_value,
            )
            .await
            {
                Ok(value) => {
                    let _ = health.try_set(value);
                    notify("当前页健康检查已完成".to_string());
                }
                Err(err) => notify(err),
            }
            let _ = health_loading.try_set(false);
        });
    };
    let upsert_provider = move |provider: Value| {
        let id = json_string(&provider, "id");
        if id.is_empty() {
            return;
        }
        let _ = providers.try_update(|items| {
            if let Some(existing) = items.iter_mut().find(|item| json_string(item, "id") == id) {
                *existing = provider.clone();
            } else {
                items.insert(0, provider.clone());
                let _ = total.try_update(|value| *value += 1);
            }
            items.sort_by(|left, right| {
                json_i64_or(left, "priority", 100)
                    .cmp(&json_i64_or(right, "priority", 100))
                    .then_with(|| json_string(right, "id").cmp(&json_string(left, "id")))
            });
        });
    };
    let sync_routes_for_provider = move |provider: &Value| {
        let id = json_string(provider, "id");
        let name = json_string(provider, "name");
        let provider_type_value = json_string(provider, "provider_type");
        if id.is_empty() {
            return;
        }
        let _ = routes.try_update(|items| {
            for route in items {
                if json_string(route, "storage_provider_id") == id {
                    ensure_object(route);
                    object_insert(route, "storage_provider_name", json!(name.clone()));
                    object_insert(
                        route,
                        "storage_provider_type",
                        json!(provider_type_value.clone()),
                    );
                }
            }
        });
    };
    Effect::new(move |_| {
        load_providers();
        load_routes();
    });
    let delete_provider = move |id: String| {
        if deleting.try_get_untracked().unwrap_or(false) {
            return;
        }
        if !confirm(
            "确认删除这个存储？未被引用会直接删除；已被图片、迁移或备份引用时会停用并从列表隐藏。",
        ) {
            return;
        }
        if deleting.try_set(true).is_some() {
            return;
        }
        let token_value = token.get_untracked();
        spawn_local(async move {
            match api_empty(
                &format!("/api/admin/storage/providers/{id}"),
                &token_value,
                "DELETE",
            )
            .await
            {
                Ok(value) => {
                    let mode = json_string(&value, "mode");
                    notify(storage_delete_mode_message(&mode));
                    load_providers();
                    load_routes();
                }
                Err(err) => notify(err),
            }
            let _ = deleting.try_set(false);
        });
    };
    let save_provider = move |event: SubmitEvent| {
        event.prevent_default();
        if saving_provider.try_get_untracked().unwrap_or(false) {
            return;
        }
        let token_value = token.get_untracked();
        let provider = select_value(provider_type).unwrap_or_else(|| "local".to_string());
        let parsed = storage_config_from_form(
            &provider,
            local_root,
            local_public_prefix,
            endpoint,
            region,
            bucket,
            r2_account_id,
            r2_jurisdiction,
            access_mode,
            presigned_url_ttl_seconds,
            access_key_id,
            secret_access_key,
            session_token,
            public_domain,
            path_prefix,
            client_id,
            tenant_id,
            client_secret,
            refresh_token,
            drive_email,
            root_dir,
            namespace,
            tenancy_ocid,
            user_ocid,
            fingerprint,
            private_key,
        );
        let body = json!({
            "name": input_value(name).unwrap_or_else(|| "存储".to_string()),
            "provider_type": provider,
            "config_json": parsed,
            "enabled": checked_value(enabled),
            "priority": input_i64(priority).unwrap_or(100),
        });
        let editing_provider_id = editing_provider.try_get_untracked().flatten();
        if saving_provider.try_set(true).is_some() {
            return;
        }
        spawn_local(async move {
            let result = if let Some(id) = editing_provider_id {
                api_json::<Value, _>(
                    &format!("/api/admin/storage/providers/{id}"),
                    &token_value,
                    "PUT",
                    &body,
                )
                .await
            } else {
                api_json::<Value, _>("/api/admin/storage/providers", &token_value, "POST", &body)
                    .await
            };
            match result {
                Ok(value) => {
                    upsert_provider(value.clone());
                    sync_routes_for_provider(&value);
                    let _ = editing_provider.try_set(None);
                    notify("存储配置已保存".to_string());
                }
                Err(err) => notify(err),
            }
            let _ = saving_provider.try_set(false);
        });
    };
    let reset_form = move || {
        let _ = editing_provider.try_set(None);
        title_value(name, "");
        select_set_value(provider_type, "local");
        let _ = provider_kind.try_set("local".to_string());
        checked_set_value(enabled, true);
        title_value(priority, "100");
        clear_storage_form(
            local_root,
            local_public_prefix,
            endpoint,
            region,
            bucket,
            r2_account_id,
            r2_jurisdiction,
            access_mode,
            presigned_url_ttl_seconds,
            access_key_id,
            secret_access_key,
            session_token,
            public_domain,
            path_prefix,
            client_id,
            tenant_id,
            client_secret,
            refresh_token,
            drive_email,
            root_dir,
            namespace,
            tenancy_ocid,
            user_ocid,
            fingerprint,
            private_key,
        );
    };
    let reset_route_form = move || {
        let _ = editing_route.try_set(None);
        title_value(route_name, "");
        select_set_value(route_scope_type, "global");
        title_value(route_scope_value, "");
        select_set_value(route_provider_id, "");
        checked_set_value(route_enabled, true);
        title_value(route_priority, "100");
        title_value(route_note, "");
    };
    let edit_provider = move |id: String| {
        let token_value = token.get_untracked();
        spawn_local(async move {
            match api_get::<Value>(&format!("/api/admin/storage/providers/{id}"), &token_value)
                .await
            {
                Ok(value) => {
                    if editing_provider.try_set(Some(id)).is_some() {
                        return;
                    }
                    fill_storage_provider_form(
                        &value,
                        name,
                        provider_type,
                        enabled,
                        priority,
                        local_root,
                        local_public_prefix,
                        endpoint,
                        region,
                        bucket,
                        r2_account_id,
                        r2_jurisdiction,
                        access_mode,
                        presigned_url_ttl_seconds,
                        access_key_id,
                        secret_access_key,
                        session_token,
                        public_domain,
                        path_prefix,
                        client_id,
                        tenant_id,
                        client_secret,
                        refresh_token,
                        drive_email,
                        root_dir,
                        namespace,
                        tenancy_ocid,
                        user_ocid,
                        fingerprint,
                        private_key,
                    );
                    let _ = provider_kind.try_set(json_string(&value, "provider_type"));
                    notify("已载入存储配置".to_string());
                }
                Err(err) => notify(err),
            }
        });
    };
    let save_route = move |event: SubmitEvent| {
        event.prevent_default();
        let token_value = token.get_untracked();
        let provider_id = select_value(route_provider_id).unwrap_or_default();
        if provider_id.is_empty() {
            notify("请选择路由目标存储".to_string());
            return;
        }
        let scope_type = select_value(route_scope_type).unwrap_or_else(|| "global".to_string());
        let body = json!({
            "name": input_value(route_name).unwrap_or_default(),
            "scope_type": scope_type,
            "scope_value": input_value(route_scope_value).unwrap_or_default(),
            "storage_provider_id": provider_id,
            "enabled": checked_value(route_enabled),
            "priority": input_i32(route_priority).unwrap_or(100),
            "note": input_value(route_note).unwrap_or_default(),
        });
        let editing_route_id = editing_route.try_get_untracked().flatten();
        spawn_local(async move {
            let result = if let Some(id) = editing_route_id {
                api_json::<Value, _>(
                    &format!("/api/admin/storage/routes/{id}"),
                    &token_value,
                    "PUT",
                    &body,
                )
                .await
            } else {
                api_json::<Value, _>("/api/admin/storage/routes", &token_value, "POST", &body).await
            };
            match result {
                Ok(_) => {
                    let _ = editing_route.try_set(None);
                    notify("存储路由已保存".to_string());
                    load_routes();
                }
                Err(err) => notify(err),
            }
        });
    };
    let edit_route = move |id: String| {
        let token_value = token.get_untracked();
        spawn_local(async move {
            match api_get::<Value>(&format!("/api/admin/storage/routes/{id}"), &token_value).await {
                Ok(value) => {
                    if editing_route.try_set(Some(id)).is_some() {
                        return;
                    }
                    title_value(route_name, &json_string(&value, "name"));
                    select_set_value(
                        route_scope_type,
                        &json_string_or(&value, "scope_type", "global"),
                    );
                    title_value(route_scope_value, &json_string(&value, "scope_value"));
                    select_set_value(
                        route_provider_id,
                        &json_string(&value, "storage_provider_id"),
                    );
                    checked_set_value(route_enabled, json_bool(&value, "enabled", true));
                    title_value(
                        route_priority,
                        &json_i64_or(&value, "priority", 100).to_string(),
                    );
                    title_value(route_note, &json_string(&value, "note"));
                    notify("已载入存储路由".to_string());
                }
                Err(err) => notify(err),
            }
        });
    };
    let route_action = move |id: String, action: &'static str| {
        if action == "delete" && !confirm("确认删除这条存储路由？") {
            return;
        }
        let token_value = token.get_untracked();
        let (path, method) = if action == "delete" {
            (format!("/api/admin/storage/routes/{id}"), "DELETE")
        } else {
            (format!("/api/admin/storage/routes/{id}/{action}"), "POST")
        };
        spawn_local(async move {
            match api_empty(&path, &token_value, method).await {
                Ok(_) => {
                    notify("存储路由操作完成".to_string());
                    load_routes();
                }
                Err(err) => notify(err),
            }
        });
    };
    let storage_action = move |id: String, action: &'static str| {
        let action_key = format!("{id}:{action}");
        if provider_action.try_get_untracked().flatten().is_some() {
            return;
        }
        if provider_action.try_set(Some(action_key.clone())).is_some() {
            return;
        }
        let token_value = token.get_untracked();
        spawn_local(async move {
            match api_empty(
                &format!("/api/admin/storage/providers/{id}/{action}"),
                &token_value,
                "POST",
            )
            .await
            {
                Ok(value) => {
                    notify(storage_action_message(action, &value));
                    if matches!(action, "test-connection" | "test-upload" | "test-delete") {
                        let is_healthy = value
                            .get("healthy")
                            .and_then(Value::as_bool)
                            .unwrap_or_else(|| {
                                value.get("ok").and_then(Value::as_bool).unwrap_or(true)
                            });
                        let error = if is_healthy {
                            String::new()
                        } else {
                            json_string(&value, "error")
                        };
                        let _ = health.try_update(|items| {
                            if let Some(item) = items.iter_mut().find(|item| json_string(item, "id") == id)
                            {
                                ensure_object(item);
                                object_insert(item, "healthy", json!(is_healthy));
                                object_insert(
                                    item,
                                    "error",
                                    if error.is_empty() {
                                        Value::Null
                                    } else {
                                        json!(error.clone())
                                    },
                                );
                            } else {
                                items.push(json!({
                                    "id": id,
                                    "healthy": is_healthy,
                                    "error": if error.is_empty() { Value::Null } else { json!(error) }
                                }));
                            }
                        });
                    }
                    if action == "set-default" {
                        upsert_provider(value.clone());
                        let _ = providers.try_update(|items| {
                            for item in items {
                                ensure_object(item);
                                object_insert(
                                    item,
                                    "is_default",
                                    json!(json_string(item, "id") == id),
                                );
                                if json_string(item, "id") == id {
                                    object_insert(item, "enabled", json!(true));
                                }
                            }
                        });
                        sync_routes_for_provider(&value);
                    }
                }
                Err(err) => notify(err),
            }
            let _ = provider_action.try_set(None);
        });
    };
    let toggle_selected = move |id: String, checked: bool| {
        let _ = selected.try_update(|items| {
            if checked {
                if !items.iter().any(|item| item == &id) {
                    items.push(id);
                }
            } else {
                items.retain(|item| item != &id);
            }
        });
    };
    let bulk_delete = move || {
        let ids = selected.try_get_untracked().unwrap_or_default();
        if ids.is_empty() {
            notify("请选择要删除的存储".to_string());
            return;
        }
        if deleting.try_get_untracked().unwrap_or(false) {
            return;
        }
        if !confirm(
            "确认删除选中的存储？未被引用会直接删除；已被图片、迁移或备份引用时会停用并从列表隐藏。",
        ) {
            return;
        }
        if deleting.try_set(true).is_some() {
            return;
        }
        let token_value = token.get_untracked();
        spawn_local(async move {
            let body = json!({ "ids": ids });
            match api_json::<Value, _>(
                "/api/admin/storage/providers/bulk-delete",
                &token_value,
                "POST",
                &body,
            )
            .await
            {
                Ok(value) => {
                    let deleted = value
                        .get("deleted")
                        .and_then(Value::as_array)
                        .map(|items| items.len())
                        .unwrap_or(0);
                    let disabled = value
                        .get("disabled")
                        .and_then(Value::as_array)
                        .map(|items| items.len())
                        .unwrap_or(0);
                    let failed = value
                        .get("failed")
                        .and_then(Value::as_array)
                        .map(|items| items.len())
                        .unwrap_or(0);
                    notify(format!(
                        "批量删除完成：真删除 {deleted} 个，停用隐藏 {disabled} 个，失败 {failed} 个"
                    ));
                    load_providers();
                    load_routes();
                }
                Err(err) => notify(err),
            }
            let _ = deleting.try_set(false);
        });
    };
    view! {
        <div class="admin-stack">
            <section class="admin-section">
                <h3>{move || if signal_option_is_some(editing_provider) { "编辑存储" } else { "添加存储" }}</h3>
                <form class="admin-form admin-settings-form" on:submit=save_provider>
                    <input node_ref=name placeholder="存储名称"/>
                    <select node_ref=provider_type on:change=move |event| {
                        let _ = provider_kind.try_set(event_target_value(&event));
                    }>
                        <option value="local">"本地"</option>
                        <option value="cloudflare_r2">"R2"</option>
                        <option value="onedrive">"OneDrive"</option>
                        <option value="oracle_s3">"S3"</option>
                        <option value="oracle_oci_native">"OCI"</option>
                        <option value="s3_compatible">"S3 兼容"</option>
                    </select>
                    <Show when=move || signal_string_eq(provider_kind, "local")>
                        <input node_ref=local_root placeholder="本地根目录，例如 /data/storage"/>
                        <input node_ref=local_public_prefix placeholder="公开访问前缀，例如 /files"/>
                    </Show>
                    <Show when=move || signal_string_eq(provider_kind, "cloudflare_r2")>
                        <input node_ref=r2_account_id placeholder="Cloudflare Account ID"/>
                        <input node_ref=bucket placeholder="存储桶"/>
                        <input node_ref=access_key_id placeholder="R2 API Token Access Key ID"/>
                        <input node_ref=secret_access_key type="password" placeholder="R2 API Token Secret Access Key，编辑留空保留"/>
                        <input node_ref=session_token type="password" placeholder="临时凭据 Session Token，可选，编辑留空保留"/>
                        <input node_ref=public_domain placeholder="公开域名，可选"/>
                        <select node_ref=r2_jurisdiction>
                            <option value="default">"Global"</option>
                            <option value="eu">"EU"</option>
                            <option value="fedramp">"FedRAMP"</option>
                        </select>
                        <select node_ref=access_mode>
                            <option value="signed_url">"签名 URL"</option>
                            <option value="public_domain">"公开域名直出"</option>
                            <option value="proxy">"系统代理"</option>
                        </select>
                        <input node_ref=presigned_url_ttl_seconds type="number" placeholder="签名 URL 有效期秒数，例如 3600"/>
                        <input node_ref=endpoint placeholder="Endpoint 覆盖，可选"/>
                        <input node_ref=region placeholder="Region 覆盖，默认 auto"/>
                    </Show>
                    <Show when=move || signal_string_matches(provider_kind, &["oracle_s3", "s3_compatible"])>
                        <input node_ref=endpoint placeholder="服务地址"/>
                        <input node_ref=region placeholder="区域"/>
                        <input node_ref=bucket placeholder="存储桶"/>
                        <input node_ref=access_key_id placeholder="访问密钥 ID"/>
                        <input node_ref=secret_access_key type="password" placeholder="访问密钥 Secret，编辑留空保留"/>
                        <input node_ref=public_domain placeholder="公开域名，可选"/>
                        <input node_ref=presigned_url_ttl_seconds type="number" placeholder="签名 URL 有效期秒数，例如 3600"/>
                    </Show>
                    <Show when=move || signal_string_eq(provider_kind, "onedrive")>
                        <input node_ref=client_id placeholder="客户端 ID"/>
                        <input node_ref=tenant_id placeholder="租户 ID"/>
                        <input node_ref=client_secret type="password" placeholder="客户端密钥，编辑留空保留"/>
                        <input node_ref=refresh_token type="password" placeholder="刷新令牌，可选，编辑留空保留"/>
                        <input node_ref=drive_email placeholder="账号邮箱"/>
                        <input node_ref=root_dir placeholder="根目录，例如 TideImages"/>
                    </Show>
                    <Show when=move || signal_string_eq(provider_kind, "oracle_oci_native")>
                        <input node_ref=region placeholder="区域，例如 uk-london-1"/>
                        <input node_ref=namespace placeholder="命名空间，例如 lr75wo7ktlpn"/>
                        <input node_ref=bucket placeholder="存储桶名称，不是 Bucket OCID"/>
                        <input node_ref=tenancy_ocid placeholder="Tenancy OCID，ocid1.tenancy..."/>
                        <input node_ref=user_ocid placeholder="User OCID，ocid1.user..."/>
                        <input node_ref=fingerprint placeholder="API Key 指纹，例如 02:16:..."/>
                        <textarea node_ref=private_key placeholder="私钥 PEM 内容，必须以 -----BEGIN 开头，编辑留空保留"></textarea>
                        <input node_ref=public_domain placeholder="公开域名，可选"/>
                    </Show>
                    <input node_ref=path_prefix placeholder="路径前缀，可选"/>
                    <label class="checkbox-row"><input node_ref=enabled type="checkbox" checked/>"启用存储"</label>
                    <input node_ref=priority type="number" placeholder="优先级，数字越小越靠前"/>
                    <div class="actions">
                        <button type="submit" disabled=move || signal_bool(saving_provider)>
                            {move || {
                                if signal_bool(saving_provider) {
                                    "保存中..."
                                } else if signal_option_is_some(editing_provider) {
                                    "保存存储"
                                } else {
                                    "添加存储"
                                }
                            }}
                        </button>
                        <button class="secondary" type="button" on:click=move |_| reset_form()>"清空表单"</button>
                    </div>
                </form>
            </section>
            <div class="admin-toolbar">
                <input node_ref=filter_q placeholder="搜索存储名称 / 编号"/>
                <select node_ref=filter_type>
                    <option value="">"全部类型"</option>
                    <option value="local">"本地"</option>
                    <option value="cloudflare_r2">"R2"</option>
                    <option value="onedrive">"OneDrive"</option>
                    <option value="oracle_s3">"S3"</option>
                    <option value="oracle_oci_native">"OCI"</option>
                    <option value="s3_compatible">"S3 兼容"</option>
                </select>
                <select node_ref=filter_status>
                    <option value="">"全部状态"</option>
                    <option value="enabled">"已启用"</option>
                    <option value="disabled">"已停用"</option>
                    <option value="default">"默认存储"</option>
                </select>
                <button type="button" on:click=move |_| {
                    let _ = page.try_set(1);
                    load_providers();
                }>"筛选存储"</button>
                <button type="button" on:click=move |_| load_providers()>"刷新存储"</button>
                <button type="button" disabled=move || signal_bool(health_loading) on:click=move |_| check_health()>
                    {move || if signal_bool(health_loading) { "检查中..." } else { "检查当前页健康" }}
                </button>
                <button class="danger" type="button" disabled=move || signal_bool(deleting) on:click=move |_| bulk_delete()>
                    {move || if signal_bool(deleting) { "删除中..." } else { "删除选中" }}
                </button>
                <span class="muted">{move || page_summary(signal_i64(page), signal_i64(page_size), signal_i64(total), signal_bool(loading))}</span>
            </div>
            <div class="admin-table-wrap">
                <table class="admin-table">
                    <thead><tr><th>"选择"</th><th>"名称"</th><th>"类型"</th><th>"状态"</th><th>"优先级"</th><th>"默认"</th><th>"操作"</th></tr></thead>
                    <tbody>
                        {move || providers.try_get().unwrap_or_default().into_iter().map(|provider| {
                            let id = json_string(&provider, "id");
                            let selected_id = id.clone();
                            let toggle_id = id.clone();
                            let connection_id = id.clone();
                            let upload_id = id.clone();
                            let delete_test_id = id.clone();
                            let default_id = id.clone();
                            let row_delete_id = id.clone();
                            let edit_id = id.clone();
                            let connection_busy = format!("{id}:test-connection");
                            let upload_busy = format!("{id}:test-upload");
                            let delete_test_busy = format!("{id}:test-delete");
                            let default_busy = format!("{id}:set-default");
                            let enabled_value = provider.get("enabled").and_then(Value::as_bool).unwrap_or(false);
                            let default_value = provider.get("is_default").and_then(Value::as_bool).unwrap_or(false);
                            let health_item = health.try_get().unwrap_or_default().into_iter().find(|item| json_string(item, "id") == id);
                            let healthy = health_item.as_ref().and_then(|item| item.get("healthy")).and_then(Value::as_bool).unwrap_or(false);
                            let health_error = health_item.as_ref().map(|item| json_string(item, "error")).unwrap_or_default();
                            let status_value = if healthy {
                                "healthy".to_string()
                            } else if health_error.is_empty() {
                                if enabled_value { "pending".to_string() } else { "disabled".to_string() }
                            } else {
                                "error".to_string()
                            };
                            view! {
                                <tr>
                                    <td data-label="选择">
                                        <input
                                            type="checkbox"
                                            checked=move || selected.try_get().unwrap_or_default().iter().any(|item| item == &selected_id)
                                            on:change=move |event| toggle_selected(toggle_id.clone(), checked_event(&event))
                                        />
                                    </td>
                                    <td data-label="名称"><strong>{json_string(&provider, "name")}</strong></td>
                                    <td data-label="类型">{storage_type_label(&json_string(&provider, "provider_type"))}</td>
                                    <td data-label="状态">
                                        <StorageStatusBadge
                                            value=if enabled_value { status_value } else { "disabled".to_string() }
                                            error=health_error
                                        />
                                    </td>
                                    <td data-label="优先级">{json_i64(&provider, "priority")}</td>
                                    <td data-label="默认">{bool_label(default_value, "默认", "否")}</td>
                                    <td data-label="操作">
                                        <div class="row-actions">
                                            <button type="button" disabled=move || signal_option_is_some(provider_action) on:click=move |_| storage_action(connection_id.clone(), "test-connection")>
                                                {move || if signal_option_eq(provider_action, connection_busy.as_str()) { "连接中..." } else { "连接" }}
                                            </button>
                                            <button type="button" disabled=move || signal_option_is_some(provider_action) on:click=move |_| storage_action(upload_id.clone(), "test-upload")>
                                                {move || if signal_option_eq(provider_action, upload_busy.as_str()) { "上传中..." } else { "上传测试" }}
                                            </button>
                                            <button type="button" disabled=move || signal_option_is_some(provider_action) on:click=move |_| storage_action(delete_test_id.clone(), "test-delete")>
                                                {move || if signal_option_eq(provider_action, delete_test_busy.as_str()) { "删除中..." } else { "删除测试" }}
                                            </button>
                                            <button type="button" disabled=move || signal_option_is_some(provider_action) on:click=move |_| storage_action(default_id.clone(), "set-default")>
                                                {move || if signal_option_eq(provider_action, default_busy.as_str()) { "设置中..." } else { "默认" }}
                                            </button>
                                            <button type="button" on:click=move |_| edit_provider(edit_id.clone())>"编辑"</button>
                                            <button class="danger" type="button" disabled=move || signal_bool(deleting) on:click=move |_| delete_provider(row_delete_id.clone())>"删除"</button>
                                        </div>
                                    </td>
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            </div>
            <Show when=move || providers.try_get().unwrap_or_default().is_empty()>
                <div class="empty">"暂无存储配置"</div>
            </Show>
            <AdminPager page=page page_size=page_size total=total loading=loading load=load_providers/>
            <section class="admin-section storage-route-panel">
                <div class="section-head">
                    <div>
                        <h3>{move || if signal_option_is_some(editing_route) { "编辑存储路由" } else { "存储路由策略" }}</h3>
                        <p class="muted">"匹配顺序：用户、用户组、角色、全局；未命中时沿用配额默认存储和全局默认存储。"</p>
                    </div>
                    <div class="route-summary">
                        <span>{move || format!("启用 {} 条", json_i64(&route_summary.try_get().unwrap_or_else(|| json!({})), "active_routes"))}</span>
                        <span>"用户 > 组 > 角色 > 全局"</span>
                    </div>
                </div>
                <form class="admin-form admin-settings-form" on:submit=save_route>
                    <input node_ref=route_name placeholder="路由名称，可留空自动命名"/>
                    <select node_ref=route_scope_type>
                        <option value="global">"全局"</option>
                        <option value="role">"角色"</option>
                        <option value="group">"用户组"</option>
                        <option value="user">"用户"</option>
                    </select>
                    <input node_ref=route_scope_value placeholder="作用域值：角色 user / 用户组 code / 用户 UUID，全局留空"/>
                    <select node_ref=route_provider_id>
                        <option value="">"选择目标存储"</option>
                        {move || providers.try_get().unwrap_or_default().into_iter().map(|provider| {
                            let id = json_string(&provider, "id");
                            let enabled_label = if provider.get("enabled").and_then(Value::as_bool).unwrap_or(false) { "" } else { " · 已停用" };
                            view! {
                                <option value=id.clone()>
                                    {format!("{} · {}{}", json_string(&provider, "name"), storage_type_label(&json_string(&provider, "provider_type")), enabled_label)}
                                </option>
                            }
                        }).collect_view()}
                    </select>
                    <input node_ref=route_priority type="number" placeholder="优先级，数字越小越靠前"/>
                    <input node_ref=route_note placeholder="备注，可选"/>
                    <label class="checkbox-row"><input node_ref=route_enabled type="checkbox" checked/>"启用路由"</label>
                    <div class="actions">
                        <button type="submit">{move || if signal_option_is_some(editing_route) { "保存路由" } else { "添加路由" }}</button>
                        <button class="secondary" type="button" on:click=move |_| reset_route_form()>"清空路由"</button>
                    </div>
                </form>
            </section>
            <div class="admin-toolbar">
                <input node_ref=route_filter_q placeholder="搜索路由 / 作用域 / 存储"/>
                <select node_ref=route_filter_scope>
                    <option value="">"全部作用域"</option>
                    <option value="global">"全局"</option>
                    <option value="role">"角色"</option>
                    <option value="group">"用户组"</option>
                    <option value="user">"用户"</option>
                </select>
                <select node_ref=route_filter_status>
                    <option value="">"全部状态"</option>
                    <option value="enabled">"已启用"</option>
                    <option value="disabled">"已停用"</option>
                </select>
                <button type="button" on:click=move |_| {
                    let _ = route_page.try_set(1);
                    load_routes();
                }>"筛选路由"</button>
                <button type="button" on:click=move |_| load_routes()>"刷新路由"</button>
                <span class="muted">{move || page_summary(signal_i64(route_page), signal_i64(route_page_size), signal_i64(route_total), signal_bool(route_loading))}</span>
            </div>
            <div class="admin-table-wrap">
                <table class="admin-table">
                    <thead><tr><th>"名称"</th><th>"作用域"</th><th>"目标存储"</th><th>"状态"</th><th>"优先级"</th><th>"备注"</th><th>"操作"</th></tr></thead>
                    <tbody>
                        {move || routes.try_get().unwrap_or_default().into_iter().map(|route| {
                            let id = json_string(&route, "id");
                            let edit_id = id.clone();
                            let delete_id = id.clone();
                            let toggle_id = id.clone();
                            let enabled_value = route.get("enabled").and_then(Value::as_bool).unwrap_or(false);
                            let action = if enabled_value { "disable" } else { "enable" };
                            view! {
                                <tr>
                                    <td data-label="名称"><strong>{json_string(&route, "name")}</strong><small>{short_text(&json_string(&route, "id"), 12)}</small></td>
                                    <td data-label="作用域">{storage_scope_label(&json_string(&route, "scope_type"), &json_string(&route, "scope_value"))}</td>
                                    <td data-label="目标存储"><strong>{json_string(&route, "storage_provider_name")}</strong><small>{storage_type_label(&json_string(&route, "storage_provider_type"))}</small></td>
                                    <td data-label="状态"><StatusBadge value=if enabled_value { "active".to_string() } else { "disabled".to_string() }/></td>
                                    <td data-label="优先级">{json_i64(&route, "priority")}</td>
                                    <td data-label="备注">{display_or_dash(&json_string(&route, "note"))}</td>
                                    <td data-label="操作">
                                        <div class="row-actions">
                                            <button type="button" on:click=move |_| edit_route(edit_id.clone())>"编辑"</button>
                                            <button type="button" on:click=move |_| route_action(toggle_id.clone(), action)>{if enabled_value { "停用" } else { "启用" }}</button>
                                            <button class="danger" type="button" on:click=move |_| route_action(delete_id.clone(), "delete")>"删除"</button>
                                        </div>
                                    </td>
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            </div>
            <Show when=move || routes.try_get().unwrap_or_default().is_empty()>
                <div class="empty">"暂无存储路由策略"</div>
            </Show>
            <AdminPager page=route_page page_size=route_page_size total=route_total loading=route_loading load=load_routes/>
        </div>
    }
}

#[component]
fn UploadSettingsPanel(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let allowed_mime_types = NodeRef::<Input>::new();
    let webp_enabled = NodeRef::<Input>::new();
    let webp_max_width = NodeRef::<Input>::new();
    let webp_max_height = NodeRef::<Input>::new();
    let webp_quality = NodeRef::<Input>::new();
    let remove_exif = NodeRef::<Input>::new();
    let max_tags_per_image = NodeRef::<Input>::new();
    let max_tag_length = NodeRef::<Input>::new();
    let tag_sensitive_words = NodeRef::<Input>::new();
    let tag_review_required = NodeRef::<Input>::new();
    let load = move || {
        let token_value = token.get();
        spawn_local(async move {
            match api_get::<Value>("/api/settings/upload", &token_value).await {
                Ok(value) => fill_upload_settings_form(
                    &value,
                    allowed_mime_types,
                    webp_enabled,
                    webp_max_width,
                    webp_max_height,
                    webp_quality,
                    remove_exif,
                    max_tags_per_image,
                    max_tag_length,
                    tag_sensitive_words,
                    tag_review_required,
                ),
                Err(err) => notify(err),
            }
        });
    };
    Effect::new(move |_| load());
    let save = move |event: SubmitEvent| {
        event.prevent_default();
        let token_value = token.get();
        let body = json!({
            "allowed_mime_types": parse_comma_values(&input_value(allowed_mime_types).unwrap_or_default()),
            "webp_enabled": checked_value(webp_enabled),
            "webp_max_width": input_i64(webp_max_width).unwrap_or(512),
            "webp_max_height": input_i64(webp_max_height).unwrap_or(512),
            "webp_quality": input_i64(webp_quality).unwrap_or(75),
            "remove_exif": checked_value(remove_exif),
            "max_tags_per_image": input_i64(max_tags_per_image).unwrap_or(10),
            "max_tag_length": input_i64(max_tag_length).unwrap_or(32),
            "tag_sensitive_words": parse_comma_values(&input_value(tag_sensitive_words).unwrap_or_default()),
            "tag_review_required": checked_value(tag_review_required),
        });
        spawn_local(async move {
            match api_json::<Value, _>("/api/admin/settings/upload", &token_value, "PUT", &body)
                .await
            {
                Ok(_) => notify("上传设置已保存".to_string()),
                Err(err) => notify(err),
            }
        });
    };
    view! {
        <section class="admin-section">
            <form class="form admin-settings-form" on:submit=save>
                <label>"允许文件类型"<input node_ref=allowed_mime_types placeholder="image/jpeg,image/png,image/webp"/></label>
                <label class="checkbox-row"><input node_ref=webp_enabled type="checkbox"/>"启用 WebP 预览图"</label>
                <label>"WebP 最大宽度"<input node_ref=webp_max_width type="number" min="64" max="4096"/></label>
                <label>"WebP 最大高度"<input node_ref=webp_max_height type="number" min="64" max="4096"/></label>
                <label>"WebP 质量"<input node_ref=webp_quality type="number" min="1" max="100"/></label>
                <label class="checkbox-row"><input node_ref=remove_exif type="checkbox"/>"移除 EXIF"</label>
                <label>"单图最多标签数"<input node_ref=max_tags_per_image type="number" min="0"/></label>
                <label>"标签最大长度"<input node_ref=max_tag_length type="number" min="1"/></label>
                <label>"标签敏感词"<input node_ref=tag_sensitive_words placeholder="逗号分隔"/></label>
                <label class="checkbox-row"><input node_ref=tag_review_required type="checkbox"/>"新标签需要审核"</label>
                <div class="actions">
                    <button type="submit">"保存上传设置"</button>
                    <button class="secondary" type="button" on:click=move |_| load()>"重新加载"</button>
                </div>
            </form>
        </section>
    }
}

#[component]
fn RandomSettingsPanel(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let enabled = NodeRef::<Input>::new();
    let default_image = NodeRef::<Select>::new();
    let limit_enabled = NodeRef::<Input>::new();
    let allow_tag_filter = NodeRef::<Input>::new();
    let allow_orientation_filter = NodeRef::<Input>::new();
    let allow_resolution_filter = NodeRef::<Input>::new();
    let no_match_strategy = NodeRef::<Select>::new();
    let load = move || {
        let token_value = token.get();
        spawn_local(async move {
            match api_get::<Value>("/api/settings/random", &token_value).await {
                Ok(value) => fill_random_settings_form(
                    &value,
                    RandomSettingsRefs {
                        enabled,
                        default_image,
                        limit_enabled,
                        allow_tag_filter,
                        allow_orientation_filter,
                        allow_resolution_filter,
                        no_match_strategy,
                    },
                ),
                Err(err) => notify(err),
            }
        });
    };
    Effect::new(move |_| load());
    let save = move |event: SubmitEvent| {
        event.prevent_default();
        let token_value = token.get();
        let body = json!({
            "enabled": checked_value(enabled),
            "default_image": select_value(default_image).unwrap_or_else(|| "preview".to_string()),
            "limit_enabled": checked_value(limit_enabled),
            "allow_tag_filter": checked_value(allow_tag_filter),
            "allow_orientation_filter": checked_value(allow_orientation_filter),
            "allow_resolution_filter": checked_value(allow_resolution_filter),
            "no_match_strategy": select_value(no_match_strategy).unwrap_or_else(|| "not_found".to_string()),
        });
        spawn_local(async move {
            match api_json::<Value, _>("/api/admin/settings/random", &token_value, "PUT", &body)
                .await
            {
                Ok(_) => notify("随机图设置已保存".to_string()),
                Err(err) => notify(err),
            }
        });
    };
    view! {
        <section class="admin-section">
            <form class="form admin-settings-form" on:submit=save>
                <label class="checkbox-row"><input node_ref=enabled type="checkbox"/>"启用随机图"</label>
                <label>"默认输出图片"<select node_ref=default_image>
                    <option value="preview">"WebP 预览图"</option>
                    <option value="original">"原图"</option>
                </select></label>
                <label class="checkbox-row"><input node_ref=limit_enabled type="checkbox"/>"计入调用配额"</label>
                <label class="checkbox-row"><input node_ref=allow_tag_filter type="checkbox"/>"允许标签筛选"</label>
                <label class="checkbox-row"><input node_ref=allow_orientation_filter type="checkbox"/>"允许方向筛选"</label>
                <label class="checkbox-row"><input node_ref=allow_resolution_filter type="checkbox"/>"允许分辨率筛选"</label>
                <label>"无匹配策略"<select node_ref=no_match_strategy>
                    <option value="not_found">"返回未找到"</option>
                </select></label>
                <div class="actions">
                    <button type="submit">"保存随机图设置"</button>
                    <button class="secondary" type="button" on:click=move |_| load()>"重新加载"</button>
                </div>
            </form>
        </section>
    }
}

#[component]
fn AppearanceSettings(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
    theme: RwSignal<ThemeSettings>,
) -> impl IntoView {
    let preset = NodeRef::<Select>::new();
    let mode = NodeRef::<Select>::new();
    let radius = NodeRef::<Input>::new();
    let blur = NodeRef::<Input>::new();
    let mobile_blur = NodeRef::<Input>::new();
    let card_opacity = NodeRef::<Input>::new();
    let primary_color = NodeRef::<Input>::new();
    let accent_color = NodeRef::<Input>::new();
    let background_color = NodeRef::<Input>::new();
    let surface_color = NodeRef::<Input>::new();
    let font = NodeRef::<Input>::new();
    let background_image = NodeRef::<Input>::new();
    let simplify_mobile_effects = NodeRef::<Input>::new();
    let load = move || {
        let token_value = token.get();
        spawn_local(async move {
            match api_get::<Value>("/api/settings/theme", &token_value).await {
                Ok(value) => {
                    let settings = ThemeSettings::from_value(&value);
                    fill_theme_form(
                        &settings,
                        preset,
                        mode,
                        radius,
                        blur,
                        mobile_blur,
                        card_opacity,
                        primary_color,
                        accent_color,
                        background_color,
                        surface_color,
                        font,
                        background_image,
                        simplify_mobile_effects,
                    );
                    theme.set(settings);
                }
                Err(err) => notify(err),
            }
        });
    };
    Effect::new(move |_| load());
    let apply_preset = move |_| {
        let settings =
            theme_preset(&select_value(preset).unwrap_or_else(|| "blue_white".to_string()));
        fill_theme_form(
            &settings,
            preset,
            mode,
            radius,
            blur,
            mobile_blur,
            card_opacity,
            primary_color,
            accent_color,
            background_color,
            surface_color,
            font,
            background_image,
            simplify_mobile_effects,
        );
        theme.set(settings);
    };
    let save = move |event: SubmitEvent| {
        event.prevent_default();
        let body = json!({
            "mode": select_value(mode).unwrap_or_else(|| "light".to_string()),
            "preset": select_value(preset).unwrap_or_else(|| "blue_white".to_string()),
            "radius": input_i64(radius).unwrap_or(16),
            "blur": input_i64(blur).unwrap_or(18),
            "mobile_blur": input_i64(mobile_blur).unwrap_or(10),
            "card_opacity": input_f64(card_opacity).unwrap_or(0.72),
            "primary_color": input_value(primary_color).unwrap_or_else(|| "#1d6fd8".to_string()),
            "accent_color": input_value(accent_color).unwrap_or_else(|| "#58b7ff".to_string()),
            "background_color": input_value(background_color).unwrap_or_else(|| "#eef7ff".to_string()),
            "surface_color": input_value(surface_color).unwrap_or_else(|| "#ffffff".to_string()),
            "font": input_value(font).unwrap_or_else(|| "系统圆体".to_string()),
            "background_image": input_value(background_image).unwrap_or_default(),
            "simplify_mobile_effects": checked_value(simplify_mobile_effects),
        });
        let token_value = token.get();
        spawn_local(async move {
            match api_json::<Value, _>("/api/admin/settings/theme", &token_value, "PUT", &body)
                .await
            {
                Ok(_) => {
                    let next = ThemeSettings::from_value(&body);
                    theme.set(next.clone());
                    notify("外观设置已保存".to_string());
                }
                Err(err) => notify(err),
            }
        });
    };
    view! {
        <form class="form appearance-form" on:submit=save>
            <div class="grid two">
                <label>"主题预设"<select node_ref=preset on:change=apply_preset>
                    <option value="blue_white">"蓝白默认"</option>
                    <option value="macaron">"马卡龙"</option>
                    <option value="dark_ocean">"深海"</option>
                </select></label>
                <label>"模式"<select node_ref=mode>
                    <option value="light">"浅色"</option>
                    <option value="dark">"深色"</option>
                    <option value="auto">"跟随系统"</option>
                </select></label>
                <label>"圆角"<input node_ref=radius type="number" min="0" max="32"/></label>
                <label>"模糊强度"<input node_ref=blur type="number" min="0" max="32"/></label>
                <label>"移动端模糊"<input node_ref=mobile_blur type="number" min="0" max="24"/></label>
                <label>"卡片透明度"<input node_ref=card_opacity type="number" min="0.35" max="0.95" step="0.01"/></label>
                <label>"主色"<input node_ref=primary_color type="color"/></label>
                <label>"辅色"<input node_ref=accent_color type="color"/></label>
                <label>"背景色"<input node_ref=background_color type="color"/></label>
                <label>"卡片色"<input node_ref=surface_color type="color"/></label>
                <label>"字体"<input node_ref=font placeholder="系统圆体"/></label>
                <label>"背景图"<input node_ref=background_image placeholder="https://..."/></label>
            </div>
            <label class="checkbox-row"><input node_ref=simplify_mobile_effects type="checkbox"/>"移动端简化背景效果"</label>
            <div class="actions">
                <button type="submit">"保存外观"</button>
                <button class="secondary" type="button" on:click=move |_| load()>"重新加载"</button>
            </div>
        </form>
    }
}

#[component]
fn AdminSystemSettings(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
    theme: RwSignal<ThemeSettings>,
) -> impl IntoView {
    let section = RwSignal::new("site".to_string());
    let item = move |key: &'static str, title: &'static str| {
        view! {
            <button
                type="button"
                class=move || if section.get() == key { "selected" } else { "" }
                on:click=move |_| section.set(key.to_string())
            >
                {title}
            </button>
        }
    };
    view! {
        <div class="admin-hub">
            <div class="admin-subtabs settings-subtabs">
                {item("site", "站点")}
                {item("upload", "上传")}
                {item("random", "随机图")}
                {item("appearance", "外观")}
                {item("smtp", "邮件")}
                {item("logs", "日志")}
                {item("backups", "备份恢复")}
            </div>
            {move || match section.get().as_str() {
                "upload" => view! {
                    <>
                        <AdminHubHeader title="上传设置" description="管理允许文件类型、WebP 预览图、EXIF 清理和标签基础策略。"/>
                        <UploadSettingsPanel token=token notify=notify/>
                    </>
                }.into_any(),
                "random" => view! {
                    <>
                        <AdminHubHeader title="随机图设置" description="控制随机图开关、默认返回预览图或原图、筛选能力和调用配额。"/>
                        <RandomSettingsPanel token=token notify=notify/>
                    </>
                }.into_any(),
                "appearance" => view! {
                    <>
                        <AdminHubHeader title="外观设置" description="调整蓝白默认主题、琉璃模糊、圆角、色彩、字体和移动端性能策略。"/>
                        <AppearanceSettings token=token notify=notify theme=theme/>
                    </>
                }.into_any(),
                "smtp" => view! {
                    <>
                        <AdminHubHeader title="邮件设置" description="配置 SMTP 主机、发件身份和启用状态，用于邮箱验证和通知。"/>
                        <SmtpSettingsPanel token=token notify=notify/>
                    </>
                }.into_any(),
                "logs" => view! {
                    <>
                        <AdminHubHeader title="日志中心" description="分页查看系统日志和管理员操作日志，避免后台首屏拉取大列表。"/>
                        <AdminLogs token=token notify=notify/>
                    </>
                }.into_any(),
                "backups" => view! {
                    <>
                        <AdminHubHeader title="备份恢复" description="创建站点备份并按需恢复设置、主题、SMTP、存储和元数据。"/>
                        <AdminBackups token=token notify=notify/>
                    </>
                }.into_any(),
                _ => view! {
                    <>
                        <AdminHubHeader title="站点设置" description="维护站点标题、副标题、访客上传开关和访客审核策略。"/>
                        <SiteSettingsPanel token=token notify=notify/>
                    </>
                }.into_any(),
            }}
        </div>
    }
}

#[component]
fn SiteSettingsPanel(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let title = NodeRef::<Input>::new();
    let subtitle = NodeRef::<Input>::new();
    let guest_upload_enabled = NodeRef::<Input>::new();
    let guest_review_strategy = NodeRef::<Select>::new();
    let load = move || {
        let token_value = token.get();
        spawn_local(async move {
            match api_get::<Value>("/api/settings/site", &token_value).await {
                Ok(value) => fill_site_settings_form(
                    &value,
                    title,
                    subtitle,
                    guest_upload_enabled,
                    guest_review_strategy,
                ),
                Err(err) => notify(err),
            }
        });
    };
    Effect::new(move |_| load());
    let save = move |event: SubmitEvent| {
        event.prevent_default();
        let token_value = token.get();
        let body = json!({
            "title": input_value(title).unwrap_or_else(|| "潮汐图床".to_string()),
            "subtitle": input_value(subtitle).unwrap_or_default(),
            "guest_upload_enabled": checked_value(guest_upload_enabled),
            "guest_review_strategy": select_value(guest_review_strategy).unwrap_or_else(|| "manual_required".to_string()),
        });
        spawn_local(async move {
            match api_json::<Value, _>("/api/admin/settings/site", &token_value, "PUT", &body).await
            {
                Ok(_) => notify("站点设置已保存".to_string()),
                Err(err) => notify(err),
            }
        });
    };
    view! {
        <section class="admin-section">
            <h3>"站点设置"</h3>
            <form class="form admin-settings-form" on:submit=save>
                <label>"站点名称"<input node_ref=title placeholder="潮汐图床"/></label>
                <label>"站点副标题"<input node_ref=subtitle placeholder=""/></label>
                <label class="checkbox-row"><input node_ref=guest_upload_enabled type="checkbox"/>"允许访客上传"</label>
                <label>"访客审核策略"<select node_ref=guest_review_strategy>
                    <option value="manual_required">"需要人工审核"</option>
                    <option value="auto">"自动放行"</option>
                    <option value="group">"按用户组策略"</option>
                    <option value="reject">"拒绝访客上传"</option>
                </select></label>
                <div class="actions">
                    <button type="submit">"保存站点设置"</button>
                    <button class="secondary" type="button" on:click=move |_| load()>"重新加载"</button>
                </div>
            </form>
        </section>
    }
}

#[component]
fn SmtpSettingsPanel(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let name = NodeRef::<Input>::new();
    let host = NodeRef::<Input>::new();
    let port = NodeRef::<Input>::new();
    let username = NodeRef::<Input>::new();
    let password = NodeRef::<Input>::new();
    let from_email = NodeRef::<Input>::new();
    let from_name = NodeRef::<Input>::new();
    let enabled = NodeRef::<Input>::new();
    let load = move || {
        let token_value = token.get();
        spawn_local(async move {
            match api_get::<Value>("/api/admin/settings/smtp", &token_value).await {
                Ok(value) => fill_smtp_settings_form(
                    &value, name, host, port, username, password, from_email, from_name, enabled,
                ),
                Err(err) => notify(err),
            }
        });
    };
    Effect::new(move |_| load());
    let save = move |event: SubmitEvent| {
        event.prevent_default();
        let token_value = token.get();
        let body = json!({
            "name": input_value(name).unwrap_or_else(|| "SMTP".to_string()),
            "host": input_value(host).unwrap_or_default(),
            "port": input_i64(port).unwrap_or(587),
            "username": input_value(username).unwrap_or_default(),
            "password": input_value(password).unwrap_or_default(),
            "from_email": input_value(from_email).unwrap_or_default(),
            "from_name": input_value(from_name).unwrap_or_else(|| "潮汐图床".to_string()),
            "enabled": checked_value(enabled),
        });
        spawn_local(async move {
            match api_json::<Value, _>("/api/admin/settings/smtp", &token_value, "PUT", &body).await
            {
                Ok(_) => notify("SMTP 设置已保存".to_string()),
                Err(err) => notify(err),
            }
        });
    };
    view! {
        <section class="admin-section">
            <h3>"SMTP 设置"</h3>
            <form class="form admin-settings-form" on:submit=save>
                <label class="checkbox-row"><input node_ref=enabled type="checkbox"/>"启用邮件发送"</label>
                <label>"配置名称"<input node_ref=name placeholder="SMTP"/></label>
                <label>"SMTP 主机"<input node_ref=host placeholder="smtp.example.com"/></label>
                <label>"端口"<input node_ref=port type="number" min="1"/></label>
                <label>"用户名"<input node_ref=username/></label>
                <label>"密码"<input node_ref=password type="password" placeholder="留空或 ******** 表示保留"/></label>
                <label>"发件邮箱"<input node_ref=from_email placeholder="noreply@example.com"/></label>
                <label>"发件名称"<input node_ref=from_name placeholder="潮汐图床"/></label>
                <div class="actions">
                    <button type="submit">"保存 SMTP"</button>
                    <button class="secondary" type="button" on:click=move |_| load()>"重新加载"</button>
                </div>
            </form>
        </section>
    }
}

#[component]
fn AdminLogs(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let system_logs = RwSignal::new(Vec::<Value>::new());
    let operation_logs = RwSignal::new(Vec::<Value>::new());
    let system_page = RwSignal::new(1_i64);
    let system_page_size = RwSignal::new(40_i64);
    let system_total = RwSignal::new(0_i64);
    let system_loading = RwSignal::new(false);
    let operation_page = RwSignal::new(1_i64);
    let operation_page_size = RwSignal::new(40_i64);
    let operation_total = RwSignal::new(0_i64);
    let operation_loading = RwSignal::new(false);
    let system_level = NodeRef::<Select>::new();
    let system_q = NodeRef::<Input>::new();
    let operation_q = NodeRef::<Input>::new();
    let load = move || {
        let token_value = token.get();
        let system_path = admin_page_path(
            "/api/admin/logs/system/page",
            system_page.get(),
            system_page_size.get(),
            &[
                ("level", select_value(system_level).unwrap_or_default()),
                ("q", input_value(system_q).unwrap_or_default()),
            ],
        );
        let operation_path = admin_page_path(
            "/api/admin/logs/operations/page",
            operation_page.get(),
            operation_page_size.get(),
            &[("q", input_value(operation_q).unwrap_or_default())],
        );
        system_loading.set(true);
        operation_loading.set(true);
        spawn_local(async move {
            match api_get::<Page<Value>>(&system_path, &token_value).await {
                Ok(value) => {
                    system_total.set(value.total);
                    system_logs.set(value.items);
                }
                Err(err) => notify(err),
            }
            system_loading.set(false);
            match api_get::<Page<Value>>(&operation_path, &token_value).await {
                Ok(value) => {
                    operation_total.set(value.total);
                    operation_logs.set(value.items);
                }
                Err(err) => notify(err),
            }
            operation_loading.set(false);
        });
    };
    Effect::new(move |_| load());
    view! {
        <div class="admin-stack">
            <div class="admin-toolbar">
                <button type="button" on:click=move |_| load()>"刷新日志"</button>
            </div>
            <section class="admin-section">
                <h3>"系统日志"</h3>
                <div class="filters">
                    <select node_ref=system_level>
                        <option value="">"全部级别"</option>
                        <option value="info">"信息"</option>
                        <option value="warn">"警告"</option>
                        <option value="error">"错误"</option>
                    </select>
                    <input node_ref=system_q placeholder="搜索模块 / 消息"/>
                    <button type="button" on:click=move |_| {
                        system_page.set(1);
                        load();
                    }>"筛选系统日志"</button>
                    <span class="muted">{move || page_summary(system_page.get(), system_page_size.get(), system_total.get(), system_loading.get())}</span>
                </div>
                <div class="admin-table-wrap">
                    <table class="admin-table">
                        <thead><tr><th>"级别"</th><th>"模块"</th><th>"消息"</th><th>"时间"</th></tr></thead>
                        <tbody>
                            {move || system_logs.get().into_iter().map(|item| view! {
                                <tr>
                                    <td data-label="级别"><StatusBadge value=json_string(&item, "level")/></td>
                                    <td data-label="模块">{json_string(&item, "module")}</td>
                                    <td data-label="消息">{display_or_dash(&json_string(&item, "message"))}</td>
                                    <td data-label="时间">{time_label(&json_string(&item, "created_at"))}</td>
                                </tr>
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
                <AdminPager page=system_page page_size=system_page_size total=system_total loading=system_loading load=load/>
            </section>
            <section class="admin-section">
                <h3>"操作日志"</h3>
                <div class="filters">
                    <input node_ref=operation_q placeholder="搜索管理员 / 动作 / 对象"/>
                    <button type="button" on:click=move |_| {
                        operation_page.set(1);
                        load();
                    }>"筛选操作日志"</button>
                    <span class="muted">{move || page_summary(operation_page.get(), operation_page_size.get(), operation_total.get(), operation_loading.get())}</span>
                </div>
                <div class="admin-table-wrap">
                    <table class="admin-table">
                        <thead><tr><th>"管理员"</th><th>"操作"</th><th>"对象"</th><th>"目标"</th><th>"时间"</th></tr></thead>
                        <tbody>
                            {move || operation_logs.get().into_iter().map(|item| view! {
                                <tr>
                                    <td data-label="管理员">{json_string(&item, "admin_user_id")}</td>
                                    <td data-label="操作">{admin_action_label(&json_string(&item, "action"))}</td>
                                    <td data-label="对象">{admin_target_label(&json_string(&item, "target_type"))}</td>
                                    <td data-label="目标">{display_or_dash(&json_string(&item, "target_id"))}</td>
                                    <td data-label="时间">{time_label(&json_string(&item, "created_at"))}</td>
                                </tr>
                            }).collect_view()}
                        </tbody>
                    </table>
                </div>
                <AdminPager page=operation_page page_size=operation_page_size total=operation_total loading=operation_loading load=load/>
            </section>
        </div>
    }
}

#[component]
fn AdminMigrations(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let migrations = RwSignal::new(Vec::<Value>::new());
    let detail = RwSignal::new(None::<Value>);
    let page = RwSignal::new(1_i64);
    let page_size = RwSignal::new(40_i64);
    let total = RwSignal::new(0_i64);
    let loading = RwSignal::new(false);
    let detail_items_page = RwSignal::new(1_i64);
    let detail_items_total = RwSignal::new(0_i64);
    let source = NodeRef::<Input>::new();
    let target = NodeRef::<Input>::new();
    let mode = NodeRef::<Select>::new();
    let list_status = NodeRef::<Select>::new();
    let filter_status = NodeRef::<Select>::new();
    let filter_tag = NodeRef::<Input>::new();
    let filter_user_id = NodeRef::<Input>::new();
    let load = move || {
        let token_value = token.get();
        let path = admin_page_path(
            "/api/admin/migrations/page",
            page.get(),
            page_size.get(),
            &[("status", select_value(list_status).unwrap_or_default())],
        );
        loading.set(true);
        spawn_local(async move {
            match api_get::<Page<Value>>(&path, &token_value).await {
                Ok(value) => {
                    total.set(value.total);
                    migrations.set(value.items);
                }
                Err(err) => notify(err),
            }
            loading.set(false);
        });
    };
    Effect::new(move |_| load());
    let create = move |event: SubmitEvent| {
        event.prevent_default();
        if !confirm("确认启动迁移任务？") {
            return;
        }
        let token_value = token.get();
        let filter_json = migration_filter_body(filter_status, filter_tag, filter_user_id);
        let body = json!({
            "source_storage_provider_id": input_value(source).unwrap_or_default(),
            "target_storage_provider_id": input_value(target).unwrap_or_default(),
            "migration_mode": select_value(mode).unwrap_or_else(|| "copy".to_string()),
            "filter_json": filter_json,
        });
        spawn_local(async move {
            match api_json::<Value, _>("/api/admin/migrations", &token_value, "POST", &body).await {
                Ok(value) => {
                    detail.set(Some(json!({"task": value, "items": []})));
                    notify("迁移任务已创建".to_string());
                    load();
                }
                Err(err) => notify(err),
            }
        });
    };
    let show_detail = move |id: String| {
        let token_value = token.get();
        detail_items_page.set(1);
        spawn_local(async move {
            let task = api_get::<Value>(&format!("/api/admin/migrations/{id}"), &token_value).await;
            let items = api_get::<Page<Value>>(
                &format!("/api/admin/migrations/{id}/items/page?page=1&page_size=80"),
                &token_value,
            )
            .await;
            match (task, items) {
                (Ok(task), Ok(items)) => {
                    detail_items_total.set(items.total);
                    detail.set(Some(json!({"task": task, "items": items.items})));
                }
                (Ok(task), Err(_)) => detail.set(Some(json!({"task": task, "items": []}))),
                _ => notify("迁移详情加载失败".to_string()),
            }
        });
    };
    let task_action = move |id: String, action: &'static str| {
        if matches!(action, "cancel" | "retry-failed") && !confirm("确认执行这个迁移操作？")
        {
            return;
        }
        let token_value = token.get();
        spawn_local(async move {
            match api_empty(
                &format!("/api/admin/migrations/{id}/{action}"),
                &token_value,
                "POST",
            )
            .await
            {
                Ok(_) => {
                    notify("迁移操作已提交".to_string());
                    load();
                }
                Err(err) => notify(err),
            }
        });
    };
    view! {
        <div class="admin-stack">
            <section class="admin-section">
                <h3>"创建迁移任务"</h3>
                <form id="migrationForm" class="admin-form admin-settings-form" on:submit=create>
                    <input node_ref=source placeholder="来源存储编号"/>
                    <input node_ref=target placeholder="目标存储编号"/>
                    <select node_ref=mode>
                        <option value="copy">"复制"</option>
                        <option value="move">"移动"</option>
                        <option value="backup">"备份"</option>
                    </select>
                    <select node_ref=filter_status>
                        <option value="">"全部图片状态"</option>
                        <option value="active">"正常"</option>
                        <option value="pending_review">"待审核"</option>
                        <option value="trashed">"回收站"</option>
                    </select>
                    <input node_ref=filter_tag placeholder="标签筛选，可选"/>
                    <input node_ref=filter_user_id placeholder="用户编号，可选"/>
                    <button type="submit">"启动迁移"</button>
                </form>
            </section>
            <div class="admin-toolbar">
                <select node_ref=list_status>
                    <option value="">"全部状态"</option>
                    <option value="pending">"等待中"</option>
                    <option value="running">"运行中"</option>
                    <option value="paused">"已暂停"</option>
                    <option value="completed">"已完成"</option>
                    <option value="failed">"失败"</option>
                    <option value="cancelled">"已取消"</option>
                </select>
                <button type="button" on:click=move |_| {
                    page.set(1);
                    load();
                }>"筛选迁移"</button>
                <button type="button" on:click=move |_| load()>"刷新迁移"</button>
                <span class="muted">{move || page_summary(page.get(), page_size.get(), total.get(), loading.get())}</span>
            </div>
            <div class="admin-table-wrap">
                <table class="admin-table">
                    <thead><tr><th>"任务"</th><th>"模式"</th><th>"状态"</th><th>"进度"</th><th>"创建时间"</th><th>"操作"</th></tr></thead>
                    <tbody>
                        {move || migrations.get().into_iter().map(|item| {
                            let id = json_string(&item, "id");
                            let detail_id = id.clone();
                            let pause_id = id.clone();
                            let resume_id = id.clone();
                            let cancel_id = id.clone();
                            let retry_id = id.clone();
                            view! {
                                <tr>
                                    <td data-label="任务"><strong>{id.clone()}</strong></td>
                                    <td data-label="模式">{migration_mode_label(&json_string(&item, "migration_mode"))}</td>
                                    <td data-label="状态"><StatusBadge value=json_string(&item, "status")/></td>
                                    <td data-label="进度">{format!("{}/{}", json_i64(&item, "succeeded_count"), json_i64(&item, "total_count"))}</td>
                                    <td data-label="创建时间">{time_label(&json_string(&item, "created_at"))}</td>
                                    <td data-label="操作">
                                        <div class="row-actions">
                                            <button type="button" on:click=move |_| show_detail(detail_id.clone())>"详情"</button>
                                            <button type="button" on:click=move |_| task_action(pause_id.clone(), "pause")>"暂停"</button>
                                            <button type="button" on:click=move |_| task_action(resume_id.clone(), "resume")>"继续"</button>
                                            <button type="button" on:click=move |_| task_action(cancel_id.clone(), "cancel")>"取消"</button>
                                            <button type="button" on:click=move |_| task_action(retry_id.clone(), "retry-failed")>"重试失败"</button>
                                        </div>
                                    </td>
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            </div>
            <Show when=move || migrations.get().is_empty()>
                <div class="empty">"暂无迁移任务"</div>
            </Show>
            <AdminPager page=page page_size=page_size total=total loading=loading load=load/>
            {move || detail.get().map(|value| {
                let task = value.get("task").cloned().unwrap_or_else(|| json!({}));
                let items = value.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
                view! {
                    <section class="admin-detail">
                        <header class="section-head">
                            <h3>"迁移详情"</h3>
                            <button class="secondary" type="button" on:click=move |_| detail.set(None)>"收起"</button>
                        </header>
                        <div class="detail-fields">
                            <span><small>"任务编号"</small><strong>{json_string(&task, "id")}</strong></span>
                            <span><small>"状态"</small><strong>{status_label(&json_string(&task, "status"))}</strong></span>
                            <span><small>"模式"</small><strong>{migration_mode_label(&json_string(&task, "migration_mode"))}</strong></span>
                            <span><small>"进度"</small><strong>{format!("{}/{}", json_i64(&task, "succeeded_count"), json_i64(&task, "total_count"))}</strong></span>
                        </div>
                        <p class="muted">{format!("明细第 {} 页 · 共 {} 条", detail_items_page.get(), detail_items_total.get())}</p>
                        <div class="admin-table-wrap">
                            <table class="admin-table">
                                <thead><tr><th>"图片"</th><th>"源对象"</th><th>"目标对象"</th><th>"状态"</th><th>"错误"</th></tr></thead>
                                <tbody>
                                    {items.into_iter().take(200).map(|item| view! {
                                        <tr>
                                            <td data-label="图片">{json_string(&item, "image_id")}</td>
                                            <td data-label="源对象">{json_string(&item, "source_object_id")}</td>
                                            <td data-label="目标对象">{json_string(&item, "target_object_id")}</td>
                                            <td data-label="状态"><StatusBadge value=json_string(&item, "status")/></td>
                                            <td data-label="错误">{display_or_dash(&json_string(&item, "error_message"))}</td>
                                        </tr>
                                    }).collect_view()}
                                </tbody>
                            </table>
                        </div>
                    </section>
                }
            }).into_any()}
        </div>
    }
}

#[component]
fn AdminBackups(
    token: RwSignal<String>,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let backups = RwSignal::new(Vec::<Value>::new());
    let detail = RwSignal::new(None::<Value>);
    let page = RwSignal::new(1_i64);
    let page_size = RwSignal::new(40_i64);
    let total = RwSignal::new(0_i64);
    let loading = RwSignal::new(false);
    let list_status = NodeRef::<Select>::new();
    let include_files = NodeRef::<Input>::new();
    let include_logs = NodeRef::<Input>::new();
    let target_storage = NodeRef::<Input>::new();
    let restore_site_settings = NodeRef::<Input>::new();
    let restore_theme_settings = NodeRef::<Input>::new();
    let restore_smtp_settings = NodeRef::<Input>::new();
    let restore_storage_providers = NodeRef::<Input>::new();
    let restore_metadata = NodeRef::<Input>::new();
    let restore_audit = NodeRef::<Input>::new();
    let restore_logs = NodeRef::<Input>::new();
    let load = move || {
        let token_value = token.get();
        let path = admin_page_path(
            "/api/admin/backups/page",
            page.get(),
            page_size.get(),
            &[("status", select_value(list_status).unwrap_or_default())],
        );
        loading.set(true);
        spawn_local(async move {
            match api_get::<Page<Value>>(&path, &token_value).await {
                Ok(value) => {
                    total.set(value.total);
                    backups.set(value.items);
                }
                Err(err) => notify(err),
            }
            loading.set(false);
        });
    };
    Effect::new(move |_| load());
    let create = move |event: SubmitEvent| {
        event.prevent_default();
        if !confirm("确认创建新的系统备份？") {
            return;
        }
        let token_value = token.get();
        let target = input_value(target_storage).filter(|value| !value.is_empty());
        let body = json!({
            "backup_type": "manual",
            "target_storage_provider_id": target,
            "include_files": checked_value(include_files),
            "include_logs": checked_value(include_logs),
        });
        spawn_local(async move {
            match api_json::<Value, _>("/api/admin/backups", &token_value, "POST", &body).await {
                Ok(value) => {
                    detail.set(Some(json!({"task": value, "files": []})));
                    notify("备份任务已创建".to_string());
                    load();
                }
                Err(err) => notify(err),
            }
        });
    };
    let show_detail = move |id: String| {
        let token_value = token.get();
        spawn_local(async move {
            match api_get::<Value>(&format!("/api/admin/backups/{id}"), &token_value).await {
                Ok(value) => detail.set(Some(value)),
                Err(err) => notify(err),
            }
        });
    };
    let restore = move |id: String| {
        if !confirm("确认从这个备份恢复系统配置？") {
            return;
        }
        let token_value = token.get();
        let restore_options_json = json!({
            "settings": checked_value(restore_site_settings) || checked_value(restore_theme_settings) || checked_value(restore_smtp_settings) || checked_value(restore_storage_providers),
            "site_settings": checked_value(restore_site_settings),
            "theme_settings": checked_value(restore_theme_settings),
            "smtp_settings": checked_value(restore_smtp_settings),
            "storage_providers": checked_value(restore_storage_providers),
            "metadata": checked_value(restore_metadata),
            "audit": checked_value(restore_audit),
            "logs": checked_value(restore_logs),
        });
        spawn_local(async move {
            match api_json::<Value, _>(
                "/api/admin/restores",
                &token_value,
                "POST",
                &json!({"backup_id": id, "restore_options_json": restore_options_json}),
            )
            .await
            {
                Ok(_) => notify("恢复任务已创建".to_string()),
                Err(err) => notify(err),
            }
        });
    };
    view! {
        <div class="admin-stack">
            <form id="backupCreateForm" class="backup-tools admin-section" on:submit=create>
                <h3>"创建备份"</h3>
                <input node_ref=target_storage placeholder="目标存储编号，可选"/>
                <div id="backupOptions" class="backup-options">
                    <label><input node_ref=include_files type="checkbox"/>"包含图片文件"</label>
                    <label><input node_ref=include_logs type="checkbox" checked/>"包含日志"</label>
                </div>
                <h3>"恢复选项"</h3>
                <div class="backup-options">
                    <label><input node_ref=restore_site_settings type="checkbox" checked/>"站点设置"</label>
                    <label><input node_ref=restore_theme_settings type="checkbox" checked/>"外观设置"</label>
                    <label><input node_ref=restore_smtp_settings type="checkbox"/>"SMTP"</label>
                    <label><input node_ref=restore_storage_providers type="checkbox"/>"存储配置"</label>
                    <label><input node_ref=restore_metadata type="checkbox"/>"图片元数据"</label>
                    <label><input node_ref=restore_audit type="checkbox"/>"审核数据"</label>
                    <label><input node_ref=restore_logs type="checkbox"/>"日志"</label>
                </div>
                <button type="submit">"创建备份"</button>
            </form>
            <div class="admin-toolbar">
                <select node_ref=list_status>
                    <option value="">"全部状态"</option>
                    <option value="pending">"等待中"</option>
                    <option value="running">"运行中"</option>
                    <option value="completed">"已完成"</option>
                    <option value="failed">"失败"</option>
                </select>
                <button type="button" on:click=move |_| {
                    page.set(1);
                    load();
                }>"筛选备份"</button>
                <button type="button" on:click=move |_| load()>"刷新备份"</button>
                <span class="muted">{move || page_summary(page.get(), page_size.get(), total.get(), loading.get())}</span>
            </div>
            <div class="admin-table-wrap">
                <table class="admin-table">
                    <thead><tr><th>"备份"</th><th>"类型"</th><th>"状态"</th><th>"大小"</th><th>"创建时间"</th><th>"操作"</th></tr></thead>
                    <tbody>
                        {move || backups.get().into_iter().map(|item| {
                            let id = json_string(&item, "id");
                            let detail_id = id.clone();
                            let restore_id = id.clone();
                            let download_href = format!("/api/admin/backups/{id}/download");
                            view! {
                                <tr>
                                    <td data-label="备份"><strong>{id.clone()}</strong></td>
                                    <td data-label="类型">{json_string(&item, "backup_type")}</td>
                                    <td data-label="状态"><StatusBadge value=json_string(&item, "status")/></td>
                                    <td data-label="大小">{format_bytes(json_i64(&item, "backup_size"))}</td>
                                    <td data-label="创建时间">{time_label(&json_string(&item, "created_at"))}</td>
                                    <td data-label="操作">
                                        <div class="row-actions">
                                            <button type="button" on:click=move |_| show_detail(detail_id.clone())>"详情"</button>
                                            <a class="button-link" href=download_href target="_blank">"下载"</a>
                                            <button type="button" on:click=move |_| restore(restore_id.clone())>"恢复"</button>
                                        </div>
                                    </td>
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            </div>
            <Show when=move || backups.get().is_empty()>
                <div class="empty">"暂无备份任务，创建后可下载和恢复"</div>
            </Show>
            <AdminPager page=page page_size=page_size total=total loading=loading load=load/>
            {move || detail.get().map(|value| {
                let task = value.get("task").cloned().unwrap_or_else(|| json!({}));
                let files = value.get("files").and_then(Value::as_array).cloned().unwrap_or_default();
                view! {
                    <section class="admin-detail">
                        <header class="section-head">
                            <h3>"备份详情"</h3>
                            <button class="secondary" type="button" on:click=move |_| detail.set(None)>"收起"</button>
                        </header>
                        <div class="detail-fields">
                            <span><small>"备份编号"</small><strong>{json_string(&task, "id")}</strong></span>
                            <span><small>"状态"</small><strong>{status_label(&json_string(&task, "status"))}</strong></span>
                            <span><small>"类型"</small><strong>{json_string(&task, "backup_type")}</strong></span>
                            <span><small>"大小"</small><strong>{format_bytes(json_i64(&task, "backup_size"))}</strong></span>
                        </div>
                        <div class="admin-table-wrap">
                            <table class="admin-table">
                                <thead><tr><th>"文件"</th><th>"类型"</th><th>"大小"</th><th>"状态"</th></tr></thead>
                                <tbody>
                                    {files.into_iter().map(|file| view! {
                                        <tr>
                                            <td data-label="文件">{json_string(&file, "file_name")}</td>
                                            <td data-label="类型">{json_string(&file, "file_type")}</td>
                                            <td data-label="大小">{format_bytes(json_i64(&file, "file_size"))}</td>
                                            <td data-label="状态"><StatusBadge value=json_string(&file, "status")/></td>
                                        </tr>
                                    }).collect_view()}
                                </tbody>
                            </table>
                        </div>
                    </section>
                }
            }).into_any()}
        </div>
    }
}

async fn upload_queued_files(
    token: &str,
    files: Vec<QueuedUpload>,
    tags: String,
    progress: RwSignal<Vec<UploadProgress>>,
) -> Result<Value, String> {
    let total = files.len();
    let mut succeeded = 0usize;
    let mut items = Vec::with_capacity(total);
    for (index, file) in files.into_iter().enumerate() {
        progress.update(|rows| {
            if let Some(row) = rows.get_mut(index) {
                row.percent = 20.0;
                row.message = "上传中".to_string();
            }
        });
        match upload_single_file(token, &file.file, &tags).await {
            Ok(response) => {
                succeeded += 1;
                progress.update(|rows| {
                    if let Some(row) = rows.get_mut(index) {
                        row.percent = 100.0;
                        row.finished = true;
                        row.success = true;
                        row.message = "完成".to_string();
                    }
                });
                items.push(json!({
                    "file_name": file.name,
                    "success": true,
                    "response": response,
                    "error": null
                }));
            }
            Err(error) => {
                progress.update(|rows| {
                    if let Some(row) = rows.get_mut(index) {
                        row.percent = 100.0;
                        row.finished = true;
                        row.success = false;
                        row.message = error.clone();
                    }
                });
                items.push(json!({
                    "file_name": file.name,
                    "success": false,
                    "response": null,
                    "error": {"message": error}
                }));
            }
        }
    }
    Ok(json!({
        "total": total,
        "succeeded": succeeded,
        "failed": total.saturating_sub(succeeded),
        "items": items
    }))
}

fn validate_queued_files(files: &[QueuedUpload], max_size: Option<i64>) -> Result<(), String> {
    for file in files {
        if file.size == 0 {
            return Err(format!("{} 是空文件", file.name));
        }
        if !file.is_image() {
            return Err(format!("{} 不是支持的图片文件", file.name));
        }
        if let Some(max_size) = max_size
            && max_size > 0
            && file.size > max_size as u64
        {
            return Err(format!("{} 超过单文件大小限制", file.name));
        }
    }
    Ok(())
}

async fn ensure_guest_upload_allowed(files: &[QueuedUpload]) -> Result<(), String> {
    let site = api_get::<Value>("/api/settings/site", "").await?;
    if !json_bool(&site, "guest_upload_enabled", false) {
        return Err("访客上传已关闭，请登录后上传".to_string());
    }
    let upload = api_get::<Value>("/api/settings/upload", "")
        .await
        .unwrap_or_else(|_| json!({}));
    let allowed = upload
        .get("allowed_mime_types")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !allowed.is_empty()
        && let Some(file) = files.iter().find(|file| {
            let mime = file.mime_type();
            mime.is_empty() || !allowed.iter().any(|allowed| allowed == &mime)
        })
    {
        return Err(format!("{} 的文件类型不允许", file.name));
    }
    Ok(())
}

fn mark_uploads_failed(
    progress: RwSignal<Vec<UploadProgress>>,
    files: &[QueuedUpload],
    message: &str,
) {
    progress.set(
        files
            .iter()
            .map(|file| UploadProgress::failed(&file.name, message))
            .collect(),
    );
}

async fn upload_single_file(
    token: &str,
    file: &File,
    tags: &str,
) -> Result<Value, String> {
    let form = FormData::new().map_err(|_| "无法创建上传表单".to_string())?;
    form.append_with_blob_and_filename("file", file, &file.name())
        .map_err(|_| "无法加入图片文件".to_string())?;
    form.append_with_str("tags", tags)
        .map_err(|_| "无法加入标签".to_string())?;
    api_form(
        if token.is_empty() {
            "/api/guest/images/upload"
        } else {
            "/api/images/upload"
        },
        token,
        form,
    )
    .await
}

async fn upload_avatar(token: &str, files: FileList) -> Result<Value, String> {
    let form = FormData::new().map_err(|_| "无法创建头像表单".to_string())?;
    if let Some(file) = files.get(0) {
        form.append_with_blob_and_filename("file", &file, &file.name())
            .map_err(|_| "无法加入头像文件".to_string())?;
    }
    api_form("/api/user/avatar", token, form).await
}

#[derive(Clone, Debug)]
struct UiError {
    message: String,
}

impl UiError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<UiError> for String {
    fn from(value: UiError) -> Self {
        value.message
    }
}

async fn api_get<T: DeserializeOwned>(path: &str, token: &str) -> Result<T, String> {
    typed_api_get(path, token).await.map_err(Into::into)
}

async fn typed_api_get<T: DeserializeOwned>(path: &str, token: &str) -> Result<T, UiError> {
    let mut request = Request::get(path);
    if !token.is_empty() {
        request = request.header("authorization", &format!("Bearer {token}"));
    }
    parse_response(request.send().await.map_err(request_error)?).await
}

async fn api_empty(path: &str, token: &str, method: &str) -> Result<Value, String> {
    api_raw(path, token, method, "{}".to_string()).await
}

async fn api_json<T: DeserializeOwned, B: Serialize>(
    path: &str,
    token: &str,
    method: &str,
    body: &B,
) -> Result<T, String> {
    let mut request = request_with_method(path, method).header("content-type", "application/json");
    if !token.is_empty() {
        request = request.header("authorization", &format!("Bearer {token}"));
    }
    let request = request
        .body(serde_json::to_string(body).map_err(|err| err.to_string())?)
        .map_err(|err| err.to_string())?;
    parse_response(request.send().await.map_err(request_error)?)
        .await
        .map_err(Into::into)
}

async fn api_raw(path: &str, token: &str, method: &str, body: String) -> Result<Value, String> {
    let mut request = request_with_method(path, method).header("content-type", "application/json");
    if !token.is_empty() {
        request = request.header("authorization", &format!("Bearer {token}"));
    }
    let request = request.body(body).map_err(|err| err.to_string())?;
    parse_response(request.send().await.map_err(request_error)?)
        .await
        .map_err(Into::into)
}

async fn api_form(path: &str, token: &str, form: FormData) -> Result<Value, String> {
    let mut request = Request::post(path);
    if !token.is_empty() {
        request = request.header("authorization", &format!("Bearer {token}"));
    }
    let request = request.body(form).map_err(|err| err.to_string())?;
    parse_response(request.send().await.map_err(request_error)?)
        .await
        .map_err(Into::into)
}

fn request_error(err: gloo_net::Error) -> String {
    let message = err.to_string();
    if message.contains("NetworkError")
        || message.contains("Failed to fetch")
        || message.contains("error sending request")
    {
        "服务器连接失败，请检查服务是否已启动".to_string()
    } else {
        message
    }
}

impl From<String> for UiError {
    fn from(value: String) -> Self {
        UiError::new(value)
    }
}

fn request_with_method(path: &str, method: &str) -> gloo_net::http::RequestBuilder {
    match method {
        "DELETE" => Request::delete(path),
        "PUT" => Request::put(path),
        "POST" => Request::post(path),
        _ => Request::get(path),
    }
}

async fn parse_response<T: DeserializeOwned>(
    response: gloo_net::http::Response,
) -> Result<T, UiError> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| UiError::new(err.to_string()))?;
    let value: Value =
        serde_json::from_str(&text).map_err(|_| UiError::new(response_status_label(status)))?;
    if status >= 400 || value.get("success").and_then(Value::as_bool) == Some(false) {
        let code = value
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let message = value
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("请求失败");
        return Err(UiError::new(error_label(code, message)));
    }
    let data = value.get("data").cloned().unwrap_or(value);
    serde_json::from_value(data).map_err(|err| UiError::new(err.to_string()))
}

fn response_status_label(status: u16) -> String {
    match status {
        401 => "请先登录".to_string(),
        403 => "没有权限或后台已关闭该能力".to_string(),
        404 => "请求的资源不存在".to_string(),
        413 => "文件超过大小限制".to_string(),
        429 => "请求过于频繁，请稍后再试".to_string(),
        500..=599 => "系统繁忙，请稍后重试".to_string(),
        _ => "请求失败，请稍后重试".to_string(),
    }
}

enum OAuthHashResult {
    Token(String),
    Error(String),
}

fn parse_oauth_hash(hash: &str) -> Option<OAuthHashResult> {
    let payload = hash.strip_prefix("oauth:")?;
    let mut token = None::<String>;
    let mut error = None::<String>;
    for pair in payload.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "token" => {
                token = urlencoding::decode(value)
                    .ok()
                    .map(|value| value.into_owned())
            }
            "error" => {
                error = urlencoding::decode(value)
                    .ok()
                    .map(|value| value.into_owned())
            }
            _ => {}
        }
    }
    token
        .filter(|value| !value.is_empty())
        .map(OAuthHashResult::Token)
        .or_else(|| {
            error
                .filter(|value| !value.is_empty())
                .map(OAuthHashResult::Error)
        })
}

fn current_hash() -> Option<String> {
    web_sys::window()?
        .location()
        .hash()
        .ok()
        .map(|hash| hash.trim_start_matches('#').to_string())
        .filter(|hash| !hash.is_empty())
}

fn storage_get(key: &str) -> Option<String> {
    web_sys::window()
        .and_then(|window| window.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item(key).ok().flatten())
        .map(|value| serde_json::from_str::<String>(&value).ok().unwrap_or(value))
        .or_else(|| LocalStorage::get::<String>(key).ok())
}

fn storage_set(key: &str, value: &str) {
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = storage.set_item(key, value);
    } else {
        let _ = LocalStorage::set(key, value.to_string());
    }
}

fn storage_delete(key: &str) {
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = storage.remove_item(key);
    } else {
        LocalStorage::delete(key);
    }
}

fn input_value(node: NodeRef<Input>) -> Option<String> {
    node.get().map(|input| input.value().trim().to_string())
}

fn input_i32(node: NodeRef<Input>) -> Option<i32> {
    input_value(node).and_then(|value| value.parse::<i32>().ok())
}

fn input_i64(node: NodeRef<Input>) -> Option<i64> {
    input_value(node).and_then(|value| value.parse::<i64>().ok())
}

fn input_f64(node: NodeRef<Input>) -> Option<f64> {
    input_value(node).and_then(|value| value.parse::<f64>().ok())
}

fn checked_value(node: NodeRef<Input>) -> bool {
    node.get().map(|input| input.checked()).unwrap_or(false)
}

fn title_value(node: NodeRef<Input>, value: &str) {
    if let Some(input) = node.get() {
        input.set_value(value);
    }
}

fn title_sensitive_value(node: NodeRef<Input>, value: &str) {
    if value == REDACTED_VALUE {
        title_value(node, "");
    } else {
        title_value(node, value);
    }
}

fn textarea_value(node: NodeRef<Textarea>, value: &str) {
    if let Some(input) = node.get() {
        input.set_value(value);
    }
}

fn textarea_sensitive_value(node: NodeRef<Textarea>, value: &str) {
    if value == REDACTED_VALUE {
        textarea_value(node, "");
    } else {
        textarea_value(node, value);
    }
}

fn textarea_current_value(node: NodeRef<Textarea>) -> Option<String> {
    node.get().map(|input| input.value())
}

fn select_value(node: NodeRef<Select>) -> Option<String> {
    node.get().map(|input| input.value())
}

fn select_set_value(node: NodeRef<Select>, value: &str) {
    if let Some(input) = node.get() {
        input.set_value(value);
    }
}

fn checked_set_value(node: NodeRef<Input>, value: bool) {
    if let Some(input) = node.get() {
        input.set_checked(value);
    }
}

fn signal_bool(signal: RwSignal<bool>) -> bool {
    signal.try_get().unwrap_or(false)
}

fn signal_i64(signal: RwSignal<i64>) -> i64 {
    signal.try_get().unwrap_or_default()
}

fn signal_string_eq(signal: RwSignal<String>, expected: &str) -> bool {
    signal
        .try_get()
        .is_some_and(|value| value.as_str() == expected)
}

fn signal_string_matches(signal: RwSignal<String>, expected: &[&str]) -> bool {
    signal
        .try_get()
        .is_some_and(|value| expected.iter().any(|item| value == *item))
}

fn signal_option_is_some(signal: RwSignal<Option<String>>) -> bool {
    signal.try_get().flatten().is_some()
}

fn signal_option_eq(signal: RwSignal<Option<String>>, expected: &str) -> bool {
    signal
        .try_get()
        .flatten()
        .is_some_and(|value| value == expected)
}

fn event_target_value(event: &Event) -> String {
    event
        .target()
        .and_then(|target| target.dyn_into::<HtmlSelectElement>().ok())
        .map(|input| input.value())
        .or_else(|| {
            event
                .target()
                .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
                .map(|input| input.value())
        })
        .or_else(|| {
            event
                .target()
                .and_then(|target| target.dyn_into::<HtmlTextAreaElement>().ok())
                .map(|input| input.value())
        })
        .unwrap_or_default()
}

fn checked_event(event: &Event) -> bool {
    event
        .target()
        .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
        .map(|input| input.checked())
        .unwrap_or(false)
}

fn copy_to_clipboard(value: &str) -> bool {
    if let Some(window) = web_sys::window() {
        let clipboard = window.navigator().clipboard();
        if !clipboard.is_undefined() {
            let promise = clipboard.write_text(value);
            let value = value.to_string();
            spawn_local(async move {
                if wasm_bindgen_futures::JsFuture::from(promise).await.is_err() {
                    fallback_copy_to_clipboard(&value);
                }
            });
            return true;
        }
    }
    fallback_copy_to_clipboard(value)
}

fn fallback_copy_to_clipboard(value: &str) -> bool {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return false;
    };
    let Ok(textarea) = document.create_element("textarea") else {
        return false;
    };
    let textarea: web_sys::HtmlTextAreaElement = textarea.unchecked_into();
    textarea.set_value(value);
    if let Some(style) = textarea.dyn_ref::<web_sys::Element>() {
        style.set_attribute("style", "position:fixed;left:-9999px;top:-9999px;").ok();
    }
    textarea.set_attribute("readonly", "").ok();
    if document.body().and_then(|b| b.append_child(&textarea).ok()).is_none() {
        return false;
    }
    textarea.focus().ok();
    textarea.set_selection_start(Some(0)).ok();
    textarea.set_selection_end(Some(value.len() as u32)).ok();
    let result = document
        .dyn_into::<web_sys::HtmlDocument>()
        .ok()
        .and_then(|d| d.exec_command("copy").ok())
        .unwrap_or(false);
    let _ = textarea.remove();
    result
}

fn confirm(message: &str) -> bool {
    web_sys::window()
        .and_then(|window| window.confirm_with_message(message).ok())
        .unwrap_or(false)
}

fn display_url(value: &str) -> String {
    let value = normalize_url_slashes(value.trim());
    if let Some(path) = local_public_resource_path(&value) {
        path
    } else {
        value
    }
}

fn public_url(value: &str) -> String {
    let value = display_url(value);
    if value.starts_with('/') {
        format!("{}{}", current_origin(), value)
    } else {
        value
    }
}

fn rewrite_embedded_public_urls(value: &str) -> String {
    let mut rewritten = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(offset) = next_url_offset(&value[cursor..]) {
        let start = cursor + offset;
        let end = embedded_url_end(value, start);
        rewritten.push_str(&value[cursor..start]);
        rewritten.push_str(&public_url(&value[start..end]));
        cursor = end;
    }
    rewritten.push_str(&value[cursor..]);
    rewrite_embedded_public_paths(&rewritten)
}

fn next_url_offset(value: &str) -> Option<usize> {
    match (value.find("http://"), value.find("https://")) {
        (Some(http), Some(https)) => Some(http.min(https)),
        (Some(http), None) => Some(http),
        (None, Some(https)) => Some(https),
        (None, None) => None,
    }
}

fn embedded_url_end(value: &str, start: usize) -> usize {
    value[start..]
        .char_indices()
        .find(|(_, ch)| matches!(ch, '"' | '\'' | ')' | '<' | '>' | ' ' | '\n' | '\r' | '\t'))
        .map(|(index, _)| start + index)
        .unwrap_or(value.len())
}

fn rewrite_embedded_public_paths(value: &str) -> String {
    let mut rewritten = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(offset) = next_public_path_offset(&value[cursor..]) {
        let start = cursor + offset;
        let end = embedded_url_end(value, start);
        rewritten.push_str(&value[cursor..start]);
        if should_prefix_public_path(value, start) {
            rewritten.push_str(&current_origin());
        }
        rewritten.push_str(&value[start..end]);
        cursor = end;
    }
    rewritten.push_str(&value[cursor..]);
    rewritten
}

fn next_public_path_offset(value: &str) -> Option<usize> {
    match (value.find("/files/"), value.find("/api/storage/proxy/")) {
        (Some(files), Some(proxy)) => Some(files.min(proxy)),
        (Some(files), None) => Some(files),
        (None, Some(proxy)) => Some(proxy),
        (None, None) => None,
    }
}

fn should_prefix_public_path(value: &str, start: usize) -> bool {
    let Some(prev) = value[..start].chars().next_back() else {
        return true;
    };
    !prev.is_ascii_alphanumeric() && !matches!(prev, '.' | ':' | '/' | '-' | '_')
}

fn current_origin() -> String {
    web_sys::window()
        .and_then(|window| window.location().origin().ok())
        .filter(|origin| !origin.is_empty() && origin != "null")
        .unwrap_or_default()
}

fn normalize_url_slashes(value: &str) -> String {
    let Some((scheme, rest)) = value.split_once("://") else {
        return value.replace("//", "/");
    };
    format!("{scheme}://{}", rest.replace("//", "/"))
}

fn local_public_resource_path(value: &str) -> Option<String> {
    let (_, rest) = value.split_once("://")?;
    let (_, path) = rest.split_once('/')?;
    let path = format!("/{path}");
    if is_local_public_host(rest.split('/').next().unwrap_or_default())
        && (path.starts_with("/files/") || path.starts_with("/api/storage/proxy/"))
    {
        Some(path)
    } else {
        None
    }
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

fn format_bytes(bytes: i64) -> String {
    let mut value = bytes.max(0) as f64;
    let units = ["字节", "千字节", "兆字节", "吉字节", "太字节"];
    let mut index = 0usize;
    while value >= 1024.0 && index < units.len() - 1 {
        value /= 1024.0;
        index += 1;
    }
    if index == 0 {
        format!("{} {}", value as i64, units[index])
    } else {
        format!("{value:.1} {}", units[index])
    }
}

fn image_aspect_ratio(width: i32, height: i32) -> String {
    if width > 0 && height > 0 {
        format!("aspect-ratio: {width} / {height};")
    } else {
        "aspect-ratio: 4 / 3;".to_string()
    }
}

/// 收集剪贴板事件里携带的图片文件。
/// 优先取 DataTransferItemList 中类型为图片的项，其次回退到 DataTransfer.files。
fn collect_paste_files(data: &DataTransfer) -> Option<Vec<File>> {
    let items = data.items();
    let mut collected: Vec<File> = Vec::new();
    for index in 0..items.length() {
        if let Some(item) = items.get(index) {
            if item.kind() == "file" {
                if let Ok(Some(file)) = item.get_as_file() {
                    let mime = file.type_();
                    if mime.starts_with("image/") {
                        collected.push(file);
                    }
                }
            }
        }
    }
    if !collected.is_empty() {
        return Some(collected);
    }
    let mut fallback: Vec<File> = Vec::new();
    if let Some(files) = data.files() {
        for index in 0..files.length() {
            if let Some(file) = files.get(index)
                && file.type_().starts_with("image/")
            {
                fallback.push(file);
            }
        }
    }
    if fallback.is_empty() {
        None
    } else {
        Some(fallback)
    }
}

fn html_escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn json_string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn json_i64(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or_default()
}

fn json_bool(value: &Value, key: &str, fallback: bool) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(fallback)
}

fn parse_comma_values(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn datetime_local_to_rfc3339(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else if value.ends_with('Z') || value.contains('+') {
        Some(value.to_string())
    } else if value.matches(':').count() >= 2 {
        Some(format!("{value}Z"))
    } else {
        Some(format!("{value}:00Z"))
    }
}

fn selected_token_scopes(
    upload: bool,
    read: bool,
    delete: bool,
    random: bool,
) -> Vec<&'static str> {
    let mut scopes = Vec::new();
    if upload {
        scopes.push("upload");
    }
    if read {
        scopes.push("read");
    }
    if delete {
        scopes.push("delete");
    }
    if random {
        scopes.push("random");
    }
    scopes
}

fn token_scope_label(value: &Value) -> String {
    let labels = value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(scope_label)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if labels.is_empty() {
        "未设置".to_string()
    } else {
        labels.join("，")
    }
}

fn token_ip_label(value: &Value) -> String {
    let items = value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|item| !item.trim().is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if items.is_empty() {
        "不限".to_string()
    } else {
        items.join("，")
    }
}

fn token_expiration_label(value: &Value) -> String {
    value
        .as_str()
        .map(|value| value.replace('T', " ").replace('Z', ""))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "长期有效".to_string())
}

fn scope_label(value: &str) -> &'static str {
    match value {
        "upload" => "上传",
        "read" => "读取",
        "delete" => "删除",
        "random" => "随机图",
        "ai" => "智能审核",
        _ => "未知",
    }
}

struct AdminImageFilterValues {
    status: String,
    orientation: String,
    guest: String,
    tag: Option<String>,
    user_id: Option<String>,
    storage_id: Option<String>,
    min_width: Option<String>,
    min_height: Option<String>,
    page: i64,
    page_size: i64,
}

fn admin_image_query_path(filters: AdminImageFilterValues) -> String {
    let mut params = vec![
        format!("page={}", filters.page.max(1)),
        format!("page_size={}", filters.page_size.clamp(1, 100)),
    ];
    push_query_param(&mut params, "status", &filters.status);
    push_query_param(&mut params, "orientation", &filters.orientation);
    push_query_param(&mut params, "is_guest_upload", &filters.guest);
    if let Some(value) = filters.tag {
        push_query_param(&mut params, "tag", &value);
    }
    if let Some(value) = filters.user_id {
        push_query_param(&mut params, "user_id", &value);
    }
    if let Some(value) = filters.storage_id {
        push_query_param(&mut params, "storage_provider_id", &value);
    }
    if let Some(value) = filters.min_width {
        push_query_param(&mut params, "min_width", &value);
    }
    if let Some(value) = filters.min_height {
        push_query_param(&mut params, "min_height", &value);
    }
    format!("/api/admin/images?{}", params.join("&"))
}

fn push_query_param(params: &mut Vec<String>, key: &str, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        params.push(format!("{key}={}", urlencoding::encode(value)));
    }
}

fn localize_admin_image_detail(mut value: Value) -> Value {
    if let Some(image) = value.get_mut("image").and_then(Value::as_object_mut) {
        if let Some(status) = image.get("status").and_then(Value::as_str) {
            image.insert("status_label".to_string(), json!(status_label(status)));
        }
        if let Some(orientation) = image.get("orientation").and_then(Value::as_str) {
            image.insert(
                "orientation_label".to_string(),
                json!(orientation_label(orientation)),
            );
        }
    }
    if let Some(objects) = value
        .get_mut("storage_objects")
        .and_then(Value::as_array_mut)
    {
        for object in objects {
            if let Some(map) = object.as_object_mut() {
                if let Some(provider_type) = map.get("provider_type").and_then(Value::as_str) {
                    map.insert(
                        "provider_type_label".to_string(),
                        json!(storage_type_label(provider_type)),
                    );
                }
                if let Some(object_type) = map.get("object_type").and_then(Value::as_str) {
                    map.insert(
                        "object_type_label".to_string(),
                        json!(object_type_label(object_type)),
                    );
                }
                if let Some(status) = map.get("status").and_then(Value::as_str) {
                    map.insert("status_label".to_string(), json!(status_label(status)));
                }
            }
        }
    }
    value
}

fn admin_title(value: &str) -> &'static str {
    match value {
        "dashboard" => "仪表盘",
        "images" => "图片与标签",
        "users" => "用户与配额",
        "audit" => "审核管理",
        "storage" => "存储与迁移",
        "settings" => "系统设置",
        _ => "管理员后台",
    }
}

fn admin_subtitle(value: &str) -> &'static str {
    match value {
        "dashboard" => "聚合统计、近期上传、审核动态和存储健康。",
        "images" => "图片运营、访客上传筛选、标签治理和审核操作集中处理。",
        "users" => "用户、用户组、配额规则和单用户覆盖集中管理。",
        "audit" => "审核队列、审核日志、AI 服务和关键词策略。",
        "storage" => "存储提供方、上传路由、健康检查和图片迁移。",
        "settings" => "站点、上传、随机图、外观、邮件、验证码、日志和备份恢复。",
        _ => "",
    }
}

#[component]
fn StatusBadge(value: String) -> impl IntoView {
    let class = format!("status-badge {}", status_class(&value));
    view! { <span class=class>{status_label(&value)}</span> }
}

#[component]
fn StorageStatusBadge(value: String, error: String) -> impl IntoView {
    let class = format!("status-badge {}", status_class(&value));
    let label = storage_status_label(&value);
    let summary = error_summary(&error, 68);
    let title = error.clone();
    let summary_view = if error.is_empty() {
        ().into_any()
    } else {
        view! { <small>{summary}</small> }.into_any()
    };
    view! {
        <span class="storage-status" title=title>
            <span class=class>{label}</span>
            {summary_view}
        </span>
    }
}

#[component]
fn AdminPager(
    page: RwSignal<i64>,
    page_size: RwSignal<i64>,
    total: RwSignal<i64>,
    loading: RwSignal<bool>,
    load: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let total_pages = move || {
        let page_size = signal_i64(page_size).max(1);
        ((signal_i64(total) + page_size - 1) / page_size).max(1)
    };
    Effect::new(move |_| {
        let pages = total_pages();
        if page.try_get().is_some_and(|current| current > pages) && page.try_set(pages).is_none() {
            load();
        }
    });
    let prev_disabled = move || signal_i64(page) <= 1 || signal_bool(loading);
    let next_disabled = move || signal_i64(page) >= total_pages() || signal_bool(loading);
    view! {
        <div class="admin-pager">
            <button
                type="button"
                disabled=prev_disabled
                on:click=move |_| {
                    if page.try_update(|value| *value = (*value - 1).max(1)).is_some() {
                        load();
                    }
                }
            >"上一页"</button>
            <span>{move || format!("第 {} / {} 页 · 共 {} 条", signal_i64(page), total_pages(), signal_i64(total))}</span>
            <select on:change=move |event| {
                if let Ok(value) = event_target_value(&event).parse::<i64>()
                    && page_size.try_set(value).is_none()
                    && page.try_set(1).is_none()
                {
                    load();
                }
            }>
                <option value="20" selected=move || page_size.try_get() == Some(20)>"20 / 页"</option>
                <option value="40" selected=move || page_size.try_get() == Some(40)>"40 / 页"</option>
                <option value="80" selected=move || page_size.try_get() == Some(80)>"80 / 页"</option>
            </select>
            <button
                type="button"
                disabled=next_disabled
                on:click=move |_| {
                    if page.try_update(|value| *value += 1).is_some() {
                        load();
                    }
                }
            >"下一页"</button>
        </div>
    }
}

fn page_summary(page: i64, page_size: i64, total: i64, loading: bool) -> String {
    let total_pages = ((total + page_size - 1) / page_size).max(1);
    let suffix = if loading { " · 加载中..." } else { "" };
    format!("第 {page}/{total_pages} 页 · 共 {total} 条{suffix}")
}

fn admin_page_path(base: &str, page: i64, page_size: i64, filters: &[(&str, String)]) -> String {
    let mut params = vec![format!("page={page}"), format!("page_size={page_size}")];
    for (key, value) in filters {
        push_query_param(&mut params, key, value);
    }
    format!("{base}?{}", params.join("&"))
}

fn status_class(value: &str) -> &'static str {
    match value {
        "active" | "passed" | "completed" | "info" | "healthy" => "good",
        "pending" | "pending_review" | "manual_required" | "running" | "warn" => "warn",
        "failed" | "rejected" | "blocked" | "deleted" | "error" => "bad",
        _ => "neutral",
    }
}

fn role_label(value: &str) -> String {
    match value {
        "guest" | "guest_account" => "访客".to_string(),
        "normal" | "user" => "普通用户".to_string(),
        "trusted" => "可信用户".to_string(),
        "supporter" => "公益支持者".to_string(),
        "admin" => "管理员".to_string(),
        "super_admin" => "超级管理员".to_string(),
        other => other.to_string(),
    }
}

fn visibility_label(value: &str) -> String {
    match value {
        "public" => "公开".to_string(),
        "private" => "私有".to_string(),
        "unlisted" => "隐藏".to_string(),
        other => other.to_string(),
    }
}

fn bool_label(value: bool, yes: &str, no: &str) -> String {
    if value {
        yes.to_string()
    } else {
        no.to_string()
    }
}

fn display_or_dash(value: &str) -> String {
    if value.trim().is_empty() {
        "-".to_string()
    } else {
        value.to_string()
    }
}

fn short_text(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn error_summary(value: &str, limit: usize) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut text = value.chars().take(limit).collect::<String>();
    if value.chars().count() > limit {
        text.push_str("...");
    }
    text
}

fn time_label(value: &str) -> String {
    value
        .replace('T', " ")
        .replace('Z', "")
        .split('.')
        .next()
        .unwrap_or(value)
        .to_string()
}

fn json_array(value: &Value, key: &str) -> Vec<Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn dashboard_users(data: &Value) -> i64 {
    json_i64(data, "users_total")
}

fn dashboard_today_users(data: &Value) -> i64 {
    json_i64(data, "users_today")
}

fn dashboard_images_total(data: &Value) -> i64 {
    json_i64(data, "images_total")
}

fn dashboard_today_images(data: &Value) -> i64 {
    json_i64(data, "images_today")
}

fn dashboard_storage_bytes(data: &Value) -> i64 {
    json_i64(data, "storage_bytes")
}

fn dashboard_pending_audit(data: &Value) -> i64 {
    json_i64(data, "pending_audit")
}

fn dashboard_storage_health(data: &Value) -> String {
    let storage = json_array(data, "storage");
    let healthy = storage
        .iter()
        .filter(|item| {
            item.get("healthy")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    format!("{healthy}/{}", storage.len())
}

fn status_label(value: &str) -> String {
    match value {
        "active" => "正常".to_string(),
        "pending_review" => "待审核".to_string(),
        "rejected" => "已拒绝".to_string(),
        "trashed" => "回收站".to_string(),
        "deleted" => "已删除".to_string(),
        "blocked" => "已隔离".to_string(),
        "passed" => "已通过".to_string(),
        "failed" => "失败".to_string(),
        "running" => "运行中".to_string(),
        "pending" => "等待中".to_string(),
        "manual_required" => "待人工审核".to_string(),
        "completed" => "已完成".to_string(),
        "cancelled" => "已取消".to_string(),
        "disabled" => "已停用".to_string(),
        other => other.to_string(),
    }
}

fn storage_status_label(value: &str) -> String {
    match value {
        "healthy" => "健康".to_string(),
        "error" => "异常".to_string(),
        "pending" => "待检查".to_string(),
        "disabled" => "已停用".to_string(),
        other => status_label(other),
    }
}

fn storage_type_label(value: &str) -> String {
    match value {
        "local" => "本地".to_string(),
        "cloudflare_r2" => "R2".to_string(),
        "onedrive" => "OneDrive".to_string(),
        "oracle_s3" => "S3".to_string(),
        "oracle_oci_native" => "OCI".to_string(),
        "s3_compatible" => "S3 兼容".to_string(),
        other => other.to_string(),
    }
}

fn storage_scope_label(scope_type: &str, scope_value: &str) -> String {
    match scope_type {
        "global" => "全局".to_string(),
        "role" => format!("角色 · {}", role_label(scope_value)),
        "group" => format!("用户组 · {}", role_label(scope_value)),
        "user" => format!("用户 · {}", short_text(scope_value, 12)),
        other => other.to_string(),
    }
}

fn storage_delete_mode_message(mode: &str) -> String {
    match mode {
        "deleted" => "存储已删除".to_string(),
        "disabled" => "存储仍有历史引用，已停用并从列表隐藏".to_string(),
        _ => "存储删除操作已完成".to_string(),
    }
}

fn storage_action_message(action: &str, value: &Value) -> String {
    if value.get("ok").and_then(Value::as_bool) == Some(false)
        || value.get("healthy").and_then(Value::as_bool) == Some(false)
    {
        let error = json_string(value, "error");
        let stage = storage_test_stage_label(&json_string(value, "stage"));
        return if error.is_empty() {
            "存储测试失败，请检查配置".to_string()
        } else if stage.is_empty() || error.starts_with(stage.as_str()) {
            format!("存储测试失败：{error}")
        } else {
            format!("存储测试失败：{stage}失败：{error}")
        };
    }
    match action {
        "test-connection" => "存储连接正常".to_string(),
        "test-upload" => {
            let read_back = json_bool(value, "read_back_ok", false);
            let size = format_bytes(json_i64(value, "size"));
            if read_back {
                format!("上传测试通过，已读回 {size}")
            } else {
                "上传成功，但读回校验未通过".to_string()
            }
        }
        "test-delete" => "删除测试通过".to_string(),
        "set-default" => "默认存储已更新".to_string(),
        _ => "存储操作完成".to_string(),
    }
}

fn storage_test_stage_label(stage: &str) -> String {
    match stage {
        "connection" => "连接检查".to_string(),
        "put" => "写入对象".to_string(),
        "get" => "读取对象".to_string(),
        "delete" => "删除对象".to_string(),
        _ => String::new(),
    }
}

fn provider_name_from_id(providers: &[Value], id: &str) -> String {
    if id.trim().is_empty() {
        return "按存储路由".to_string();
    }
    providers
        .iter()
        .find(|provider| json_string(provider, "id") == id)
        .map(|provider| json_string(provider, "name"))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| short_text(id, 12))
}

fn orientation_label(value: &str) -> String {
    match value {
        "landscape" => "横图".to_string(),
        "portrait" => "竖图".to_string(),
        "square" => "方图".to_string(),
        other => other.to_string(),
    }
}

fn object_type_label(value: &str) -> String {
    match value {
        "original" => "原图".to_string(),
        "preview" => "预览图".to_string(),
        "avatar" => "头像".to_string(),
        "backup" => "备份文件".to_string(),
        other => other.to_string(),
    }
}

fn migration_mode_label(value: &str) -> String {
    match value {
        "copy" => "复制".to_string(),
        "move" => "移动".to_string(),
        "backup" => "备份".to_string(),
        other => other.to_string(),
    }
}

fn audit_type_label(value: &str) -> String {
    match value {
        "keyword" => "关键词审核".to_string(),
        "ai" => "智能审核".to_string(),
        "llm" => "大模型审核".to_string(),
        "manual" => "人工审核".to_string(),
        "third_party" => "第三方审核".to_string(),
        other => other.to_string(),
    }
}

fn provider_label(value: &str) -> String {
    match value {
        "local" => "本地".to_string(),
        "fastapi" => "智能审核服务".to_string(),
        "keyword" => "关键词".to_string(),
        other => other.to_string(),
    }
}

fn risk_label(value: &str) -> String {
    match value {
        "low" => "低风险".to_string(),
        "medium" => "中风险".to_string(),
        "high" => "高风险".to_string(),
        "unknown" => "未知风险".to_string(),
        other => other.to_string(),
    }
}

fn admin_action_label(value: &str) -> String {
    value.replace('.', " / ")
}

fn admin_target_label(value: &str) -> String {
    match value {
        "user" => "用户".to_string(),
        "image" => "图片".to_string(),
        "audit_task" => "审核任务".to_string(),
        "storage_provider" => "存储".to_string(),
        "settings" => "设置".to_string(),
        "migration_task" => "迁移任务".to_string(),
        "backup_task" => "备份任务".to_string(),
        other => other.to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_upload_settings_form(
    value: &Value,
    allowed_mime_types: NodeRef<Input>,
    webp_enabled: NodeRef<Input>,
    webp_max_width: NodeRef<Input>,
    webp_max_height: NodeRef<Input>,
    webp_quality: NodeRef<Input>,
    remove_exif: NodeRef<Input>,
    max_tags_per_image: NodeRef<Input>,
    max_tag_length: NodeRef<Input>,
    tag_sensitive_words: NodeRef<Input>,
    tag_review_required: NodeRef<Input>,
) {
    title_value(
        allowed_mime_types,
        &json_string_array(value, "allowed_mime_types").join(","),
    );
    checked_set_value(webp_enabled, json_bool(value, "webp_enabled", true));
    title_value(
        webp_max_width,
        &json_i64_or(value, "webp_max_width", 512).to_string(),
    );
    title_value(
        webp_max_height,
        &json_i64_or(value, "webp_max_height", 512).to_string(),
    );
    title_value(
        webp_quality,
        &json_i64_or(value, "webp_quality", 75).to_string(),
    );
    checked_set_value(remove_exif, json_bool(value, "remove_exif", true));
    title_value(
        max_tags_per_image,
        &json_i64_or(value, "max_tags_per_image", 10).to_string(),
    );
    title_value(
        max_tag_length,
        &json_i64_or(value, "max_tag_length", 32).to_string(),
    );
    title_value(
        tag_sensitive_words,
        &json_string_array(value, "tag_sensitive_words").join(","),
    );
    checked_set_value(
        tag_review_required,
        json_bool(value, "tag_review_required", false),
    );
}

struct RandomSettingsRefs {
    enabled: NodeRef<Input>,
    default_image: NodeRef<Select>,
    limit_enabled: NodeRef<Input>,
    allow_tag_filter: NodeRef<Input>,
    allow_orientation_filter: NodeRef<Input>,
    allow_resolution_filter: NodeRef<Input>,
    no_match_strategy: NodeRef<Select>,
}

fn fill_random_settings_form(value: &Value, refs: RandomSettingsRefs) {
    checked_set_value(refs.enabled, json_bool(value, "enabled", true));
    select_set_value(
        refs.default_image,
        &json_string_or(value, "default_image", "preview"),
    );
    checked_set_value(refs.limit_enabled, json_bool(value, "limit_enabled", true));
    checked_set_value(
        refs.allow_tag_filter,
        json_bool(value, "allow_tag_filter", true),
    );
    checked_set_value(
        refs.allow_orientation_filter,
        json_bool(value, "allow_orientation_filter", true),
    );
    checked_set_value(
        refs.allow_resolution_filter,
        json_bool(value, "allow_resolution_filter", true),
    );
    select_set_value(
        refs.no_match_strategy,
        &json_string_or(value, "no_match_strategy", "not_found"),
    );
}

#[allow(clippy::too_many_arguments)]
fn fill_audit_form(
    value: &Value,
    ai_enabled: NodeRef<Input>,
    service_url: NodeRef<Input>,
    api_token: NodeRef<Input>,
    failure_strategy: NodeRef<Select>,
    keyword_enabled: NodeRef<Input>,
    ocr_enabled: NodeRef<Input>,
    description_enabled: NodeRef<Input>,
    keywords: NodeRef<Input>,
) {
    checked_set_value(ai_enabled, json_bool(value, "ai_enabled", true));
    title_value(service_url, &json_string(value, "service_url"));
    title_value(api_token, &json_string(value, "api_token"));
    select_set_value(
        failure_strategy,
        &json_string_or(value, "failure_strategy", "manual_required"),
    );
    checked_set_value(keyword_enabled, json_bool(value, "keyword_enabled", true));
    checked_set_value(ocr_enabled, json_bool(value, "ocr_enabled", true));
    checked_set_value(
        description_enabled,
        json_bool(value, "description_enabled", true),
    );
    title_value(keywords, &json_string_array(value, "keywords").join(","));
}

struct AuditSettingsValues {
    ai_enabled: bool,
    service_url: String,
    api_token: String,
    failure_strategy: String,
    keyword_enabled: bool,
    ocr_enabled: bool,
    description_enabled: bool,
    keywords: String,
}

fn audit_settings_body(existing: &Value, values: AuditSettingsValues) -> Value {
    let mut body = existing.clone();
    ensure_object(&mut body);
    object_insert(&mut body, "ai_enabled", json!(values.ai_enabled));
    object_insert(&mut body, "service_type", json!("fastapi"));
    object_insert(&mut body, "service_url", json!(values.service_url));
    object_insert(&mut body, "api_token", json!(values.api_token));
    object_insert(
        &mut body,
        "failure_strategy",
        json!(values.failure_strategy),
    );
    object_insert(&mut body, "keyword_enabled", json!(values.keyword_enabled));
    object_insert(
        &mut body,
        "filename_keyword_enabled",
        json!(values.keyword_enabled),
    );
    object_insert(&mut body, "ocr_enabled", json!(values.ocr_enabled));
    object_insert(
        &mut body,
        "description_enabled",
        json!(values.description_enabled),
    );
    object_insert(
        &mut body,
        "tag_suggestions_enabled",
        json!(values.description_enabled),
    );
    object_insert(
        &mut body,
        "keywords",
        json!(parse_comma_values(&values.keywords)),
    );
    body
}

fn fill_site_settings_form(
    value: &Value,
    title: NodeRef<Input>,
    subtitle: NodeRef<Input>,
    guest_upload_enabled: NodeRef<Input>,
    guest_review_strategy: NodeRef<Select>,
) {
    title_value(title, &json_string_or(value, "title", "潮汐图床"));
    title_value(subtitle, &json_string_or(value, "subtitle", ""));
    checked_set_value(
        guest_upload_enabled,
        json_bool(value, "guest_upload_enabled", true),
    );
    select_set_value(
        guest_review_strategy,
        &json_string_or(value, "guest_review_strategy", "manual_required"),
    );
}

#[allow(clippy::too_many_arguments)]
fn fill_smtp_settings_form(
    value: &Value,
    name: NodeRef<Input>,
    host: NodeRef<Input>,
    port: NodeRef<Input>,
    username: NodeRef<Input>,
    password: NodeRef<Input>,
    from_email: NodeRef<Input>,
    from_name: NodeRef<Input>,
    enabled: NodeRef<Input>,
) {
    title_value(name, &json_string_or(value, "name", "SMTP"));
    title_value(host, &json_string(value, "host"));
    title_value(port, &json_i64_or(value, "port", 587).to_string());
    title_value(username, &json_string(value, "username"));
    title_value(password, &json_string(value, "password"));
    title_value(from_email, &json_string(value, "from_email"));
    title_value(from_name, &json_string_or(value, "from_name", "潮汐图床"));
    checked_set_value(enabled, json_bool(value, "enabled", false));
}


#[allow(clippy::too_many_arguments)]
fn storage_config_from_form(
    provider: &str,
    local_root: NodeRef<Input>,
    local_public_prefix: NodeRef<Input>,
    endpoint: NodeRef<Input>,
    region: NodeRef<Input>,
    bucket: NodeRef<Input>,
    r2_account_id: NodeRef<Input>,
    r2_jurisdiction: NodeRef<Select>,
    access_mode: NodeRef<Select>,
    presigned_url_ttl_seconds: NodeRef<Input>,
    access_key_id: NodeRef<Input>,
    secret_access_key: NodeRef<Input>,
    session_token: NodeRef<Input>,
    public_domain: NodeRef<Input>,
    path_prefix: NodeRef<Input>,
    client_id: NodeRef<Input>,
    tenant_id: NodeRef<Input>,
    client_secret: NodeRef<Input>,
    refresh_token: NodeRef<Input>,
    drive_email: NodeRef<Input>,
    root_dir: NodeRef<Input>,
    namespace: NodeRef<Input>,
    tenancy_ocid: NodeRef<Input>,
    user_ocid: NodeRef<Input>,
    fingerprint: NodeRef<Input>,
    private_key: NodeRef<Textarea>,
) -> Value {
    let path_prefix_value = input_value(path_prefix).unwrap_or_default();
    match provider {
        "local" => {
            let mut config = json!({});
            if let Some(value) = input_value(local_root).filter(|value| !value.is_empty()) {
                config["root"] = json!(value);
            }
            if let Some(value) = input_value(local_public_prefix).filter(|value| !value.is_empty())
            {
                config["public_prefix"] = json!(value);
            }
            if !path_prefix_value.is_empty() {
                config["path_prefix"] = json!(path_prefix_value);
            }
            config
        }
        "onedrive" => json!({
            "client_id": input_value(client_id).unwrap_or_default(),
            "tenant_id": input_value(tenant_id).unwrap_or_default(),
            "client_secret": input_value(client_secret).unwrap_or_default(),
            "refresh_token": input_value(refresh_token).unwrap_or_default(),
            "email": input_value(drive_email).unwrap_or_default(),
            "root_dir": input_value(root_dir).unwrap_or_else(|| "TideImages".to_string()),
            "path_prefix": path_prefix_value,
        }),
        "oracle_oci_native" => json!({
            "region": input_value(region).unwrap_or_default(),
            "namespace": input_value(namespace).unwrap_or_default(),
            "bucket": input_value(bucket).unwrap_or_default(),
            "tenancy_ocid": input_value(tenancy_ocid).unwrap_or_default(),
            "user_ocid": input_value(user_ocid).unwrap_or_default(),
            "fingerprint": input_value(fingerprint).unwrap_or_default(),
            "private_key": textarea_current_value(private_key).unwrap_or_default(),
            "public_domain": input_value(public_domain).unwrap_or_default(),
            "path_prefix": path_prefix_value,
        }),
        "cloudflare_r2" => json!({
            "account_id": input_value(r2_account_id).unwrap_or_default(),
            "jurisdiction": select_value(r2_jurisdiction).unwrap_or_else(|| "default".to_string()),
            "endpoint": input_value(endpoint).unwrap_or_default(),
            "region": input_value(region).unwrap_or_else(|| "auto".to_string()),
            "bucket": input_value(bucket).unwrap_or_default(),
            "access_key_id": input_value(access_key_id).unwrap_or_default(),
            "secret_access_key": input_value(secret_access_key).unwrap_or_default(),
            "session_token": input_value(session_token).unwrap_or_default(),
            "public_domain": input_value(public_domain).unwrap_or_default(),
            "access_mode": select_value(access_mode).unwrap_or_else(|| "signed_url".to_string()),
            "presigned_url_ttl_seconds": input_i64(presigned_url_ttl_seconds).unwrap_or(3600),
            "path_prefix": path_prefix_value,
        }),
        _ => json!({
            "endpoint": input_value(endpoint).unwrap_or_default(),
            "region": input_value(region).unwrap_or_default(),
            "bucket": input_value(bucket).unwrap_or_default(),
            "access_key_id": input_value(access_key_id).unwrap_or_default(),
            "secret_access_key": input_value(secret_access_key).unwrap_or_default(),
            "session_token": input_value(session_token).unwrap_or_default(),
            "public_domain": input_value(public_domain).unwrap_or_default(),
            "presigned_url_ttl_seconds": input_i64(presigned_url_ttl_seconds).unwrap_or(3600),
            "path_prefix": path_prefix_value,
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_storage_provider_form(
    value: &Value,
    name: NodeRef<Input>,
    provider_type: NodeRef<Select>,
    enabled: NodeRef<Input>,
    priority: NodeRef<Input>,
    local_root: NodeRef<Input>,
    local_public_prefix: NodeRef<Input>,
    endpoint: NodeRef<Input>,
    region: NodeRef<Input>,
    bucket: NodeRef<Input>,
    r2_account_id: NodeRef<Input>,
    r2_jurisdiction: NodeRef<Select>,
    access_mode: NodeRef<Select>,
    presigned_url_ttl_seconds: NodeRef<Input>,
    access_key_id: NodeRef<Input>,
    secret_access_key: NodeRef<Input>,
    session_token: NodeRef<Input>,
    public_domain: NodeRef<Input>,
    path_prefix: NodeRef<Input>,
    client_id: NodeRef<Input>,
    tenant_id: NodeRef<Input>,
    client_secret: NodeRef<Input>,
    refresh_token: NodeRef<Input>,
    drive_email: NodeRef<Input>,
    root_dir: NodeRef<Input>,
    namespace: NodeRef<Input>,
    tenancy_ocid: NodeRef<Input>,
    user_ocid: NodeRef<Input>,
    fingerprint: NodeRef<Input>,
    private_key: NodeRef<Textarea>,
) {
    title_value(name, &json_string(value, "name"));
    select_set_value(
        provider_type,
        &json_string_or(value, "provider_type", "local"),
    );
    checked_set_value(enabled, json_bool(value, "enabled", true));
    title_value(priority, &json_i64_or(value, "priority", 100).to_string());
    clear_storage_form(
        local_root,
        local_public_prefix,
        endpoint,
        region,
        bucket,
        r2_account_id,
        r2_jurisdiction,
        access_mode,
        presigned_url_ttl_seconds,
        access_key_id,
        secret_access_key,
        session_token,
        public_domain,
        path_prefix,
        client_id,
        tenant_id,
        client_secret,
        refresh_token,
        drive_email,
        root_dir,
        namespace,
        tenancy_ocid,
        user_ocid,
        fingerprint,
        private_key,
    );
    let config = value
        .get("config_json")
        .cloned()
        .unwrap_or_else(|| json!({}));
    title_value(local_root, &json_string(&config, "root"));
    title_value(local_public_prefix, &json_string(&config, "public_prefix"));
    title_value(endpoint, &json_string(&config, "endpoint"));
    title_value(region, &json_string(&config, "region"));
    title_value(bucket, &json_string(&config, "bucket"));
    title_value(r2_account_id, &json_string(&config, "account_id"));
    select_set_value(
        r2_jurisdiction,
        &json_string_or(&config, "jurisdiction", "default"),
    );
    select_set_value(
        access_mode,
        &json_string_or(&config, "access_mode", "signed_url"),
    );
    title_value(
        presigned_url_ttl_seconds,
        &json_i64_or(&config, "presigned_url_ttl_seconds", 3600).to_string(),
    );
    title_value(access_key_id, &json_string(&config, "access_key_id"));
    title_sensitive_value(
        secret_access_key,
        &json_string(&config, "secret_access_key"),
    );
    title_sensitive_value(session_token, &json_string(&config, "session_token"));
    title_value(public_domain, &json_string(&config, "public_domain"));
    title_value(path_prefix, &json_string(&config, "path_prefix"));
    title_value(client_id, &json_string(&config, "client_id"));
    title_value(tenant_id, &json_string(&config, "tenant_id"));
    title_sensitive_value(client_secret, &json_string(&config, "client_secret"));
    title_sensitive_value(refresh_token, &json_string(&config, "refresh_token"));
    title_value(drive_email, &json_string(&config, "email"));
    title_value(root_dir, &json_string(&config, "root_dir"));
    title_value(namespace, &json_string(&config, "namespace"));
    title_value(tenancy_ocid, &json_string(&config, "tenancy_ocid"));
    title_value(user_ocid, &json_string(&config, "user_ocid"));
    title_value(fingerprint, &json_string(&config, "fingerprint"));
    textarea_sensitive_value(private_key, &json_string(&config, "private_key"));
}

#[allow(clippy::too_many_arguments)]
fn clear_storage_form(
    local_root: NodeRef<Input>,
    local_public_prefix: NodeRef<Input>,
    endpoint: NodeRef<Input>,
    region: NodeRef<Input>,
    bucket: NodeRef<Input>,
    r2_account_id: NodeRef<Input>,
    r2_jurisdiction: NodeRef<Select>,
    access_mode: NodeRef<Select>,
    presigned_url_ttl_seconds: NodeRef<Input>,
    access_key_id: NodeRef<Input>,
    secret_access_key: NodeRef<Input>,
    session_token: NodeRef<Input>,
    public_domain: NodeRef<Input>,
    path_prefix: NodeRef<Input>,
    client_id: NodeRef<Input>,
    tenant_id: NodeRef<Input>,
    client_secret: NodeRef<Input>,
    refresh_token: NodeRef<Input>,
    drive_email: NodeRef<Input>,
    root_dir: NodeRef<Input>,
    namespace: NodeRef<Input>,
    tenancy_ocid: NodeRef<Input>,
    user_ocid: NodeRef<Input>,
    fingerprint: NodeRef<Input>,
    private_key: NodeRef<Textarea>,
) {
    for node in [
        local_root,
        local_public_prefix,
        endpoint,
        region,
        bucket,
        r2_account_id,
        presigned_url_ttl_seconds,
        access_key_id,
        secret_access_key,
        session_token,
        public_domain,
        path_prefix,
        client_id,
        tenant_id,
        client_secret,
        refresh_token,
        drive_email,
        root_dir,
        namespace,
        tenancy_ocid,
        user_ocid,
        fingerprint,
    ] {
        title_value(node, "");
    }
    select_set_value(r2_jurisdiction, "default");
    select_set_value(access_mode, "signed_url");
    textarea_value(private_key, "");
}

fn migration_filter_body(
    status: NodeRef<Select>,
    tag: NodeRef<Input>,
    user_id: NodeRef<Input>,
) -> Value {
    let mut body = json!({});
    let status = select_value(status).unwrap_or_default();
    let tag = input_value(tag).unwrap_or_default();
    let user_id = input_value(user_id).unwrap_or_default();
    if !status.is_empty() {
        object_insert(&mut body, "status", json!(status));
    }
    if !tag.is_empty() {
        object_insert(&mut body, "tag", json!(tag));
    }
    if !user_id.is_empty() {
        object_insert(&mut body, "user_id", json!(user_id));
    }
    body
}

fn json_string_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn json_i64_or(value: &Value, key: &str, fallback: i64) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(fallback)
}

fn ensure_object(value: &mut Value) {
    if !value.is_object() {
        *value = json!({});
    }
}

fn object_insert(value: &mut Value, key: &str, next: Value) {
    if let Some(map) = value.as_object_mut() {
        map.insert(key.to_string(), next);
    }
}

fn error_label(code: &str, message: &str) -> String {
    match code {
        "BAD_REQUEST" => format!("请求参数有误：{message}"),
        "QUOTA_EXCEEDED" => format!("配额不足：{message}"),
        "FILE_TOO_LARGE" => "文件超过大小限制".to_string(),
        "FILE_TYPE_NOT_ALLOWED" => "文件类型不允许".to_string(),
        "UNAUTHORIZED" => "请先登录".to_string(),
        "FORBIDDEN" => format!("没有权限：{message}"),
        "NOT_FOUND" => "资源不存在".to_string(),
        "CONFLICT" => format!("数据冲突：{message}"),
        "EXTERNAL_SERVICE_ERROR" => format!("外部服务异常：{message}"),
        "DATABASE_ERROR" => "数据库异常".to_string(),
        _ => message.to_string(),
    }
}

fn auth_mode_title(mode: Option<AuthMode>) -> &'static str {
    match mode.unwrap_or(AuthMode::Login) {
        AuthMode::Login => "登录潮汐图床",
        AuthMode::Register => "注册新账号",
        AuthMode::Reset => "重置密码",
    }
}

fn auth_submit_label(mode: Option<AuthMode>) -> &'static str {
    match mode.unwrap_or(AuthMode::Login) {
        AuthMode::Login => "登录",
        AuthMode::Register => "注册并登录",
        AuthMode::Reset => "确认重置",
    }
}

fn upload_result_view(
    value: Value,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
    preview_modal: RwSignal<Option<ImageLinkBundle>>,
    links_modal: RwSignal<Option<ImageLinkBundle>>,
) -> AnyView {
    if let Some(items) = value.get("items").and_then(Value::as_array).cloned() {
        return view! {
            <>
                {items.into_iter().map(move |item| upload_result_item_view(item, notify, preview_modal, links_modal)).collect_view()}
            </>
        }
        .into_any();
    }
    if value.get("url").is_some() {
        return upload_success_card(value, notify, preview_modal, links_modal).into_any();
    }
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("操作已完成，返回内容已收起。")
        .to_string();
    view! {
        <article class="upload-result-card">
            <strong>"处理完成"</strong>
            <span>{message}</span>
        </article>
    }
    .into_any()
}

fn upload_result_item_view(
    item: Value,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
    preview_modal: RwSignal<Option<ImageLinkBundle>>,
    links_modal: RwSignal<Option<ImageLinkBundle>>,
) -> AnyView {
    let file_name = json_string(&item, "file_name");
    if item
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let response = item.get("response").cloned().unwrap_or_else(|| json!({}));
        return upload_success_card(response, notify, preview_modal, links_modal).into_any();
    }
    let message = item
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("上传失败")
        .to_string();
    view! {
        <article class="upload-result-card failed">
            <strong>{if file_name.is_empty() { "上传失败".to_string() } else { file_name }}</strong>
            <span>{message}</span>
        </article>
    }
    .into_any()
}

fn upload_success_card(
    response: Value,
    notify: impl Fn(String) + Copy + Send + Sync + 'static,
    preview_modal: RwSignal<Option<ImageLinkBundle>>,
    links_modal: RwSignal<Option<ImageLinkBundle>>,
) -> impl IntoView {
    let id = json_string(&response, "id");
    let raw_status = json_string(&response, "status");
    let status = status_label(&raw_status);
    let deduplicated = response
        .get("deduplicated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let bundle = ImageLinkBundle::from_upload(&response);
    let preview_bundle = bundle.clone();
    let links_bundle = bundle.clone();
    view! {
        <article class="upload-result-card">
            <div class="upload-result-main">
                <button class="upload-preview-button" type="button" on:click=move |_| {
                    preview_modal.set(Some(preview_bundle.clone()));
                }>
                    <ImageThumb src=bundle.preview_src() alt=bundle.alt.clone()/>
                </button>
                <span>
                    <strong>{if id.is_empty() { "上传成功".to_string() } else { id }}</strong>
                    <small>{format!("{}{}", status, if deduplicated { " · 已复用相同文件" } else { "" })}</small>
                </span>
            </div>
            <div class="row-actions">
                <button type="button" on:click=move |_| {
                    links_modal.set(Some(links_bundle.clone()));
                    notify("已打开部署引用".to_string());
                }>"部署引用"</button>
            </div>
        </article>
    }
}

fn json_string_or(value: &Value, key: &str, fallback: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(fallback)
        .to_string()
}

#[allow(clippy::too_many_arguments)]
fn fill_theme_form(
    theme: &ThemeSettings,
    preset: NodeRef<Select>,
    mode: NodeRef<Select>,
    radius: NodeRef<Input>,
    blur: NodeRef<Input>,
    mobile_blur: NodeRef<Input>,
    card_opacity: NodeRef<Input>,
    primary_color: NodeRef<Input>,
    accent_color: NodeRef<Input>,
    background_color: NodeRef<Input>,
    surface_color: NodeRef<Input>,
    font: NodeRef<Input>,
    background_image: NodeRef<Input>,
    simplify_mobile_effects: NodeRef<Input>,
) {
    select_set_value(preset, &theme.preset);
    select_set_value(mode, &theme.mode);
    title_value(radius, &theme.radius.to_string());
    title_value(blur, &theme.blur.to_string());
    title_value(mobile_blur, &theme.mobile_blur.to_string());
    title_value(card_opacity, &format!("{:.2}", theme.card_opacity));
    title_value(primary_color, &theme.primary_color);
    title_value(accent_color, &theme.accent_color);
    title_value(background_color, &theme.background_color);
    title_value(surface_color, &theme.surface_color);
    title_value(font, &theme.font);
    title_value(background_image, &theme.background_image);
    checked_set_value(simplify_mobile_effects, theme.simplify_mobile_effects);
}
