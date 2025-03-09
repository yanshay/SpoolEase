use dioxus::prelude::*;
use dioxus_sdk::clipboard::use_clipboard;
use rfd::FileDialog;

use crate::services::get_custom_filaments_index;

#[derive(Clone)]
pub struct CustomFilamentsState {
    pub custom_filaments_index: Signal<String>,
}

#[component]
pub fn CustomFilaments() -> Element {
    let mut clipboard = use_clipboard();

    let mut custom_filaments_index = use_context::<CustomFilamentsState>().custom_filaments_index;
    let mut generating = use_signal(|| false);

    rsx! {
        div { class: "section is-flex is-align-items-center is-flex-direction-column is-flex-grow-1", style: "overflow: hidden;", align_content: "center",
                textarea {class: "is-flex-grow-1",
                    class: "textarea", rows: 30, placeholder: "",
                    value: custom_filaments_index.read().to_string(),
                    oninput: move |event| custom_filaments_index.set(event.value())
                }
            div {
                class: "mt-4",
                button { 
                    class: "button is-primary ml-4",
                    disabled: generating(),
                    onclick: move |_| {
                        generating.set(true);
                        spawn(async move {
                            // tokio::time::sleep(Duration::from_secs(5)).await;
                            match get_custom_filaments_index().await {
                                Ok(v) => custom_filaments_index.set(v),
                                Err(e) => custom_filaments_index.set(e.to_string()),
                            };
                            generating.set(false);
                        }); 
                    },
                    {
                        if *(generating.read()) { "Generating" } else {"Generate"}
                    }
                }
                button {
                    class: "button is-primary ml-4",
                    disabled: if custom_filaments_index().is_empty() { true } else { false },
                    onclick: move |_event| clipboard.set(custom_filaments_index.read().to_string()).unwrap(),
                    "Copy All to Clipboard"
                }
                button {
                    class: "button is-primary ml-4",
                    disabled: if custom_filaments_index().is_empty() { true } else { false },
                    onclick: move |_| {
                        if !custom_filaments_index().is_empty() {
                            let path = FileDialog::new()
                            .save_file();
                            if let Some(path) = path {
                                std::fs::write(path, custom_filaments_index()).unwrap();
                            }
                        }
                    },
                "Save"
                }
            }
        }
    }
}
