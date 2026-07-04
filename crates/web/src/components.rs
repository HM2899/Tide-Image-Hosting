use leptos::prelude::*;

#[component]
pub(crate) fn NavButton(
    label: &'static str,
    id: &'static str,
    view: RwSignal<String>,
    set_view: impl Fn(&'static str) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    view! {
        <button
            class=move || if view.get() == id { "selected" } else { "" }
            on:click=move |_| set_view(id)
        >
            {label}
        </button>
    }
}

#[component]
pub(crate) fn ViewPanel(
    id: &'static str,
    view: RwSignal<String>,
    children: Children,
) -> impl IntoView {
    view! {
        <section id=id class=move || if view.get() == id { "view active" } else { "view" }>
            {children()}
        </section>
    }
}
