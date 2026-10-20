import { invoke, isTauri } from "@tauri-apps/api/core";
import {
  ModelViewer,
  ThumbnailRenderer,
  buildModel,
  disposeObject,
} from "./viewer.js";

const element = (id) => document.getElementById(id);
const desktop = isTauri();
const placeholderIcon = new URL("../src-tauri/icons/icon.png", import.meta.url)
  .href;
let viewer;
let thumbnailRenderer;
let entries = [];
let visible = [];
let selected = null;
let scanBusy = false;
let selectionGeneration = 0;
let cacheGeneration = 0;
let gridFrame = 0;
let thumbnailBusy = false;
let thumbnailQueue = [];
const thumbnails = new Map();
const thumbnailErrors = new Set();

function message(text, error = false) {
  element("message").textContent = text;
  element("message").className = error ? "error" : "success";
}

function option(select, value, label) {
  const item = document.createElement("option");
  item.value = value;
  item.textContent = label;
  select.append(item);
}

function readPixels(name, size) {
  return invoke("texture_pixels", { name, size });
}

function applyFilters(resetScroll = true) {
  const query = element("search").value.trim().toLowerCase();
  const group = element("group").value;
  visible = entries.filter((entry) => {
    if (!entry.name.toLowerCase().includes(query)) {
      return false;
    }
    if (group && entry.group !== group) {
      return false;
    }
    return true;
  });
  element("count").textContent =
    `${visible.length.toLocaleString()} of ${entries.length.toLocaleString()} models`;
  if (resetScroll) {
    element("grid-scroll").scrollTop = 0;
  }
  element("empty-grid").textContent = "No models match these filters.";
  renderGrid();
}

function scheduleGrid() {
  if (gridFrame) {
    return;
  }
  gridFrame = requestAnimationFrame(() => {
    gridFrame = 0;
    renderGrid();
  });
}

function renderGrid() {
  const scroll = element("grid-scroll");
  const grid = element("grid");
  const columns = Math.max(1, Math.floor(scroll.clientWidth / 180));
  const width = scroll.clientWidth / columns;
  const rowHeight = 196;
  const first =
    Math.max(0, Math.floor(scroll.scrollTop / rowHeight) - 1) * columns;
  const last = Math.min(
    visible.length,
    (Math.ceil((scroll.scrollTop + scroll.clientHeight) / rowHeight) + 1) *
      columns,
  );
  grid.style.height = `${Math.ceil(visible.length / columns) * rowHeight}px`;
  element("empty-grid").hidden = visible.length > 0;
  const cards = document.createDocumentFragment();
  thumbnailQueue = [];
  for (let index = first; index < last; index += 1) {
    const entry = visible[index];
    const card = document.createElement("button");
    card.className = "model-card";
    card.classList.toggle("selected", selected?.id === entry.id);
    card.style.left = `${(index % columns) * width + 5}px`;
    card.style.top = `${Math.floor(index / columns) * rowHeight + 5}px`;
    card.style.width = `${width - 10}px`;
    card.title = entry.name;
    // A loading icon is not a model preview. Enable the card after validation.
    card.disabled = !thumbnails.has(entry.id);
    card.setAttribute("aria-label", `Preview ${entry.name}`);
    const image = document.createElement("img");
    image.alt = "";
    image.src = thumbnails.get(entry.id) || placeholderIcon;
    if (!thumbnails.has(entry.id) && !thumbnailErrors.has(entry.id)) {
      thumbnailQueue.push(entry.id);
    }
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = entry.name;
    const badge = document.createElement("span");
    badge.className = "badge";
    badge.textContent = thumbnails.has(entry.id)
      ? entry.group
      : "Loading preview…";
    card.append(image, name, badge);
    card.addEventListener("click", () => selectEntry(entry));
    cards.append(card);
  }
  grid.replaceChildren(cards);
  pumpThumbnails();
}

