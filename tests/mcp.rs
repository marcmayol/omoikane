//! Fase 3: `omoi mcp` de punta a punta, con un cliente MCP de verdad (el de rmcp) que lo arranca
//! como proceso hijo, igual que Claude Code.

use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};
use std::path::PathBuf;

fn modelo_int8() -> Option<PathBuf> {
    let dir = std::env::var_os("OMOI_MODELO_INT8")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".omoikane/models/potion-multilingual-128M-int8")))?;
    dir.join("model.safetensors").exists().then_some(dir)
}

struct Prueba {
    dir: PathBuf,
}

impl Prueba {
    fn nueva(nombre: &str, con_modelo: bool) -> Option<Prueba> {
        let dir = std::env::temp_dir().join(format!("omoi-mcp-{nombre}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("repo").join(".git")).unwrap();
        let p = Prueba { dir };
        if con_modelo {
            let int8 = modelo_int8()?;
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

    async fn cliente(&self) -> RunningService<RoleClient, ()> {
        let (db, modelos, repo) = (self.dir.join("o.db"), self.dir.join("models"), self.dir.join("repo"));
        let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_omoi")).configure(|c| {
            c.arg("mcp").current_dir(repo).env("OMOIKANE_DB", db).env("OMOIKANE_MODELS", modelos).env_remove("OMOIKANE_LANG");
        });
        ().serve(TokioChildProcess::new(cmd).unwrap()).await.unwrap()
    }
}

impl Drop for Prueba {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Llama a una herramienta y devuelve (¿error?, el JSON de su texto).
async fn llamar(c: &RunningService<RoleClient, ()>, nombre: &'static str, args: Value) -> (bool, Value) {
    let r = c
        .call_tool(CallToolRequestParams::new(nombre).with_arguments(args.as_object().unwrap().clone()))
        .await
        .unwrap();
    let texto = r.content.first().and_then(|b| b.as_text()).map(|t| t.text.clone()).unwrap_or_default();
    let v = serde_json::from_str(&texto).unwrap_or(Value::String(texto));
    (r.is_error.unwrap_or(false), v)
}

#[tokio::test]
async fn las_herramientas_de_punta_a_punta() {
    let p = Prueba::nueva("todo", false).unwrap();
    let c = p.cliente().await;

    let info = c.peer_info().unwrap();
    assert_eq!(info.server_info.as_ref().unwrap().name, "omoikane");
    assert!(info.instructions.as_deref().unwrap().contains("remember"));
    let mut nombres: Vec<String> = c.list_all_tools().await.unwrap().into_iter().map(|t| t.name.to_string()).collect();
    nombres.sort();
    assert_eq!(nombres, ["context", "forget", "get", "history", "recent", "remember", "search", "update"]);

    let (err, r) = llamar(
        &c,
        "remember",
        json!({"title": "Bug en auth: el token caduca antes de tiempo", "content": "El middleware compara la hora local con la caducidad en UTC.", "kind": "gotcha", "paths": ["src/auth.rs"]}),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(r["saved"], true);
    let id = r["id"].as_str().unwrap().to_string();
    // El origen dice qué cliente MCP la guardó (el de los tests se presenta como «rmcp»).
    let (_, entera) = llamar(&c, "get", json!({"id": id})).await;
    assert_eq!(entera["source"], "mcp:rmcp", "{entera}");

    // Un tipo mal escrito vuelve como error de la herramienta, legible para el agente.
    let (err, r) = llamar(&c, "remember", json!({"title": "X", "content": "y", "kind": "bugfix"})).await;
    assert!(err);
    assert!(r.as_str().unwrap().contains("fact, decision, gotcha or preference"));

    // Buscar por palabras y por la ruta.
    let (_, r) = llamar(&c, "search", json!({"query": "token caduca"})).await;
    assert_eq!(r[0]["id"], id.as_str());
    assert_eq!(r[0]["paths"][0], "src/auth.rs");
    let (_, r) = llamar(&c, "search", json!({"query": "auth.rs"})).await;
    assert_eq!(r[0]["id"], id.as_str());

    // get acepta el principio del id.
    let (_, r) = llamar(&c, "get", json!({"id": &id[..12]})).await;
    assert!(r["content"].as_str().unwrap().contains("UTC"));

    let (err, _) = llamar(&c, "update", json!({"id": id, "content": "Arreglado: todo va en UTC.", "reason": "ya está corregido"})).await;
    assert!(!err);
    let (_, r) = llamar(&c, "history", json!({"id": id})).await;
    assert_eq!(r[0]["reason"], "ya está corregido");

    // Una preferencia global sale en context y en recent.
    llamar(&c, "remember", json!({"title": "Commits en inglés", "content": "Una línea, en imperativo.", "kind": "preference", "global": true})).await;
    let (_, r) = llamar(&c, "context", json!({})).await;
    assert_eq!(r["memories"].as_array().unwrap().len(), 1);
    assert_eq!(r["memories"][0]["global"], true);
    // Y lo que conviene revisar: la trampa apunta a `src/auth.rs`, que no existe en este repositorio.
    assert_eq!(r["review"][0]["kind"], "broken_path", "{r}");
    assert!(r["review"][0]["detail"].as_str().unwrap().contains("src/auth.rs"), "{r}");
    let (_, r) = llamar(&c, "recent", json!({})).await;
    assert_eq!(r.as_array().unwrap().len(), 2);

    // forget no pregunta: el agente ya lo ha decidido.
    let (err, r) = llamar(&c, "forget", json!({"id": id})).await;
    assert!(!err, "{r}");
    let (err, _) = llamar(&c, "get", json!({"id": id})).await;
    assert!(err);
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn con_modelo_avisa_de_duplicados() {
    let Some(p) = Prueba::nueva("duplicados", true) else {
        eprintln!("SALTADO: no está el modelo int8");
        return;
    };
    let c = p.cliente().await;
    let (_, r) = llamar(
        &c,
        "remember",
        json!({"title": "Bug en auth: el token caduca antes de tiempo", "content": "El middleware compara la hora local con la caducidad del token, que va en UTC.", "kind": "gotcha"}),
    )
    .await;
    let id = r["id"].as_str().unwrap().to_string();
    let parecida = json!({"title": "El token de auth caduca demasiado pronto", "content": "El middleware compara la hora local con la caducidad del token en UTC.", "kind": "gotcha"});
    let (err, r) = llamar(&c, "remember", parecida.clone()).await;
    assert!(!err);
    assert_eq!(r["saved"], false);
    assert_eq!(r["duplicate_of"]["id"], id.as_str());
    assert!(r["hint"].as_str().unwrap().contains("force"));
    // El agente decide: con force se guarda.
    let mut forzada = parecida;
    forzada["force"] = json!(true);
    let (_, r) = llamar(&c, "remember", forzada).await;
    assert_eq!(r["saved"], true);
    let (_, r) = llamar(&c, "search", json!({"query": "token caduca"})).await;
    assert!(r[0]["similarity"].is_number());
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn guardar_y_mover_a_otro_proyecto() {
    let p = Prueba::nueva("proyecto", false).unwrap();
    let otro = p.dir.join("otro");
    std::fs::create_dir_all(otro.join(".git")).unwrap();
    let c = p.cliente().await;
    let (err, r) = llamar(
        &c,
        "remember",
        json!({"title": "La otra web usa Astro", "content": "Astro 5.", "kind": "fact", "project": otro.to_str().unwrap()}),
    )
    .await;
    assert!(!err, "{r}");
    assert!(r["project"].as_str().unwrap().ends_with("otro"), "{r}");
    let id = r["id"].as_str().unwrap().to_string();
    // Desde este repo no se ve; al moverla aquí, sí.
    let (_, s) = llamar(&c, "search", json!({"query": "astro"})).await;
    assert!(s.as_array().unwrap().is_empty(), "{s}");
    let (err, _) = llamar(&c, "update", json!({"id": id, "project": p.dir.join("repo").to_str().unwrap()})).await;
    assert!(!err);
    let (_, s) = llamar(&c, "search", json!({"query": "astro"})).await;
    assert_eq!(s.as_array().unwrap().len(), 1, "{s}");
    let (_, g) = llamar(&c, "update", json!({"id": id, "project": ""})).await;
    assert_eq!(g["global"], true, "{g}");
    c.cancel().await.unwrap();
}
