use std::env;

use dioxus::prelude::*;

use components::Navbar;
use views::{CustomFilaments, CustomFilamentsState, Flash, Home};

mod components;
mod views;
mod services;

#[derive(Debug, Clone, Routable, PartialEq)]
#[rustfmt::skip]
enum Route {
    #[layout(Navbar)]
        #[route("/")]
        Home {},
        #[route("/custom-filaments")]
        CustomFilaments {},
        #[route("/flash")]
        Flash {},
}

const FAVICON: Asset = asset!("/assets/favicon.ico");
const MAIN_CSS: Asset = asset!("/assets/styling/main.css");

fn main() {
    if cfg!(target_os = "windows") {
        let user_data_dir = env::var("LOCALAPPDATA").expect("env var LOCALAPPDATA not found");
        let cfg = dioxus_desktop::Config::new().with_data_directory(user_data_dir);
        dioxus_desktop::launch::launch(App, vec![], vec![Box::new(cfg)])
    } else {
        dioxus::launch(App);
    }
}

#[component]
fn App() -> Element {
    let _custom_filaments_state = use_context_provider(|| CustomFilamentsState {
        custom_filaments_index: Signal::new(String::new()),
    });

    rsx! {
        // Global app resources
        document::Link { rel: "icon", href: FAVICON }
        document::Link { rel: "stylesheet", href: "https://cdn.jsdelivr.net/npm/bulma@1.0.2/css/bulma.min.css" }
        document::Link { rel: "stylesheet", href: MAIN_CSS }

        Router::<Route> {}
    }
}
