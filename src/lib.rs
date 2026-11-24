//! Omoikane: local memory for AI agents.

pub mod actualizar;
pub mod agentes;
pub mod bpe;
pub mod buscar;
pub mod candidatas;
pub mod copia;
pub mod db;
pub mod deriva;
pub mod diario;
pub mod documentos;
pub mod embeber;
pub mod entorno;
pub mod enlaces;
pub mod etiquetas;
pub mod evidencia;
pub mod gemma;
pub mod http;
pub mod idioma;
pub mod importar;
pub mod llegada;
pub mod mcp;
pub mod memoria;
pub mod modelos;
pub mod nombres;
pub mod obsidian;
pub mod okf;
pub mod oficios;
pub mod procedencia;
pub mod proyecto;
pub mod engine;
pub mod recall;
pub mod recetas;
pub mod reloj;
pub mod responsable;
pub mod revisar;
pub mod riesgo;
pub mod secretos;
pub mod setup;
pub mod skill;
pub mod significado;
pub mod sync;
pub mod tui;
pub mod vectores;
pub mod verificado;
pub mod veredictos;

/// The temporary folder that deletes itself, the same one the tests in `tests/` use.
#[cfg(test)]
#[path = "../tests/comun/mod.rs"]
pub(crate) mod comun;
