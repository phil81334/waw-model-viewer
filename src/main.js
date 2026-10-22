import { Channel, invoke, isTauri } from "@tauri-apps/api/core";
import { PixelCache } from "./pixels.js";
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
let catalogueStatistics = null;
let visible = [];
let selected = null;
let scanBusy = false;
let selectionGeneration = 0;
let cacheGeneration = 0;
let gridFrame = 0;
let thumbnailBusy = false;
let thumbnailQueue = [];
let animationStatistics = null;
let animationSelection = null;
let animationGeneration = 0;
let animationScanGeneration = 0;
let animationScanning = false;
let currentPlayback = null;
let modelReady = false;
let previewBusy = false;
let playableModels = new Set();
const thumbnails = new Map();
const thumbnailErrors = new Set();
const checkedThumbnails = new Set();
const cards = new Map();
const pixels = new PixelCache((name, size) => invoke("texture_pixels", { name, size }));

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
  return pixels.get(name, size);
}

function updateStatistics() {
  const overlay = element("catalogue-statistics");
  overlay.hidden = !element("statistics").checked;
  if (!catalogueStatistics) {
    overlay.textContent = scanBusy
      ? "Scanning models…"
      : "No catalogue loaded.";
    return;
  }
  const stats = catalogueStatistics;
  const count = (value) => value.toLocaleString();
  const eligible =
    stats.total -
    stats.missingMesh -
    stats.missingFiles -
    stats.emptyGeometry -
    stats.other;
  const failed = Math.max(0, eligible - entries.length);
  const lines = [
    `Models found: ${count(stats.total)} · available: ${count(entries.length)}`,
    `Left out: ${count(stats.total - entries.length)}`,
    `Missing meshes: ${count(stats.missingMesh)} · inputs: ${count(stats.missingFiles)}`,
    `Empty geometry: ${count(stats.emptyGeometry)} · other unusable: ${count(stats.other)}`,
    `Failed previews: ${count(failed)}`,
  ];
  if (animationStatistics) {
    const anim = animationStatistics;
    lines.push(
      `Animation files: ${count(anim.total)} · supported: ${count(anim.supportedClips)}`,
      `Unreadable files: ${count(anim.invalidFiles)} · unverified types: ${count(anim.unsupportedFiles)}`,
      `Models with playback: ${count(anim.modelsWithAnimations)}`,
    );
    if (animationSelection) {
      lines.push(
        `This model: ${count(animationSelection.entries.length)} playable · not offered: ${count(animationSelection.statistics.notOffered)}`,
      );
    }
    lines.push("Not offered does not mean a file is broken.");
  } else if (animationScanning) {
    lines.push("", "Checking animations…");
  }
  overlay.textContent = lines.join("\n");
}

function playbackControls(state = null) {
  const loaded = Boolean(state?.name);
  element("animation-play").disabled = !loaded || !state.lastFrame;
  element("animation-play").textContent = state?.playing ? "Pause" : "Play";
  element("animation-seek").disabled = !loaded || !state.lastFrame;
  element("animation-seek").max = String(state?.lastFrame || 0);
  element("animation-seek").value = String(state?.frame || 0);
  element("animation-loop").disabled = !loaded;
  element("animation-loop").checked = Boolean(state?.loop);
  element("animation-root").disabled = !state?.hasRootMotion;
  element("animation-root").checked = Boolean(state?.rootMotion);
  const seconds = loaded ? state.frame / state.fps : 0;
  const duration = loaded ? state.lastFrame / state.fps : 0;
  element("animation-time").textContent = `${seconds.toFixed(2)} / ${duration.toFixed(2)} s`;
}

function resetAnimations() {
  animationGeneration += 1;
  currentPlayback = null;
  animationSelection = null;
  element("animation-search").value = "";
  element("animation-search").disabled = true;
  element("animation-select").replaceChildren();
  option(element("animation-select"), "", "None — bind pose");
  element("animation-select").disabled = true;
  element("animation-info").textContent = animationScanning
    ? "Checking installed animations…"
    : "Select a model to see its supported animations.";
  playbackControls();
  updateStatistics();
}

function filterAnimations() {
  const select = element("animation-select");
  const active = currentPlayback?.clip.name || "";
  const query = element("animation-search").value.trim().toLowerCase();
  select.replaceChildren();
  option(select, "", "None — bind pose");
  for (const entry of animationSelection?.entries || []) {
    // Keep the active clip visible while narrowing the other choices.
    if (entry.name === active || entry.name.toLowerCase().includes(query)) {
      option(select, entry.name, entry.name);
    }
  }
  select.value = active;
}

