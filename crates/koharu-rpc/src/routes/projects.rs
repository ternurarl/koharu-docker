//! Project lifecycle, page import, export, and thumbnail routes.

use std::io::{Cursor, Write};

use anyhow::Context as _;
use axum::{
    Json, Router,
    body::Body,
    extract::{Multipart, Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use koharu_renderer::RasterOptions;
use koharu_scene::{EntityId, Snapshot};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::project::{import_images, source_image, ProjectInfo, ProjectSummary};
use crate::state::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListProjectsResponse {
    pub projects: Vec<ProjectSummary>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProjectRequest {
    pub name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenProjectRequest {
    pub name: String,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Rendered,
    Source,
}

impl Default for ExportFormat {
    fn default() -> Self {
        Self::Rendered
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    #[serde(default)]
    pub pages: Option<Vec<EntityId>>,
    #[serde(default)]
    pub format: ExportFormat,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects", get(list_projects).post(create_project))
        .route(
            "/projects/current",
            get(get_current_project).put(open_project).delete(close_project),
        )
        .route("/projects/{name}", delete(delete_project))
        .route("/projects/current/pages", post(import_pages))
        .route("/projects/current/export", post(export_project))
        .route("/pages/{id}/thumbnail", get(get_thumbnail))
}

async fn list_projects(State(app): State<AppState>) -> ApiResult<Json<ListProjectsResponse>> {
    Ok(Json(ListProjectsResponse {
        projects: app.library.list()?,
    }))
}

async fn create_project(
    State(app): State<AppState>,
    Json(req): Json<CreateProjectRequest>,
) -> ApiResult<Json<ProjectInfo>> {
    let project = app.library.create(&req.name).await?;
    let info = project.info()?;
    *app.project.lock().await = Some(project);
    Ok(Json(info))
}

async fn get_current_project(State(app): State<AppState>) -> ApiResult<Json<ProjectInfo>> {
    let guard = app.project.lock().await;
    let project = guard
        .as_ref()
        .ok_or_else(|| ApiError::not_found("no project open"))?;
    Ok(Json(project.info()?))
}

async fn open_project(
    State(app): State<AppState>,
    Json(req): Json<OpenProjectRequest>,
) -> ApiResult<Json<ProjectInfo>> {
    let project = app.library.open(&req.name).await?;
    let info = project.info()?;
    *app.project.lock().await = Some(project);
    Ok(Json(info))
}

async fn close_project(State(app): State<AppState>) -> ApiResult<StatusCode> {
    *app.project.lock().await = None;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_project(
    State(app): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let active = {
        let guard = app.project.lock().await;
        guard.as_ref().is_some_and(|project| project.name == name)
    };
    if active {
        *app.project.lock().await = None;
    }
    app.library.delete(&name)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn import_pages(
    State(app): State<AppState>,
    mut multipart: Multipart,
) -> ApiResult<Json<ProjectInfo>> {
    let mut images = Vec::new();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| ApiError::bad_request(error.to_string()))?
    {
        let name = field
            .file_name()
            .map(str::to_owned)
            .unwrap_or_else(|| "page".to_owned());
        let data = field
            .bytes()
            .await
            .map_err(|error| ApiError::bad_request(error.to_string()))?;
        let format = image::guess_format(data.as_ref())
            .map_err(|error| ApiError::bad_request(error.to_string()))?;
        let (width, height) = image::ImageReader::with_format(Cursor::new(data.as_ref()), format)
            .into_dimensions()
            .map_err(|error| ApiError::bad_request(error.to_string()))?;
        images.push((
            name,
            data.to_vec(),
            format.to_mime_type().to_owned(),
            width,
            height,
        ));
    }
    if images.is_empty() {
        return Err(ApiError::bad_request("no images provided"));
    }
    images.sort_unstable_by(|left, right| left.0.cmp(&right.0));

    let mut guard = app.project.lock().await;
    let project = guard
        .as_mut()
        .ok_or_else(|| ApiError::bad_request("no project open"))?;
    let commit = import_images(&mut project.session, images).await?;
    project.record(vec![commit.revision]);
    project.active_page = project.snapshot().pages().next().map(|page| page.id());
    Ok(Json(project.info()?))
}

async fn export_project(
    State(app): State<AppState>,
    Json(req): Json<ExportRequest>,
) -> ApiResult<Response> {
    let (snapshot, project_name) = {
        let guard = app.project.lock().await;
        let project = guard
            .as_ref()
            .ok_or_else(|| ApiError::bad_request("no project open"))?;
        (project.snapshot(), project.name.clone())
    };
    let page_ids = resolve_pages(&snapshot, req.pages.as_deref())?;
    if page_ids.is_empty() {
        return Err(ApiError::bad_request("no pages in selection"));
    }

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut content_type = "application/octet-stream";
    for (index, &page) in page_ids.iter().enumerate() {
        let (bytes, mime, extension) = match req.format {
            ExportFormat::Source => {
                let bytes = source_image(&snapshot, page).await?;
                let format = image::guess_format(&bytes)
                    .map_err(|error| ApiError::bad_request(error.to_string()))?;
                (bytes, format.to_mime_type(), extension_for(format))
            }
            ExportFormat::Rendered => {
                let frame = app
                    .renderer
                    .render(&snapshot, page)
                    .await
                    .map_err(|error| ApiError::internal(error.into()))?;
                let raster = app
                    .renderer
                    .rasterize(&frame, RasterOptions::default())
                    .await
                    .map_err(|error| ApiError::internal(error.into()))?;
                let bytes = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
                    let mut cursor = Cursor::new(Vec::new());
                    image::DynamicImage::ImageRgba8(raster.image)
                        .write_to(&mut cursor, image::ImageFormat::Png)?;
                    Ok(cursor.into_inner())
                })
                .await
                .map_err(|error| ApiError::internal(error.into()))??;
                (bytes, "image/png", "png")
            }
        };
        content_type = mime;
        files.push((format!("page-{:03}-{page}.{extension}", index + 1), bytes));
    }

    if files.len() == 1 {
        let (filename, bytes) = files.remove(0);
        return Ok(bytes_response(bytes, &filename, content_type));
    }

    let zip = tokio::task::spawn_blocking(move || zip_bytes(files))
        .await
        .map_err(|error| ApiError::internal(error.into()))??;
    let filename = format!("{}.zip", sanitize(&project_name, "project"));
    Ok(bytes_response(zip, &filename, "application/zip"))
}

async fn get_thumbnail(
    State(app): State<AppState>,
    Path(id): Path<EntityId>,
) -> ApiResult<Response> {
    let snapshot = {
        let guard = app.project.lock().await;
        let project = guard
            .as_ref()
            .ok_or_else(|| ApiError::bad_request("no project open"))?;
        project.snapshot()
    };
    snapshot
        .page(id)
        .map_err(|error| ApiError::not_found(format!("page {id}: {error}")))?;
    let bytes = source_image(&snapshot, id).await?;
    let webp_bytes = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let image = image::load_from_memory(&bytes).context("failed to decode source image")?;
        let image = image.thumbnail(128, 128).to_rgba8();
        let encoder = webp::Encoder::from_rgba(image.as_raw(), image.width(), image.height());
        Ok(encoder.encode(80.0).to_vec())
    })
    .await
    .map_err(|error| ApiError::internal(error.into()))??;
    Ok(webp_response(webp_bytes))
}

fn resolve_pages(
    snapshot: &Snapshot,
    requested: Option<&[EntityId]>,
) -> ApiResult<Vec<EntityId>> {
    match requested {
        None => Ok(snapshot.pages().map(|page| page.id()).collect()),
        Some(ids) => {
            for &id in ids {
                snapshot
                    .page(id)
                    .map_err(|error| ApiError::not_found(format!("page {id}: {error}")))?;
            }
            Ok(ids.to_vec())
        }
    }
}

fn extension_for(format: image::ImageFormat) -> &'static str {
    match format {
        image::ImageFormat::Png => "png",
        image::ImageFormat::Jpeg => "jpg",
        image::ImageFormat::WebP => "webp",
        _ => "bin",
    }
}

fn zip_bytes(files: Vec<(String, Vec<u8>)>) -> anyhow::Result<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in files {
            writer.start_file(name, options)?;
            writer.write_all(&bytes)?;
        }
        writer.finish()?;
    }
    Ok(cursor.into_inner())
}

fn sanitize(name: &str, fallback: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-' || *character == '_')
        .collect();
    if cleaned.is_empty() {
        fallback.to_owned()
    } else {
        cleaned
    }
}

fn bytes_response(bytes: Vec<u8>, filename: &str, content_type: &str) -> Response {
    let disposition = format!("attachment; filename=\"{filename}\"");
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    response.into_response()
}

fn webp_response(bytes: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/webp"));
    response.into_response()
}