async function excludeEntry(id) {
  thumbnailErrors.add(id);
  thumbnails.delete(id);
  entries = entries.filter((entry) => entry.id !== id);
  if (selected?.id === id) {
    selected = null;
    clearPreview();
    element("filename").textContent = "Model preview";
    element("copy-name").disabled = true;
  }
  applyFilters(false);
  try {
    await invoke("hide_model", { id });
  } catch (error) {
    console.warn("Could not remember the hidden preview", error);
  }
}

function rememberThumbnail(id, uri) {
  thumbnails.delete(id);
  thumbnails.set(id, uri);
  // Keep a small working set of PNG strings. The rest can be read from disk.
  while (thumbnails.size > 192) {
    thumbnails.delete(thumbnails.keys().next().value);
  }
}

async function pumpThumbnails() {
  if (!desktop || scanBusy || thumbnailBusy) {
    return;
  }
  thumbnailBusy = true;
  try {
    while (thumbnailQueue.length > 0 && !scanBusy) {
      const id = thumbnailQueue.shift();
      if (thumbnails.has(id) || thumbnailErrors.has(id)) {
        continue;
      }
      const generation = cacheGeneration;
      try {
        let uri = await invoke("thumbnail", { id });
        if (generation !== cacheGeneration) {
          continue;
        }
        if (!uri) {
          const data = await invoke("load_model", { id, lod: null });
          if (generation !== cacheGeneration) {
            continue;
          }
          uri = await thumbnailRenderer.render(data, readPixels);
          if (generation !== cacheGeneration) {
            continue;
          }
          await invoke("save_thumbnail", { id, png: uri });
        }
        if (generation === cacheGeneration) {
          rememberThumbnail(id, uri);
        }
      } catch (error) {
        if (generation === cacheGeneration) {
          if (thumbnailRenderer.contextLost()) {
            message(
              "The graphics context was lost. Close and reopen the app.",
              true,
            );
            thumbnailQueue = [];
            break;
          }
          await excludeEntry(id);
          console.warn("Thumbnail unavailable", id, error);
        }
      }
      renderGrid();
      // Let clicks, search and scrolling run between offscreen model renders.
      await new Promise(requestAnimationFrame);
    }
  } finally {
    thumbnailBusy = false;
  }
}

function clearPreview() {
  selectionGeneration += 1;
  viewer?.clear();
  element("reset-view").disabled = true;
  element("lod").disabled = true;
  element("bones").disabled = true;
  element("bone-names").disabled = true;
  element("mesh-outline").disabled = true;
  element("model-stats").textContent = "";
  element("model-warnings").hidden = true;
  element("preview-empty").hidden = false;
}

function selectEntry(entry) {
  selected = entry;
  clearPreview();
  element("filename").textContent = entry.name;
  element("copy-name").disabled = false;
  element("lod").replaceChildren();
  scheduleGrid();
  loadSelection(null);
}

