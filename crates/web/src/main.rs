use leptos::{mount::mount_to_body, prelude::*};
use tide_web::App;

fn main() {
    console_error_panic_hook::set_once();
    mount_to_body(|| view! { <App/> });
}
