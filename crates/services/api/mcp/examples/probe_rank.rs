//! Offline ranking probe: run compile_context against a live db's dir snapshot
//! with tunable fusion weights — no serve, no re-ingest for weight sweeps.
//!
//! Usage:
//!   cargo run -p aikoql-mcp --example probe_rank -- <db> "<task>" <target> [sem_weight] [sem_min]
//!
//! Prints the top 15 entities, then the target entity's rank, score, cosine,
//! and mention count (the components behind its final score).

use aikoql_ingestion::{compile_context_semantic_with, cosine_similarity, KnowledgeIr};
use aikoql_kernel::knowledge::kom::Value;
use aikoql_kernel::{
    EmbeddingProvider, Kernel, KnowledgeContext, RedbEngine, Subject, SystemClock,
};
use aikoql_semantic::provider::CandleEmbedding;
use std::collections::HashMap;
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: probe_rank <db> \"<task>\" <target-substring> [sem_weight] [sem_min]");
        std::process::exit(2);
    }
    let db = &args[1];
    let task = &args[2];
    let target = &args[3];
    let weight: f32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(3.0);
    let min: f32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0.30);

    let engine = RedbEngine::open(db).expect("open db");
    let k = Kernel::open(Arc::new(engine), Arc::new(SystemClock), 0).expect("kernel");
    let ctx = KnowledgeContext::from(&Subject::with_roles("ingest-dir", &["admin"]));

    // Find the ingested-directory snapshot KO and pull its two JSON blobs.
    let mut snap: Option<(String, String)> = None;
    for (koid, ..) in k.scan_heads().expect("scan heads") {
        if let Ok(ko) = k.get(ctx.clone(), &koid) {
            if ko.metadata.type_name != "aikoql:ingested-directory" {
                continue;
            }
            let prop = |name: &str| {
                ko.properties.get(name).and_then(|v| match v {
                    Value::Text(t) => Some(t.clone()),
                    _ => None,
                })
            };
            if let (Some(ir), Some(emb)) = (prop("ir_json"), prop("entity_embeddings")) {
                snap = Some((ir, emb));
                break;
            }
        }
    }
    let (ir_json, embeddings_json) = snap.expect("no dir snapshot in db");

    let ir: KnowledgeIr = serde_json::from_str(&ir_json).expect("parse ir_json");
    let stored: HashMap<String, Vec<f32>> =
        serde_json::from_str(&embeddings_json).expect("parse entity_embeddings");

    let p = CandleEmbedding::new().expect("candle model");
    let task_emb = p.embed(task, None).expect("embed task");
    let semantic: HashMap<String, f32> = stored
        .iter()
        .map(|(key, emb)| (key.clone(), cosine_similarity(&task_emb, emb)))
        .collect();

    // Candidate embedding-text experiments: any extra args are texts to embed
    // and cosine against the task — answers "what would this text score?"
    // without a re-ingest.
    for cand in args.iter().skip(6) {
        let emb = p.embed(cand, None).expect("embed candidate");
        println!(
            "candidate {:.3}  {}",
            cosine_similarity(&task_emb, &emb),
            cand
        );
    }

    // relation_boost: production default (0.65) — probes must mirror prod.
    let pkg = compile_context_semantic_with(task, &ir, 2500, Some(&semantic), weight, min, 0.65, None);

    println!(
        "task: {}\nweight={} min={} entities={}\n",
        task,
        weight,
        min,
        pkg.entities.len()
    );
    for (i, e) in pkg.entities.iter().take(15).enumerate() {
        let key = format!(
            "{}::{}",
            e.document_id.as_deref().unwrap_or_default(),
            e.name
        );
        let c = semantic.get(&key).copied().unwrap_or(0.0);
        println!(
            "{:3} {:6.2} c={:.3}  {:<44} {}",
            i + 1,
            e.score,
            c,
            e.name,
            e.justification
        );
    }

    // Target breakdown: rank in package + its raw components.
    let mut rank = None;
    let mut t_mentions = 0usize;
    let mut t_score = 0.0f32;
    let mut t_cos = 0.0f32;
    for (i, e) in pkg.entities.iter().enumerate() {
        if e.name.contains(target) && rank.is_none() {
            rank = Some(i + 1);
            t_mentions = e.mentions.len();
            t_score = e.score;
        }
    }
    for ent in &ir.entities {
        if ent.name.contains(target) {
            let key = format!(
                "{}::{}",
                ent.evidence.document_id.as_deref().unwrap_or_default(),
                ent.name
            );
            t_cos = semantic.get(&key).copied().unwrap_or(0.0);
            println!(
                "\n-- entity '{target}' ({}) doc={} confidence={:.2}",
                ent.type_hint.as_deref().unwrap_or("?"),
                ent.evidence.document_id.as_deref().unwrap_or("?"),
                ent.confidence
            );
            for (mi, m) in ent.mentions.iter().take(3).enumerate() {
                println!("   mention{}: {:.300}", mi + 1, m);
            }
        }
    }
    match rank {
        Some(r) => println!(
            "\ntarget '{target}': rank {r}/{}, score {:.2}, cosine {:.3}, mentions {}",
            pkg.entities.len(),
            t_score,
            t_cos,
            t_mentions
        ),
        None => println!(
            "\ntarget '{target}': NOT in package (cosine {:.3}, {} mentions — below pack cut)",
            t_cos, t_mentions
        ),
    }
}
