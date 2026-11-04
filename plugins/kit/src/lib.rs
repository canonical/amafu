#![allow(unsafe_code, clippy::same_length_and_capacity)]

wit_bindgen::generate!({
    path: "../../wit",
    world: "storage-plugin",
    pub_export_macro: true,
    export_macro_name: "export_storage_plugin",
    default_bindings_module: "amafu_storage_plugin_kit",
});

pub use amafu::storage::{http, logging, secrets, types};
pub use exports::amafu::storage::provider::Guest as StorageProvider;
