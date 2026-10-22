#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tauri::Manager;
use waw_model_viewer::{
    animation_catalog::{Playback, Selection, Statistics as AnimationStatistics},
    catalog::{AnimationIndex, Catalog, Index, Model, ScanProgress, Thumbnail},
    textures::digest,
};

type SharedCatalog = Arc<Mutex<Option<Catalog>>>;

fn game_root() -> Result<PathBuf, String> {
    let root = if cfg!(debug_assertions) {
        std::env::var_os("WAW_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.local/test-root")
            })
    } else {
        std::env::current_exe()
            .map_err(|error| error.to_string())?
            .parent()
            .ok_or("Cannot locate the app folder.")?
            .to_path_buf()
    };
    if !root.join("CoDWaW.exe").is_file() {
        return Err("Put this executable in your World at War folder, beside CoDWaW.exe, then open it again.".into());
    }
    Ok(root)
}

#[tauri::command]
fn startup(app: tauri::AppHandle, state: tauri::State<SharedCatalog>) -> Result<String, String> {
    let root = game_root()?;
    let cache = app
        .path()
        .app_cache_dir()
        .map_err(|error| error.to_string())?
        .join("models-v1")
        .join(digest(root.to_string_lossy().as_bytes()));
    let location = cache.display().to_string();
    *state.lock().map_err(|error| error.to_string())? = Some(Catalog::new(root, cache));
    Ok(location)
}

// File parsing and IWI decoding run on a worker thread, not the WebView UI thread.
// The mutex serialises disk cache changes with model and texture requests.
async fn with_catalog<T: Send + 'static>(
    state: SharedCatalog,
    work: impl FnOnce(&mut Catalog) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = state.lock().map_err(|error| error.to_string())?;
        work(
            guard
                .as_mut()
                .ok_or("Open the app beside CoDWaW.exe first.")?,
        )
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn scan(
    force: bool,
    progress: tauri::ipc::Channel<ScanProgress>,
    state: tauri::State<'_, SharedCatalog>,
) -> Result<Index, String> {
    with_catalog(state.inner().clone(), move |catalog| {
        catalog.scan_with_progress(force, |update| {
            // A closed window must not turn a successful scan into an error.
            let _ = progress.send(update);
        })
    })
    .await
}

#[tauri::command]
async fn load_model(
    id: String,
    lod: Option<usize>,
    state: tauri::State<'_, SharedCatalog>,
) -> Result<Model, String> {
    with_catalog(state.inner().clone(), move |catalog| catalog.load(&id, lod)).await
}

#[tauri::command]
async fn scan_animations(
    force: bool,
    state: tauri::State<'_, SharedCatalog>,
) -> Result<AnimationIndex, String> {
    let shared = state.inner().clone();
    let (token, mut animations, entries) = with_catalog(shared.clone(), |catalog| {
        Ok(catalog.prepare_animation_scan())
    })
    .await?;
    // Indexing can take seconds. Give thumbnails, textures and model requests
    // access to the live catalogue while this independent snapshot is checked.
    let animations = tauri::async_runtime::spawn_blocking(move || {
        animations.scan_detached(&entries, force)?;
        Ok::<_, String>(animations)
    })
    .await
    .map_err(|error| error.to_string())??;
    with_catalog(shared, move |catalog| {
        catalog.finish_animation_scan(token, animations)
    })
    .await
}

#[tauri::command]
async fn animation_options(
    model: String,
    state: tauri::State<'_, SharedCatalog>,
) -> Result<Selection, String> {
    with_catalog(state.inner().clone(), move |catalog| {
        catalog.animation_options(&model)
    })
    .await
}

#[tauri::command]
async fn load_animation(
    model: String,
    clip: String,
    state: tauri::State<'_, SharedCatalog>,
) -> Result<Playback, String> {
    with_catalog(state.inner().clone(), move |catalog| {
        catalog.load_animation(&model, &clip)
    })
    .await
}

#[tauri::command]
async fn texture_pixels(
    name: String,
    size: u32,
    state: tauri::State<'_, SharedCatalog>,
) -> Result<tauri::ipc::Response, String> {
    let bytes = with_catalog(state.inner().clone(), move |catalog| {
        catalog.pixels(&name, size)
    })
    .await?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
async fn thumbnail(
    id: String,
    state: tauri::State<'_, SharedCatalog>,
) -> Result<Option<String>, String> {
    with_catalog(state.inner().clone(), move |catalog| catalog.thumbnail(&id)).await
}

#[tauri::command]
async fn thumbnails(
    ids: Vec<String>,
    state: tauri::State<'_, SharedCatalog>,
) -> Result<Vec<Thumbnail>, String> {
    with_catalog(state.inner().clone(), move |catalog| {
        catalog.thumbnails(&ids)
    })
    .await
}

#[tauri::command]
async fn save_thumbnail(
    id: String,
    png: String,
    state: tauri::State<'_, SharedCatalog>,
) -> Result<(), String> {
    with_catalog(state.inner().clone(), move |catalog| {
        catalog.save_thumbnail(&id, &png)
    })
    .await
}

#[tauri::command]
async fn hide_model(
    id: String,
    state: tauri::State<'_, SharedCatalog>,
) -> Result<Option<AnimationStatistics>, String> {
    with_catalog(state.inner().clone(), move |catalog| catalog.hide(&id)).await
}

#[tauri::command]
async fn clear_cache(state: tauri::State<'_, SharedCatalog>) -> Result<(), String> {
    with_catalog(state.inner().clone(), Catalog::clear_cache).await
}

#[tauri::command]
async fn open_cache(state: tauri::State<'_, SharedCatalog>) -> Result<(), String> {
    with_catalog(state.inner().clone(), |catalog| {
        std::fs::create_dir_all(&catalog.cache).map_err(|error| error.to_string())?;
        std::process::Command::new("explorer.exe")
            .arg(&catalog.cache)
            .spawn()
            .map_err(|error| error.to_string())?;
        Ok(())
    })
    .await
}

#[tauri::command]
fn quit(app: tauri::AppHandle) {
    app.exit(0);
}

fn main() {
    let profile = tempfile::Builder::new()
        .prefix("waw-model-ui-")
        .tempdir()
        .expect("Cannot create the temporary browser profile");
    let profile_path = profile.path().to_path_buf();
    tauri::Builder::default()
        .manage(Arc::new(Mutex::new(None::<Catalog>)))
        .setup(move |app| {
            tauri::WebviewWindowBuilder::from_config(app, &app.config().app.windows[0])?
                .data_directory(profile_path)
                .build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            startup,
            scan,
            load_model,
            scan_animations,
            animation_options,
            load_animation,
            texture_pixels,
            thumbnail,
            thumbnails,
            save_thumbnail,
            hide_model,
            clear_cache,
            open_cache,
            quit
        ])
        .run(tauri::generate_context!())
        .expect("Cannot start WaW Model Viewer");
}
