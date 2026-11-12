//! Context items: screenshots, links, docs and notes you want an agent to see.
//!
//! The daemon stores each item as a file under `<data_dir>/ctx/` and hands agents the path.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::task::TaskId;

pub type ContextId = u64;

/// Where an item lives on the shelf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    /// Belongs to one task.
    Task { id: TaskId },
    /// Shared by every task in a repo.
    Project { repo: PathBuf },
    /// Loose ideas, not assigned yet.
    Inbox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    Image,
    Link,
    Note,
    File,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextItem {
    pub id: ContextId,
    pub scope: Scope,
    pub kind: ContextKind,
    /// File name, link title or the first line of a note.
    pub title: String,
    /// Where the content is stored on disk.
    pub path: PathBuf,
    /// For links: the URL. For notes: the text. Empty otherwise.
    #[serde(default)]
    pub text: String,
    /// Unix seconds.
    pub created_at: u64,
}