async function loadSelection(lod) {
  if (!selected || !desktop) {
    return;
  }
  const entry = selected;
  const generation = ++selectionGeneration;
  element("preview-empty").hidden = false;
  element("preview-empty").textContent = "Loading model…";
  element("model-info").textContent = "Reading model and colour textures…";
  element("status").textContent = "Loading…";
  element("lod").disabled = true;
  element("reset-view").disabled = true;
  element("bones").disabled = true;
  element("bone-names").disabled = true;
  element("mesh-outline").disabled = true;
  try {
    const data = await invoke("load_model", { id: entry.id, lod });
    if (generation !== selectionGeneration) {
      return;
    }
    const built = await buildModel(
      data,
      readPixels,
      2048,
      element("lighting").checked,
    );
    if (generation !== selectionGeneration) {
      disposeObject(built.group);
      return;
    }
    viewer.setModel(built.group, data);
    viewer.setLighting(element("lighting").checked);
    element("preview-empty").hidden = true;
    element("reset-view").disabled = false;
    element("lod").replaceChildren();
    for (const slot of data.lods) {
      option(
        element("lod"),
        String(slot),
        `LOD ${slot}${slot === 0 ? " — most detail" : ""}`,
      );
    }
    element("lod").value = String(data.lod);
    element("lod").disabled = data.lods.length < 2;
    element("bones").disabled = data.bones.length === 0;
    element("bone-names").disabled =
      !element("bones").checked || data.bones.length === 0;
    viewer.showBones(element("bones").checked);
    viewer.showBoneNames(element("bone-names").checked);
    element("mesh-outline").disabled = false;
    viewer.showMeshOutline(element("mesh-outline").checked);
    const triangles = data.surfaces.reduce(
      (count, surface) => count + surface.indices.length / 3,
      0,
    );
    element("model-info").textContent =
      `${entry.group} · Colour-textured preview`;
    const boneLabel = data.bones.length === 1 ? "bone" : "bones";
    element("model-stats").textContent =
      `${triangles.toLocaleString()} triangles · ${data.surfaces.length} surfaces · ${data.bones.length} ${boneLabel}`;
    const notes = [...built.warnings];
    const fallbackCount = data.surfaces.filter(
      (surface) => surface.material.fallback,
    ).length;
    if (fallbackCount > 0) {
      notes.push(
        `${fallbackCount} special material(s) shown using a basic colour-only preview.`,
      );
    }
    element("model-warnings").hidden = notes.length === 0;
    element("model-warning-title").textContent =
      `Preview notes (${notes.length})`;
    element("model-warning-text").textContent = notes.join("\n");
    message(
      notes.length > 0
        ? "Model loaded. See Preview notes for missing files or simplified materials."
        : "Model loaded. Drag to rotate, wheel to zoom, or right-drag to pan.",
    );
    element("status").textContent = "Done";
  } catch (error) {
    if (generation !== selectionGeneration) {
      return;
    }
    viewer.clear();
    await excludeEntry(entry.id);
    if (selected) {
      return;
    }
    element("model-warnings").hidden = true;
    element("preview-empty").textContent =
      "Select another model to preview it.";
    element("model-info").textContent = "This model could not be loaded.";
    message(
      "This model could not be previewed and has been hidden. Rescan retries it.",
      true,
    );
    element("status").textContent = "Done";
  }
}

async function scan(force) {
  if (!desktop || scanBusy) {
    return;
  }
  scanBusy = true;
  cacheGeneration += 1;
  thumbnailQueue = [];
  thumbnails.clear();
  thumbnailErrors.clear();
  selected = null;
  clearPreview();
  entries = [];
  visible = [];
  element("grid").replaceChildren();
  element("grid").style.height = "0px";
  element("empty-grid").hidden = false;
  element("empty-grid").textContent = "Reading the Modtools catalogue…";
  element("filename").textContent = "Model preview";
  element("copy-name").disabled = true;
  element("preview-empty").textContent = "Select a model to preview it.";
  element("model-info").textContent =
    "Select a thumbnail to look around a model.";
  element("rescan").disabled = true;
  element("clear-cache").disabled = true;
  element("status").textContent = "Scanning…";
  message("Reading the model catalogue…");
  try {
    const index = await invoke("scan", { force });
    entries = index.entries;
    element("cache-path").value = index.cachePath;
    const previous = element("group").value;
    element("group").replaceChildren();
    option(element("group"), "", "All groups");
    for (const group of [
      ...new Set(entries.map((entry) => entry.group)),
    ].sort()) {
      option(element("group"), group, group);
    }
    element("group").value = previous;
    if (element("group").selectedIndex < 0) {
      element("group").value = "";
    }
    applyFilters();
    message(
      `${entries.length.toLocaleString()} models indexed. Thumbnails load as you browse.`,
    );
    element("status").textContent = "Ready";
  } catch (error) {
    element("count").textContent = "No catalogue loaded";
    element("empty-grid").textContent =
      "Could not read models. See the message below, then click Rescan.";
    message(String(error), true);
    element("status").textContent = "Done";
  } finally {
    scanBusy = false;
    element("rescan").disabled = false;
    element("clear-cache").disabled = false;
    pumpThumbnails();
  }
}

