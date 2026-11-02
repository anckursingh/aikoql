fn main() {
    // pyo3 0.29 moved this out of its own build script: without it, the
    // abi3 cdylib fails to link on macOS (undefined Python symbols).
    pyo3_build_config::add_extension_module_link_args();
}
