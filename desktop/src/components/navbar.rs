use crate::Route;
use dioxus::prelude::*;

const NAVBAR_CSS: Asset = asset!("/assets/styling/navbar.css");

#[component]
pub fn Navbar() -> Element {
    rsx! {
        document::Link { rel: "stylesheet", href: NAVBAR_CSS }

        div { class: "is-flex is-flex-direction-column", style:"height: 100vh; overflow: hidden;",
            div { class: "hero is-link is-small is-flex-shrink-0",
                div { class: "hero-body",
                    p { class: "title", "SpoolEase Desktop"}
                }
            }
            nav {  aria_label: "main navigation", class:"navbar is-light is-flex-shrink-0",  role: "navigation",
                // div { class: "navbar-menu", style: "display:flex;",
                        div { class: "navbar-start", style: "display: flex;",
                            Link { class: "navbar-item", active_class: "is-active is-tab", to: Route::Home {}, "Home" }
                            Link { class: "navbar-item", active_class: "is-active is-tab", to: Route::CustomFilaments {} , "Custom Filaments" }
                            Link { class: "navbar-item", active_class: "is-active is-tab", to: Route::Flash {}, "Flash" }
                        }
                // }
            }
            Outlet::<Route> {}
        }
    }
}
