use dioxus::prelude::*;

#[component]
pub fn Home() -> Element {
    rsx! {
        div { class: "section is-flex my-fullheight",
            div { class: "is-fullheight is-flex is-align-items-center is-justify-content-center is-flex-grow-1 my-fullheight",
                iframe { class: "my-fullheight",
                    class: "is-flex-grow-1",
                    src: "https://www.youtube.com/embed/WKIBzVbrhOg?autoplay=1&mute=1&loop=1&playlist=WKIBzVbrhOg",
                    allowfullscreen: true,
                }
            }
        }
    }
}
