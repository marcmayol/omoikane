//! Phase 3: `omoi mcp` end to end, with a real MCP client (rmcp's) that starts it as a child
//! process, just like Claude Code.

mod common;

use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};
use std::path::PathBuf;

fn int8_model() -> Option<PathBuf> {
    let dir = std::env::var_os("OMOI_MODELO_INT8")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|h| h.join(".omoikane/models/potion-multilingual-128M-int8")))?;
    dir.join("model.safetensors").exists().then_some(dir)
}

struct Fixture {
    dir: common::TempDir,
}

impl Fixture {
    fn new(name: &str, with_model: bool) -> Option<Fixture> {
        let dir = common::TempDir::new(&format!("mcp-{name}"));
        std::fs::create_dir_all(dir.join("repo").join(".git")).unwrap();
        let p = Fixture { dir };
        if with_model {
            let int8 = int8_model()?;
            let o = std::process::Command::new(env!("CARGO_BIN_EXE_omoi"))
                .args(["model", "install", "--from"])
                .arg(int8)
                .env("OMOIKANE_DB", p.dir.join("o.db"))
                .env("OMOIKANE_MODELS", p.dir.join("models"))
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        }
        Some(p)
    }

    async fn client(&self) -> RunningService<RoleClient, ()> {
        let (db, models, repo) = (self.dir.join("o.db"), self.dir.join("models"), self.dir.join("repo"));
        let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_omoi")).configure(|c| {
            c.arg("mcp").current_dir(repo).env("OMOIKANE_DB", db).env("OMOIKANE_MODELS", models).env_remove("OMOIKANE_LANG");
        });
        ().serve(TokioChildProcess::new(cmd).unwrap()).await.unwrap()
    }
}

/// Calls a tool and returns (error?, the JSON of its text).
async fn call(c: &RunningService<RoleClient, ()>, name: &'static str, args: Value) -> (bool, Value) {
    let r = c
        .call_tool(CallToolRequestParams::new(name).with_arguments(args.as_object().unwrap().clone()))
        .await
        .unwrap();
    let text = r.content.first().and_then(|b| b.as_text()).map(|t| t.text.clone()).unwrap_or_default();
    let v = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (r.is_error.unwrap_or(false), v)
}

