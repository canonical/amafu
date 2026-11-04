wasmtime::component::bindgen!({
    path: "../../wit",
    world: "storage-plugin",
    imports: { default: async },
    exports: { default: async },
});