async function loadAnimationOptions(note = "") {
  if (!selected || !animationStatistics) {
    return;
  }
  const model = selected.name;
  const generation = ++animationGeneration;
  element("animation-info").textContent = "Reading supported animations…";
  try {
    const result = await invoke("animation_options", { model });
    if (generation !== animationGeneration || selected?.name !== model) {
      return;
    }
    animationSelection = result;
    animationStatistics = result.statistics;
    const wasPlayable = playableModels.has(model.toLowerCase());
    if (result.entries.length) {
      playableModels.add(model.toLowerCase());
    } else {
      playableModels.delete(model.toLowerCase());
    }
    if (wasPlayable !== (result.entries.length > 0) && element("animated-only").checked) {
      applyFilters(false);
    }
    element("animation-search").disabled = result.entries.length === 0;
    element("animation-select").disabled = !modelReady || result.entries.length === 0;
    filterAnimations();
    element("animation-info").textContent = note || (
      result.entries.length
        ? `${result.entries.length.toLocaleString()} supported animations. Select one, then press Play.`
        : "No verified animation pairings for this model. Its static preview is still available."
    );
    updateStatistics();
  } catch (error) {
    if (generation === animationGeneration) {
      element("animation-info").textContent = `Animation list unavailable: ${error}`;
    }
  }
}

async function selectAnimation() {
  if (!selected || !modelReady) {
    return;
  }
  const clip = element("animation-select").value;
  const model = selected.name;
  const generation = ++animationGeneration;
  viewer.clearAnimation();
  currentPlayback = null;
  playbackControls();
  if (!clip) {
    viewer.reset();
    element("animation-info").textContent = "Bind pose. Select an animation to preview it.";
    return;
  }
  element("animation-info").textContent = "Loading animation…";
  try {
    const playback = await invoke("load_animation", { model, clip });
    if (generation !== animationGeneration || selected?.name !== model) {
      return;
    }
    viewer.setAnimation(playback);
    // First-person animations can place the item far from its bind pose.
    // Move the camera once, leaving the source pose and playback motion intact.
    viewer.reset();
    currentPlayback = playback;
    element("animation-info").textContent = `${playback.context} · ${playback.clip.fps} FPS. Root motion is optional; loops return to the start.`;
    message("Animation loaded. Press Play or drag the timeline.");
  } catch (error) {
    if (generation === animationGeneration) {
      viewer.clearAnimation();
      element("animation-select").value = "";
      playbackControls();
      // A changed or unreadable input withdraws its backend choices. Refresh
      // the picker and coverage counts while retaining the useful error note.
      await loadAnimationOptions(`Animation unavailable: ${error}`);
    }
  }
}

async function scanAnimations(force) {
  const generation = ++animationScanGeneration;
  animationScanning = true;
  element("animation-info").textContent = "Checking installed animations. Model browsing is ready.";
  updateStatistics();
  try {
    const result = await invoke("scan_animations", { force });
    if (generation !== animationScanGeneration) {
      return;
    }
    animationStatistics = result.statistics;
    playableModels = new Set(result.models.map((name) => name.toLowerCase()));
    element("animated-only").disabled = false;
    element("animated-only").title = "Only models with verified playable animation choices";
    applyFilters(false);
    if (selected) {
      await loadAnimationOptions();
    } else {
      element("animation-info").textContent = "Select a model to see its supported animations.";
    }
  } catch (error) {
    if (generation === animationScanGeneration) {
      element("animated-only").checked = false;
      applyFilters(false);
      element("animation-info").textContent = `Animations unavailable. Static viewing still works. ${error}`;
    }
  } finally {
    if (generation === animationScanGeneration) {
      animationScanning = false;
      updateStatistics();
    }
  }
}

