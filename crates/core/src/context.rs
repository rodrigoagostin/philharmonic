//! Context: screenshots, links, docs and notes you want agents to see.
//!
//! Items live in **collections** — named groups you make for whatever you work on: a whole
//! project, one feature of it, a product spread over several repos, a client… A collection
//! can be linked to repos, so new tasks in those repos use it automatically, and a task can
//! use several collections. Items can also belong to just one task (a screenshot you pasted
//! into its message box).
//!
//! The daemon stores each item as a file under `<data_dir>/ctx/` and hands agents the path.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::task::TaskId;

pub type ContextId = u64;
pub type CollectionId = u64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub id: CollectionId,
    pub name: String,
    /// New tasks in these repos use this collection unless you untick it. Can be empty
    /// (you pick it by hand) or several repos (one product split over repos).
    #[serde(default)]
    pub repos: Vec<PathBuf>,
    /// Unix seconds.
    #[serde(default)]
    pub created_at: u64,
}

/// What an item belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    /// Only this task.
    Task { id: TaskId },
    /// A collection (shared by every task that uses it).
    Collection { id: CollectionId },
    /// Attached in the New task window, before the task exists. Starting the task moves
    /// them to it; leftovers (the window was closed some other way) are deleted at startup.
    Draft,
    /// Older versions had per-repo and "inbox" items. They're turned into collections when
    /// the state is loaded, so nothing else ever sees these two.
    Project { repo: PathBuf },
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
