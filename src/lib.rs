//! Omoikane: local memory for AI agents.

pub mod agents;
pub mod backup;
pub mod bpe;
pub mod candidates;
pub mod clock;
pub mod db;
pub mod diary;
pub mod documents;
pub mod drift;
pub mod embed;
pub mod engine;
pub mod environment;
pub mod evidence;
pub mod gemma;
pub mod http;
pub mod import;
pub mod lang;
pub mod links;
pub mod mcp;
pub mod meaning;
pub mod memory;
pub mod models;
pub mod names;
pub mod obsidian;
pub mod okf;
pub mod onboarding;
pub mod owner_view;
pub mod project;
pub mod provenance;
pub mod recall;
pub mod recipes;
pub mod review;
pub mod risk;
pub mod search;
pub mod secrets;
pub mod setup;
pub mod skill;
pub mod sync;
pub mod tags;
pub mod trades;
pub mod tui;
pub mod update;
pub mod vectors;
pub mod verdicts;
pub mod verified;

/// The temporary folder that deletes itself, the same one the tests in `tests/` use.
#[cfg(test)]
#[path = "../tests/common/mod.rs"]
pub(crate) mod common;
