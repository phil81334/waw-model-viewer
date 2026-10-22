# WaW Model Viewer

**An unofficial community app. Not affiliated with Treyarch, Activision or UGX.**

Browse World at War's Modtools models and find a filename by eye. Click a thumbnail to rotate a textured preview and copy its name. Made by [phil81334](https://github.com/phil81334). Built with JavaScript, Rust, Vite, Tauri and Three.js.

![WaW Model Viewer](images/waw-model-viewer.png)

## Download and run

1. Have World at War and its Modtools installed in the same game folder.
2. Open [Releases](https://github.com/phil81334/waw-model-viewer/releases/latest). Under **Assets**, download **waw-model-viewer.exe**. The source-code ZIP is for development.
3. Put the EXE in the folder containing `CoDWaW.exe`, then double-click it.

You do not need Node.js, Rust or VS Code to use the EXE. Windows needs [WebView2](https://developer.microsoft.com/microsoft-edge/webview2/#download-section), usually already installed; if missing, install its **Evergreen Bootstrapper**.

## Browse models

Search or choose a name group, then click a thumbnail. **With playable animations** shows only models with verified animation choices and works alongside both filters. Name groups use filename prefixes: **Props (anim_)** is a naming group, not a promise of animations. Drag to rotate, wheel to zoom, or right-drag to pan. **Copy name** copies the filename.

**Inspect model** offers LODs, bones with hover names, a checkbox for all bone names, and a mesh outline. Simple lighting is off by default.

**Animation** lists verified choices for the selected model. Search by filename, choose a clip, then **Play** or **Pause**. Drag the timeline to seek. **Loop** repeats the clip; **Root motion** adds its stored movement and starts from the model's origin. Loops return to the start. LOD changes keep the current playback position.

Choosing a clip centres its first pose. **Reset view** frames the current pose without restarting playback. Movement can still take a model out of view; some weapon clips only move hands or attachments that this preview does not show.

Choices use the installed weapon definitions, scripts and compatible character rigs. Unsupported types, incomplete rigs and unverified pairings are left out. This previews one clip at a time; it does not recreate gameplay, blending, animation events or procedural effects. Models without supported animations can still be browsed.

**Statistics** shows the models found, available and left out, with reasons. The test installation has **6,109 models: 5,979 available, 130 left out**. Your counts come from your files. Available means the primary mesh and colour inputs passed basic checks; previews are then checked as you browse.

Animation statistics show files found, unreadable or unverified files, models with playback and choices for the selected model. A clip not offered for a model is not necessarily broken. Counts come from your installed files.

Each material's colour image and stored vertex colour are shown. Normal/specular maps, game lighting, glow and other shader effects are not recreated, so `_on` variants may look alike. No exporting. Missing inputs, empty geometry and failed previews are hidden. Game files are never changed.

The model list, thumbnails and animation index are cached in AppData automatically. Startup shows its scan stage and a model counter when rebuilding the list. Thumbnails load as you browse; animation indexing runs separately, and the playback filter becomes available once checked. **Cache…** shows the folder and lets you clear it. **Rescan** rebuilds the catalogue, clears cached previews and retries hidden models.

## Run from source

**Windows setup — do this once.** Skip anything already installed.

| Install | What to choose |
| --- | --- |
| [VS Code](https://code.visualstudio.com/) | The Windows installer. This is where you open the project. |
| [Node.js](https://nodejs.org/en/download) | **LTS**, **Windows**, **Installer (.msi)**. Keep the default options; npm is included. |
| [Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) | In its installer, select **Desktop development with C++**. Keep the selected MSVC tools and Windows SDK. |
| [Rust](https://rust-lang.org/tools/install/) | Download and run **rustup-init.exe (x64)**. Choose the default installation. |

Close and reopen VS Code after installing these. [Tauri's Windows guide](https://v2.tauri.app/start/prerequisites/#windows) has more help. Vite and Tauri are installed by the project; no separate installation or Python is needed.

**Open and run the project:**

1. On this GitHub page, choose **Code → Download ZIP**, then extract it. You can clone it instead if you already use Git.
2. In VS Code, choose **File → Open Folder**. Open the extracted folder containing `package.json`.
3. In VS Code's Explorer, create a folder named `.local`, then a file inside it named `game-path.txt`.
4. Open your WaW game folder in Windows File Explorer—the one containing `CoDWaW.exe`. Click the address bar, copy the full folder address, paste it into `game-path.txt` and save. Use your actual folder, on whichever drive you installed WaW. One line, no quotes, no EXE filename.
5. Choose **Terminal → New Terminal**, then run these commands one at a time:

   ```powershell
   npm install
   npm run dev
   ```

`npm install` downloads the project's dependencies. `npm run dev` opens the desktop app. The first run downloads and compiles Rust dependencies, so give it time. Keep the terminal open; press **Ctrl+C** there to stop development.

Next time, run `npm run dev`, or click the play button beside **dev** in VS Code's **NPM Scripts** panel. If that panel is hidden, right-click the Explorer panel headings and enable **NPM Scripts**. Update `game-path.txt` if you move WaW. The packaged EXE always uses its own folder instead.

| Command | What it does |
| --- | --- |
| `npm run dev` | Opens the desktop app for development. |
| `npm run dev:web` | Shows the UI in a browser. Cannot read game models. |
| `npm run build` | Creates `release/waw-model-viewer.exe`. |
| `npm run build:web` | Builds the UI files only. No EXE. |

To make your own EXE, run `npm run build`, then copy `release/waw-model-viewer.exe` beside `CoDWaW.exe`.

[Credits](ATTRIBUTION.md) · [GPL-3.0 licence](LICENCE)