#[tokio::test]
async fn the_tools_end_to_end() {
    let p = Fixture::new("todo", false).unwrap();
    let c = p.client().await;

    let info = c.peer_info().unwrap();
    assert_eq!(info.server_info.as_ref().unwrap().name, "omoikane");
    assert!(info.instructions.as_deref().unwrap().contains("remember"));
    let mut names: Vec<String> = c.list_all_tools().await.unwrap().into_iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(names, ["context", "decisions", "forget", "get", "history", "recent", "remember", "search", "skill", "update"]);

    let (err, r) = call(
        &c,
        "remember",
        json!({"title": "Bug en auth: el token caduca antes de tiempo", "content": "El middleware compara la hora local con la caducidad en UTC.", "kind": "gotcha", "paths": ["src/auth.rs"]}),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(r["saved"], true);
    let id = r["id"].as_str().unwrap().to_string();
    // The source says which MCP client saved it (the tests' client introduces itself as «rmcp»).
    let (_, whole) = call(&c, "get", json!({"id": id})).await;
    assert_eq!(whole["source"], "mcp:rmcp", "{whole}");

    // A misspelled kind comes back as a tool error, readable by the agent.
    let (err, r) = call(&c, "remember", json!({"title": "X", "content": "y", "kind": "bugfix"})).await;
    assert!(err);
    assert!(r.as_str().unwrap().contains("fact, decision, gotcha, preference or procedure"));

    // Search by words and by path.
    let (_, r) = call(&c, "search", json!({"query": "token caduca"})).await;
    assert_eq!(r[0]["id"], id.as_str());
    assert_eq!(r[0]["paths"][0], "src/auth.rs");
    let (_, r) = call(&c, "search", json!({"query": "auth.rs"})).await;
    assert_eq!(r[0]["id"], id.as_str());

    // get accepts the start of the id.
    let (_, r) = call(&c, "get", json!({"id": &id[..12]})).await;
    assert!(r["content"].as_str().unwrap().contains("UTC"));

    let (err, _) = call(&c, "update", json!({"id": id, "content": "Arreglado: todo va en UTC.", "reason": "ya está corregido"})).await;
    assert!(!err);
    let (_, r) = call(&c, "history", json!({"id": id})).await;
    assert_eq!(r[0]["reason"], "ya está corregido");

    // A global preference shows up in context and in recent.
    call(&c, "remember", json!({"title": "Commits en inglés", "content": "Una línea, en imperativo.", "kind": "preference", "global": true})).await;
    let (_, r) = call(&c, "context", json!({})).await;
    assert_eq!(r["memories"].as_array().unwrap().len(), 1);
    assert_eq!(r["memories"][0]["global"], true);
    // And what is worth reviewing: the gotcha points to `src/auth.rs`, which doesn't exist in this repository.
    assert_eq!(r["review"][0]["kind"], "broken_path", "{r}");
    assert!(r["review"][0]["detail"].as_str().unwrap().contains("src/auth.rs"), "{r}");
    let (_, r) = call(&c, "recent", json!({})).await;
    assert_eq!(r.as_array().unwrap().len(), 2);

    // forget doesn't ask: the agent has already decided.
    let (err, r) = call(&c, "forget", json!({"id": id})).await;
    assert!(!err, "{r}");
    let (err, _) = call(&c, "get", json!({"id": id})).await;
    assert!(err);
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn with_model_warns_about_duplicates() {
    let Some(p) = Fixture::new("duplicados", true) else {
        eprintln!("SALTADO: no está el modelo int8");
        return;
    };
    let c = p.client().await;
    let (_, r) = call(
        &c,
        "remember",
        json!({"title": "Bug en auth: el token caduca antes de tiempo", "content": "El middleware compara la hora local con la caducidad del token, que va en UTC.", "kind": "gotcha"}),
    )
    .await;
    let id = r["id"].as_str().unwrap().to_string();
    let similar = json!({"title": "El token de auth caduca demasiado pronto", "content": "El middleware compara la hora local con la caducidad del token en UTC.", "kind": "gotcha"});
    let (err, r) = call(&c, "remember", similar.clone()).await;
    assert!(!err);
    assert_eq!(r["saved"], false);
    assert_eq!(r["duplicate_of"]["id"], id.as_str());
    assert!(r["hint"].as_str().unwrap().contains("force"));
    // The agent decides: with force it is saved.
    let mut forced = similar;
    forced["force"] = json!(true);
    let (_, r) = call(&c, "remember", forced).await;
    assert_eq!(r["saved"], true);
    let (_, r) = call(&c, "search", json!({"query": "token caduca"})).await;
    assert!(r[0]["similarity"].is_number());
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn save_and_move_to_another_project() {
    let p = Fixture::new("proyecto", false).unwrap();
    let other = p.dir.join("otro");
    std::fs::create_dir_all(other.join(".git")).unwrap();
    let c = p.client().await;
    let (err, r) = call(
        &c,
        "remember",
        json!({"title": "La otra web usa Astro", "content": "Astro 5.", "kind": "fact", "project": other.to_str().unwrap()}),
    )
    .await;
    assert!(!err, "{r}");
    assert!(r["project"].as_str().unwrap().ends_with("otro"), "{r}");
    let id = r["id"].as_str().unwrap().to_string();
    // From this repo it isn't visible; after moving it here, it is.
    let (_, s) = call(&c, "search", json!({"query": "astro"})).await;
    assert!(s.as_array().unwrap().is_empty(), "{s}");
    let (err, _) = call(&c, "update", json!({"id": id, "project": p.dir.join("repo").to_str().unwrap()})).await;
    assert!(!err);
    let (_, s) = call(&c, "search", json!({"query": "astro"})).await;
    assert_eq!(s.as_array().unwrap().len(), 1, "{s}");
    let (_, g) = call(&c, "update", json!({"id": id, "project": ""})).await;
    assert_eq!(g["global"], true, "{g}");
    c.cancel().await.unwrap();
}