for (const id of ["search", "group"]) {
  element(id).addEventListener(
    id === "search" ? "input" : "change",
    applyFilters,
  );
}
element("reset-filters").addEventListener("click", () => {
  for (const id of ["search", "group"]) {
    element(id).value = "";
  }
  applyFilters();
});
element("grid-scroll").addEventListener("scroll", scheduleGrid);
new ResizeObserver(scheduleGrid).observe(element("grid-scroll"));
element("rescan").addEventListener("click", () => scan(true));
element("reset-view").addEventListener("click", () => viewer.reset());
element("lighting").addEventListener("change", () =>
  viewer.setLighting(element("lighting").checked),
);
element("lod").addEventListener("change", () =>
  loadSelection(Number(element("lod").value)),
);
element("bones").addEventListener("change", () => {
  const enabled = element("bones").checked;
  viewer.showBones(enabled);
  element("bone-names").disabled = !enabled;
});
element("bone-names").addEventListener("change", () => {
  viewer.showBoneNames(element("bone-names").checked);
});
element("mesh-outline").addEventListener("change", () => {
  viewer.showMeshOutline(element("mesh-outline").checked);
});
element("copy-name").addEventListener("click", async () => {
  if (!selected) {
    return;
  }
  try {
    await navigator.clipboard.writeText(selected.name);
    message("Model name copied.");
    element("status").textContent = "Done";
  } catch (error) {
    message(`Could not copy the model name: ${error}`, true);
  }
});
element("about").addEventListener("click", () => {
  element("about-dialog").showModal();
  element("repo-url").focus();
  element("repo-url").select();
});
element("repo-url").addEventListener("click", (event) => event.target.select());
element("close-about").addEventListener("click", () =>
  element("about-dialog").close(),
);
element("cache-button").addEventListener("click", () =>
  element("cache-dialog").showModal(),
);
element("close-cache").addEventListener("click", () =>
  element("cache-dialog").close(),
);
element("open-cache").addEventListener("click", async () => {
  try {
    await invoke("open_cache");
  } catch (error) {
    message(String(error), true);
  }
});
element("clear-cache").addEventListener("click", async () => {
  if (scanBusy || !desktop) {
    return;
  }
  element("cache-dialog").close();
  await scan(true);
});
element("startup-error").addEventListener("cancel", (event) =>
  event.preventDefault(),
);
element("quit").addEventListener("click", () => invoke("quit"));
window.addEventListener("viewer-error", (event) => message(event.detail, true));

async function start() {
  let folderValidated = false;
  try {
    if (desktop) {
      element("cache-path").value = await invoke("startup");
    }
    folderValidated = true;
    element("app").hidden = false;
    element("footer").hidden = false;
    element("environment").textContent = import.meta.env.DEV
      ? "Development"
      : "Production";
    viewer = new ModelViewer(element("viewport"));
    thumbnailRenderer = new ThumbnailRenderer();
    if (desktop) {
      await scan(false);
    } else {
      element("count").textContent = "Browser UI preview";
      element("empty-grid").textContent =
        "Run the desktop app to browse your Modtools models.";
      element("rescan").disabled = true;
      element("cache-button").disabled = true;
      message(
        "Browser UI preview. Use npm run dev to open the desktop app with game files.",
      );
    }
  } catch (error) {
    element("app").hidden = true;
    element("footer").hidden = true;
    if (folderValidated) {
      element("startup-title").textContent = "Could not open the viewer";
    }
    element("startup-message").textContent = String(error);
    element("startup-error").showModal();
  }
}

start();