function applyFilters(resetScroll = true) {
  const query = element("search").value.trim().toLowerCase();
  const group = element("group").value;
  const animatedOnly = element("animated-only").checked;
  visible = entries.filter((entry) => {
    if (!entry.name.toLowerCase().includes(query)) {
      return false;
    }
    if (group && entry.group !== group) {
      return false;
    }
    if (animatedOnly && !playableModels.has(entry.name.toLowerCase())) {
      return false;
    }
    return true;
  });
  element("count").textContent =
    `${visible.length.toLocaleString()} of ${entries.length.toLocaleString()} models`;
  if (resetScroll) {
    element("grid-scroll").scrollTop = 0;
  }
  element("empty-grid").textContent = animatedOnly && !animationStatistics
    ? "Checking playable animations…"
    : "No models match these filters.";
  renderGrid();
  updateStatistics();
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
  const shown = new Set(visible.slice(first, last).map((entry) => entry.id));
  for (const [id, card] of cards) {
    if (!shown.has(id)) {
      card.remove();
      cards.delete(id);
    }
  }
  thumbnailQueue = [];
  for (let index = first; index < last; index += 1) {
    const entry = visible[index];
    let card = cards.get(entry.id);
    if (!card) {
      card = document.createElement("button");
      card.className = "model-card";
      card.title = entry.name;
      card.setAttribute("aria-label", `Preview ${entry.name}`);
      const image = document.createElement("img");
      image.alt = "";
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = entry.name;
      const badge = document.createElement("span");
      badge.className = "badge";
      card.append(image, name, badge);
      card.addEventListener("click", () => selectEntry(entry));
      cards.set(entry.id, card);
    }
    card.classList.toggle("selected", selected?.id === entry.id);
    card.style.left = `${(index % columns) * width + 5}px`;
    card.style.top = `${Math.floor(index / columns) * rowHeight + 5}px`;
    card.style.width = `${width - 10}px`;
    // A loading icon is not a model preview. Enable the card after validation.
    card.disabled = !thumbnails.has(entry.id);
    const image = card.children[0];
    const source = thumbnails.get(entry.id) || placeholderIcon;
    if (image.src !== source) {
      image.src = source;
    }
    if (!thumbnails.has(entry.id) && !thumbnailErrors.has(entry.id)) {
      thumbnailQueue.push(entry.id);
    }
    const badge = card.children[2];
    badge.textContent = thumbnails.has(entry.id)
      ? entry.group
      : "Loading preview…";
    const position = index - first;
    if (grid.children[position] !== card) {
      grid.insertBefore(card, grid.children[position] || null);
    }
  }
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
    const statistics = await invoke("hide_model", { id });
    if (statistics) {
      animationStatistics = statistics;
      updateStatistics();
    }
  } catch (error) {
    console.warn("Could not remember the hidden preview", error);
  }
}

function rememberThumbnail(id, uri) {
  thumbnails.delete(id);
  thumbnails.set(id, uri);
  // Retain the visible page even on a large display; evicting visible cards
  // would repeatedly reread their PNGs. Other pages use the disk cache.
  while (thumbnails.size > Math.max(192, cards.size)) {
    const oldest = thumbnails.keys().next().value;
    thumbnails.delete(oldest);
    checkedThumbnails.delete(oldest);
  }
}

