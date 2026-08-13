//! Headless project management over koharu_scene::Session.
//!
//! Mirrors the desktop app's project layer without the Tauri/Desktop coupling:
//! projects live in <data>/projects/<name>.khrproj, each backed by a
//! koharu_scene::Session.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use koharu_scene::{
    AssetInput, AssetMetadata, AssetRole, At, Commit, EntityId, PageDraft, Revision, Session,
    Snapshot,
};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageSummary {
    pub id: EntityId,
    pub label: String,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    pub name: String,
    pub revision: Revision,
    pub active_page: Option<EntityId>,
    pub pages: Vec<PageSummary>,
}

/// Managed project directory tree rooted at a configurable path.
pub struct ProjectLibrary {
    root: PathBuf,
}

impl ProjectLibrary {
    pub fn new(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root)
            .with_context(|| format!("failed to create {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn list(&self) -> Result<Vec<ProjectSummary>> {
        let mut projects = std::fs::read_dir(&self.root)
            .with_context(|| format!("failed to read {}", self.root.display()))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| {
                let path = entry.path();
                let is_project = path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("khrproj"))
                    && (path.join("state-a.khr").is_file() || path.join("state-b.khr").is_file());
                if !is_project {
                    return None;
                }
                Some(ProjectSummary {
                    name: path.file_stem()?.to_str()?.to_owned(),
                })
            })
            .collect::<Vec<_>>();
        projects.sort_unstable_by_key(|project| project.name.to_lowercase());
        Ok(projects)
    }

    pub async fn create(&self, name: &str) -> Result<Project> {
        let (name, path) = self.resolve(name)?;
        Project::create(name, path).await
    }

    pub async fn open(&self, name: &str) -> Result<Project> {
        let (name, path) = self.resolve(name)?;
        Project::open(name, path).await
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        let (_, path) = self.resolve(name)?;
        if !path.is_dir() {
            bail!("project {name:?} does not exist");
        }
        std::fs::remove_dir_all(&path)
            .with_context(|| format!("failed to delete {}", path.display()))
    }

    fn resolve(&self, name: &str) -> Result<(String, PathBuf)> {
        let name = validate_project_name(name)?;
        Ok((name.clone(), self.root.join(format!("{name}.khrproj"))))
    }
}

/// One open project: its scene session plus lightweight UI history state.
pub struct Project {
    pub session: Session,
    pub name: String,
    pub active_page: Option<EntityId>,
    pub undo: Vec<Vec<Revision>>,
    pub redo: Vec<Vec<Revision>>,
}

impl Project {
    pub async fn create(name: String, path: PathBuf) -> Result<Self> {
        let session = Session::create(&path)
            .await
            .with_context(|| format!("failed to create {}", path.display()))?;
        Ok(Self::new(session, name))
    }

    pub async fn open(name: String, path: PathBuf) -> Result<Self> {
        let session = Session::open(&path)
            .await
            .with_context(|| format!("failed to open {}", path.display()))?;
        Ok(Self::new(session, name))
    }

    fn new(session: Session, name: String) -> Self {
        let active_page = session.snapshot().pages().next().map(|page| page.id());
        Self {
            session,
            name,
            active_page,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.session.snapshot()
    }

    pub fn revision(&self) -> Revision {
        self.snapshot().revision()
    }

    pub fn info(&self) -> Result<ProjectInfo> {
        Ok(ProjectInfo {
            name: self.name.clone(),
            revision: self.revision(),
            active_page: self.active_page,
            pages: Self::pages(&self.snapshot())?,
        })
    }

    pub fn record(&mut self, revisions: Vec<Revision>) {
        if !revisions.is_empty() {
            self.undo.push(revisions);
            self.redo.clear();
        }
    }

    pub fn pages(snapshot: &Snapshot) -> Result<Vec<PageSummary>> {
        snapshot
            .pages()
            .map(|page| {
                let value = page.page()?;
                Ok(PageSummary {
                    id: page.id(),
                    label: value.label,
                    width: value.width,
                    height: value.height,
                })
            })
            .collect()
    }
}

/// Import raster images as new pages at the end of the project.
pub async fn import_images(
    session: &mut Session,
    images: Vec<(String, Vec<u8>, String, u32, u32)>,
) -> Result<Commit> {
    let snapshot = session.snapshot();
    let source = AssetRole::new("source")?;
    let patch = snapshot.patch(|edit| {
        for (name, bytes, media_type, width, height) in images {
            let page = edit.add_page(
                PageDraft::new(name, f64::from(width), f64::from(height)),
                At::End,
            )?;
            edit.set_asset(
                page,
                &source,
                AssetInput::new(
                    bytes,
                    media_type,
                    AssetMetadata {
                        width: Some(width),
                        height: Some(height),
                        attributes: Default::default(),
                    },
                ),
            )?;
        }
        Ok(())
    })?;
    Ok(session.commit(patch).await?)
}

pub fn validate_project_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("project name cannot be empty");
    }
    if name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|character| character.is_control() || r#"<>:"/\|?*"#.contains(character))
    {
        bail!("project name contains characters that cannot be used in a file name");
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|number| {
                matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
    {
        bail!("project name is reserved by Windows");
    }
    Ok(name.to_owned())
}

/// Resolve a page's source image bytes (for thumbnails / downloads).
pub async fn source_image(snapshot: &Snapshot, page: EntityId) -> Result<Vec<u8>> {
    let asset = snapshot
        .asset(page, &AssetRole::new("source")?)?
        .with_context(|| format!("page {page} has no source image"))?;
    Ok(snapshot.read_blob(asset.blob).await?.to_vec())
}
