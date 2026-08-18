//! Subcommand runners extracted verbatim from cli.rs (PRR-7).
//! No behavior changes.

/// `aikoql model install [MODEL_ID]` — the ONLY code path that downloads a
/// model (PRR-3). Installs into the local store for offline runtime use.
pub(crate) fn run_model_install(model_id: &str, model_dir: Option<&str>) {
    #[cfg(feature = "embedding-candle")]
    {
        let store = crate::model_store_dir(model_dir);
        match aikoql_semantic::provider::CandleEmbedding::install(model_id, &store) {
            Ok(dir) => {
                println!("Installed {model_id} into {}", dir.display());
                println!("The server and ingest-dir will pick it up on their next start.");
            }
            Err(e) => {
                eprintln!("install failed: {e}");
                std::process::exit(1);
            }
        }
    }
    #[cfg(not(feature = "embedding-candle"))]
    {
        let _ = (model_id, model_dir);
        eprintln!("this binary was not compiled with the embedding-candle feature");
        std::process::exit(1);
    }
}
