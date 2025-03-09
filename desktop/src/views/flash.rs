use dioxus::prelude::*;

#[component]
pub fn Flash() -> Element {
    rsx! { 
        div { class: "is-flex is-align-items-center is-justify-content-center is-flex-grow-1",
            p { style:"font-size: 320px", "TBA"}

        }
    }
}