async function pumpThumbnails() {
  if (!desktop || scanBusy || previewBusy || thumbnailBusy) {
    return;
  }
  thumbnailBusy = true;
  try {
    while (thumbnailQueue.length > 0 && !scanBusy && !previewBusy) {
      const unchecked = thumbnailQueue.filter((id) => !checkedThumbnails.has(id)).slice(0, 32);
      if (unchecked.length) {
        const generation = cacheGeneration;
        const cached = await invoke("thumbnails", { ids: unchecked });
        if (generation !== cacheGeneration) {
          continue;
        }
        for (const item of cached) {
          if (generation !== cacheGeneration) {
            break;
          }
          checkedThumbnails.add(item.id);
          if (item.png) {
            rememberThumbnail(item.id, item.png);
          } else if (item.error) {
            await excludeEntry(item.id);
          }
        }
        if (generation !== cacheGeneration) {
          continue;
        }
        // One bridge call fills the visible cached cards. Keep their DOM nodes
        // and image decodes instead of rebuilding the whole grid per image.
        renderGrid();
        if (previewBusy) {
          break;
        }
        // A large window can contain more than one batch. Check the next
        // cached batch before deciding that any remaining card needs rendering.
        continue;
      }
      const id = thumbnailQueue.shift();
      if (!id) {
        continue;
      }
      if (thumbnails.has(id) || thumbnailErrors.has(id)) {
        continue;
      }
      const generation = cacheGeneration;
      try {
        const data = await invoke("load_model", { id, lod: null });
        if (generation !== cacheGeneration) {
          continue;
        }
        if (!thumbnailRenderer) {
          thumbnailRenderer = new ThumbnailRenderer();
        }
        const uri = await thumbnailRenderer.render(data, readPixels);
        if (generation !== cacheGeneration) {
          continue;
        }
        await invoke("save_thumbnail", { id, png: uri });
        if (generation === cacheGeneration) {
          rememberThumbnail(id, uri);
        }
      } catch (error) {
        if (generation === cacheGeneration) {
          if (thumbnailRenderer?.contextLost()) {
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
  } catch (error) {
    message(`Could not read cached previews: ${error}`, true);
  } finally {
    thumbnailBusy = false;
  }
}

function clearPreview() {
  selectionGeneration += 1;
  modelReady = false;
  previewBusy = false;
  resetAnimations();
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
  previewBusy = true;
  animationGeneration += 1;
  modelReady = false;
  const savedPlayback = currentPlayback;
  const savedState = viewer.animation?.state();
  viewer.animation?.setPlaying(false);
  playbackControls();
  element("animation-select").disabled = true;
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
    modelReady = true;
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
    // Animation failures must never enter the static-model rejection path.
    if (savedPlayback && currentPlayback === savedPlayback) {
      try {
        viewer.setAnimation(savedPlayback);
        viewer.animation.setLoop(savedState.loop);
        viewer.animation.setRootMotion(savedState.rootMotion);
        viewer.animation.seek(savedState.frame);
        viewer.reset();
        viewer.animation.setPlaying(savedState.playing);
      } catch (error) {
        currentPlayback = null;
        viewer.clearAnimation();
        element("animation-info").textContent = `Animation unavailable at this LOD: ${error}`;
      }
    }
    if (animationSelection) {
      filterAnimations();
      element("animation-select").disabled = !animationSelection.entries.length;
    } else {
      await loadAnimationOptions();
    }
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
  } finally {
    if (generation === selectionGeneration) {
      previewBusy = false;
      pumpThumbnails();
    }
  }
}

async function scan(force) {
  if (!desktop || scanBusy) {
    return;
  }
  scanBusy = true;
  animationScanGeneration += 1;
  animationStatistics = null;
  animationScanning = false;
  playableModels.clear();
  element("animated-only").disabled = true;
  element("animated-only").title = "Available when the animation check finishes";
  cacheGeneration += 1;
  thumbnailQueue = [];
  thumbnails.clear();
  checkedThumbnails.clear();
  pixels.clear();
  cards.clear();
  thumbnailErrors.clear();
  selected = null;
  clearPreview();
  entries = [];
  catalogueStatistics = null;
  updateStatistics();
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
    const progress = new Channel();
    progress.onmessage = (update) => {
      const labels = {
        images: "Reading the image archive index…",
        files: "Checking Modtools files…",
        cache: "Reading the saved catalogue…",
        models: `Checking models… ${update.completed.toLocaleString()} of ${update.total.toLocaleString()}`,
      };
      const text = labels[update.phase];
      if (scanBusy && text) {
        element("empty-grid").textContent = text;
        message(text);
      }
    };
    const index = await invoke("scan", { force, progress });
    entries = index.entries;
    catalogueStatistics = index.statistics;
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
    updateStatistics();
    element("rescan").disabled = false;
    element("clear-cache").disabled = false;
    pumpThumbnails();
  }
  if (catalogueStatistics) {
    await scanAnimations(force);
  }
}

for (const id of ["search", "group"]) {
  element(id).addEventListener(
    id === "search" ? "input" : "change",
    applyFilters,
  );
}
element("reset-filters").addEventListener("click", () => {
  element("animated-only").checked = false;
  for (const id of ["search", "group"]) {
    element(id).value = "";
  }
  applyFilters();
});
element("animated-only").addEventListener("change", () => applyFilters());
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
element("statistics").addEventListener("change", updateStatistics);

element("animation-search").addEventListener("input", filterAnimations);
element("animation-select").addEventListener("change", selectAnimation);
element("animation-play").addEventListener("click", () => {
  const player = viewer.animation;
  if (player) {
    player.setPlaying(!player.state().playing);
  }
});
element("animation-seek").addEventListener("input", () => {
  // Pausing refreshes the controls. Keep the user's new position first.
  const frame = Number(element("animation-seek").value);
  viewer.animation?.setPlaying(false);
  viewer.animation?.seek(frame);
});
element("animation-loop").addEventListener("change", () => {
  viewer.animation?.setLoop(element("animation-loop").checked);
});
element("animation-root").addEventListener("change", () => {
  viewer.animation?.setRootMotion(element("animation-root").checked);
});
window.addEventListener("animation-error", (event) => {
  currentPlayback = null;
  element("animation-select").value = "";
  playbackControls();
  element("animation-info").textContent = `Animation stopped: ${event.detail}`;
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
    viewer.onPlaybackChange = playbackControls;
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
