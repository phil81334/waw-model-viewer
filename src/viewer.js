import * as THREE from "three";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";

const side = {
  Back: THREE.FrontSide,
  Front: THREE.BackSide,
  None: THREE.DoubleSide,
};
const factors = {
  One: THREE.OneFactor,
  Zero: THREE.ZeroFactor,
  SrcAlpha: THREE.SrcAlphaFactor,
  InvSrcAlpha: THREE.OneMinusSrcAlphaFactor,
  SrcColor: THREE.SrcColorFactor,
  InvSrcColor: THREE.OneMinusSrcColorFactor,
  DestAlpha: THREE.DstAlphaFactor,
  InvDestAlpha: THREE.OneMinusDstAlphaFactor,
  DestColor: THREE.DstColorFactor,
  InvDestColor: THREE.OneMinusDstColorFactor,
};

function boneLabel(bone, index) {
  // Some stock skeletons store an empty name for their root bone. Identify it
  // in the UI without inventing an asset name or changing the source data.
  if (bone.name.trim() === "") {
    return `Unnamed bone ${index + 1}`;
  }
  return bone.name;
}

function textureFromBytes(value) {
  const bytes = new Uint8Array(value);
  const header = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const width = header.getUint32(0, true);
  const height = header.getUint32(4, true);
  if (bytes.byteLength !== 8 + width * height * 4) {
    throw new Error("Incomplete texture pixels.");
  }
  const texture = new THREE.DataTexture(bytes.subarray(8), width, height);
  texture.colorSpace = THREE.SRGBColorSpace;
  // The IWI decoder's top-to-bottom rows already match the stored model UVs.
  texture.flipY = false;
  texture.wrapS = texture.wrapT = THREE.RepeatWrapping;
  texture.magFilter = THREE.LinearFilter;
  texture.minFilter = THREE.LinearMipmapLinearFilter;
  texture.generateMipmaps = true;
  texture.needsUpdate = true;
  return texture;
}

function previewMaterial(settings, texture, lighting) {
  const options = { map: texture, color: 0xffffff, vertexColors: true };
  const material = lighting
    ? new THREE.MeshLambertMaterial(options)
    : new THREE.MeshBasicMaterial(options);
  material.side = side[settings.cull] ?? THREE.FrontSide;
  material.depthWrite = settings.depthWrite;
  material.forceSinglePass = true;
  material.transparent = settings.blendOp !== "Disable";
  if (material.transparent) {
    material.blending = THREE.CustomBlending;
    material.blendEquation =
      {
        Add: THREE.AddEquation,
        Subtract: THREE.SubtractEquation,
        RevSubtract: THREE.ReverseSubtractEquation,
        Min: THREE.MinEquation,
        Max: THREE.MaxEquation,
      }[settings.blendOp] ?? THREE.AddEquation;
    material.blendSrc = factors[settings.sourceBlend] ?? THREE.SrcAlphaFactor;
    material.blendDst =
      factors[settings.destinationBlend] ?? THREE.OneMinusSrcAlphaFactor;
    material.blendEquationAlpha = THREE.AddEquation;
    material.blendSrcAlpha = THREE.OneMinusDstAlphaFactor;
    material.blendDstAlpha = THREE.OneFactor;
  }
  if (settings.alpha !== "Always") {
    material.alphaTest = 128 / 255;
    // Three's standard cutoff represents GE128. Native GT0/LT128 require
    // their actual comparison, rather than an arbitrary near-zero threshold.
    if (settings.alpha === "GT0" || settings.alpha === "LT128") {
      material.onBeforeCompile = (shader) => {
        const condition =
          settings.alpha === "GT0"
            ? "diffuseColor.a <= 0.0"
            : "diffuseColor.a >= 128.0 / 255.0";
        shader.fragmentShader = shader.fragmentShader.replace(
          "#include <alphatest_fragment>",
          `if (${condition}) { discard; }`,
        );
      };
      material.customProgramCacheKey = () => settings.alpha;
    }
  }
  return material;
}

// Each preview owns its GPU resources. Reusing one thumbnail renderer avoids
// thousands of WebGL contexts, and disposal bounds GPU memory while scrolling.
export function disposeObject(group) {
  const geometries = new Set();
  const materials = new Set();
  const textures = new Set();
  group.traverse((object) => {
    if (object.geometry) {
      geometries.add(object.geometry);
    }
    if (object.material) {
      materials.add(object.material);
      if (object.material.map) {
        textures.add(object.material.map);
      }
    }
  });
  for (const value of geometries) {
    value.dispose();
  }
  for (const value of materials) {
    value.dispose();
  }
  for (const value of textures) {
    value.dispose();
  }
}

export async function buildModel(data, readPixels, size, lighting) {
  const group = new THREE.Group();
  const textures = new Map();
  const warnings = [...data.warnings];
  for (const surface of data.surfaces) {
    const geometry = new THREE.BufferGeometry();
    geometry.setAttribute(
      "position",
      new THREE.Float32BufferAttribute(surface.positions, 3),
    );
    geometry.setAttribute(
      "normal",
      new THREE.Float32BufferAttribute(surface.normals, 3),
    );
    geometry.setAttribute(
      "uv",
      new THREE.Float32BufferAttribute(surface.uvs, 2),
    );
    geometry.setAttribute(
      "color",
      new THREE.Uint8BufferAttribute(surface.colours, 4, true),
    );
    geometry.setIndex(new THREE.Uint16BufferAttribute(surface.indices, 1));
    let texture = null;
    const image = surface.material.image;
    if (image) {
      if (!textures.has(image)) {
        try {
          textures.set(image, textureFromBytes(await readPixels(image, size)));
        } catch (error) {
          geometry.dispose();
          disposeObject(group);
          throw new Error(`Could not read colour image ${image}: ${error}`);
        }
      }
      texture = textures.get(image);
    }
    const mesh = new THREE.Mesh(
      geometry,
      previewMaterial(surface.material, texture, lighting),
    );
    mesh.userData.settings = surface.material;
    group.add(mesh);
  }
  return { group, warnings: [...new Set(warnings)] };
}

function scene() {
  const value = new THREE.Scene();
  value.background = new THREE.Color("#161e25");
  value.add(new THREE.AmbientLight(0xffffff, 0.65));
  const key = new THREE.DirectionalLight(0xffffff, 0.85);
  key.position.set(1, -1, 2);
  value.add(key);
  const fill = new THREE.DirectionalLight(0xadc8ef, 0.3);
  fill.position.set(-1, 1, 0.5);
  value.add(fill);
  return value;
}

function hasVisiblePixels(renderer) {
  const context = renderer.getContext();
  if (context.isContextLost()) {
    throw new Error("The graphics context was lost.");
  }
  const width = context.drawingBufferWidth;
  const height = context.drawingBufferHeight;
  const pixels = new Uint8Array(width * height * 4);
  context.readPixels(
    0,
    0,
    width,
    height,
    context.RGBA,
    context.UNSIGNED_BYTE,
    pixels,
  );
  // Fitting leaves the corners clear. Ignore tiny colour rounding differences
  // when checking whether the model actually changed any background pixels.
  for (let offset = 4; offset < pixels.length; offset += 4) {
    for (let channel = 0; channel < 3; channel += 1) {
      if (Math.abs(pixels[offset + channel] - pixels[channel]) > 2) {
        return true;
      }
    }
  }
  return false;
}

function oppositeView(camera, group) {
  const centre = new THREE.Box3()
    .setFromObject(group)
    .getCenter(new THREE.Vector3());
  camera.position.sub(centre).negate().add(centre);
  camera.lookAt(centre);
}

function fit(camera, object, controls = null) {
  object.updateMatrixWorld(true);
  const box = new THREE.Box3().setFromObject(object);
  if (box.isEmpty()) {
    throw new Error("This mesh contains no visible geometry.");
  }
  const centre = box.getCenter(new THREE.Vector3());
  const radius = Math.max(
    box.getSize(new THREE.Vector3()).length() * 0.5,
    0.01,
  );
  const halfAngle = Math.atan(
    Math.tan(THREE.MathUtils.degToRad(camera.fov / 2)) *
      Math.min(camera.aspect, 1),
  );
  const distance = (radius / Math.sin(halfAngle)) * 1.12;
  camera.up.set(0, 0, 1);
  camera.near = Math.max(radius / 1000, 0.0001);
  camera.far = distance + radius * 100;
  camera.position
    .copy(centre)
    .add(new THREE.Vector3(1, -1, 0.65).normalize().multiplyScalar(distance));
  camera.lookAt(centre);
  camera.updateProjectionMatrix();
  if (controls) {
    controls.target.copy(centre);
    controls.minDistance = radius * 0.03;
    controls.maxDistance = distance * 20;
    controls.update();
  }
}

export class ThumbnailRenderer {
  constructor() {
    this.renderer = new THREE.WebGLRenderer({
      antialias: true,
      preserveDrawingBuffer: true,
    });
    this.renderer.setSize(240, 160);
    this.renderer.outputColorSpace = THREE.SRGBColorSpace;
    this.scene = scene();
    this.camera = new THREE.PerspectiveCamera(40, 1.5, 0.01, 10000);
  }

  contextLost() {
    return this.renderer.getContext().isContextLost();
  }

  async render(data, readPixels) {
    const { group } = await buildModel(data, readPixels, 512, false);
    try {
      this.scene.add(group);
      fit(this.camera, group);
      this.renderer.render(this.scene, this.camera);
      if (!hasVisiblePixels(this.renderer)) {
        // A legitimate one-sided surface can face away from the default view.
        // Try the opposite side before rejecting it as an empty preview.
        oppositeView(this.camera, group);
        this.renderer.render(this.scene, this.camera);
        if (!hasVisiblePixels(this.renderer)) {
          throw new Error("This model produces no visible preview.");
        }
      }
      return this.renderer.domElement.toDataURL("image/png");
    } finally {
      this.scene.remove(group);
      disposeObject(group);
    }
  }
}

export class ModelViewer {
  constructor(container) {
    this.container = container;
    this.renderer = new THREE.WebGLRenderer({ antialias: true });
    this.renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
    this.renderer.outputColorSpace = THREE.SRGBColorSpace;
    container.append(this.renderer.domElement);
    this.scene = scene();
    this.camera = new THREE.PerspectiveCamera(40, 1, 0.01, 10000);
    this.camera.up.set(0, 0, 1);
    this.controls = new OrbitControls(this.camera, this.renderer.domElement);
    this.controls.addEventListener("change", () => this.render());
    this.controls.addEventListener("start", () => {
      this.interacting = true;
      this.highlightBone(null);
    });
    this.controls.addEventListener("end", () => {
      this.interacting = false;
      this.pickBone();
    });
    this.controls.enabled = false;
    this.model = null;
    this.overlay = null;
    this.selectedBone = null;
    this.meshOutline = null;
    this.data = null;
    this.lighting = false;
    this.namesVisible = false;
    this.hoveredBone = null;
    this.pointer = null;
    this.interacting = false;
    this.projectedBones = [];
    this.boneLabels = [];
    this.labels = document.createElement("div");
    this.labels.className = "bone-labels";
    this.labels.setAttribute("aria-hidden", "true");
    container.append(this.labels);
    this.tooltip = document.createElement("span");
    this.tooltip.className = "bone-tooltip";
    this.tooltip.hidden = true;
    this.labels.append(this.tooltip);
    this.renderer.domElement.addEventListener("pointermove", (event) => {
      this.pointer = { x: event.clientX, y: event.clientY };
      if (!event.buttons && !this.interacting) {
        this.pickBone();
      }
    });
    this.renderer.domElement.addEventListener("pointerleave", () => {
      this.pointer = null;
      this.highlightBone(null);
    });
    new ResizeObserver(() => this.resize()).observe(container);
    this.renderer.domElement.addEventListener("webglcontextlost", (event) => {
      event.preventDefault();
      window.dispatchEvent(
        new CustomEvent("viewer-error", {
          detail: "The graphics context was lost. Close and reopen the app.",
        }),
      );
    });
  }

  resize() {
    const width = Math.max(this.container.clientWidth, 1);
    const height = Math.max(this.container.clientHeight, 1);
    this.renderer.setSize(width, height, false);
    this.camera.aspect = width / height;
    this.camera.updateProjectionMatrix();
    this.render();
  }

  render() {
    this.renderer.render(this.scene, this.camera);
    this.updateBoneLabels();
  }

  clear() {
    for (const object of [
      this.model,
      this.overlay,
      this.selectedBone,
      this.meshOutline,
    ]) {
      if (object) {
        this.scene.remove(object);
        disposeObject(object);
      }
    }
    this.model = null;
    this.overlay = null;
    this.selectedBone = null;
    this.meshOutline = null;
    this.data = null;
    this.hoveredBone = null;
    this.projectedBones = [];
    for (const label of this.boneLabels) {
      label.remove();
    }
    this.boneLabels = [];
    this.tooltip.hidden = true;
    this.controls.enabled = false;
    this.render();
  }

  setModel(group, data) {
    this.clear();
    this.model = group;
    this.data = data;
    this.boneLabels = data.bones.map((bone, index) => {
      const label = document.createElement("span");
      label.className = "bone-name";
      label.textContent = boneLabel(bone, index);
      label.dataset.boneIndex = String(index);
      label.hidden = true;
      this.labels.append(label);
      return label;
    });
    this.scene.add(group);
    this.controls.enabled = true;
    this.resize();
    this.reset();
  }

  reset() {
    if (this.model) {
      fit(this.camera, this.model, this.controls);
      this.render();
      if (!hasVisiblePixels(this.renderer)) {
        oppositeView(this.camera, this.model);
        this.controls.update();
        this.render();
        if (!hasVisiblePixels(this.renderer)) {
          throw new Error("This model produces no visible preview.");
        }
      }
    }
  }

  setLighting(enabled) {
    this.lighting = enabled;
    if (!this.model) {
      return;
    }
    this.model.traverse((object) => {
      if (object.isMesh) {
        const old = object.material;
        object.material = previewMaterial(
          object.userData.settings,
          old.map,
          enabled,
        );
        this.offsetSurface(object.material);
        old.dispose();
      }
    });
    this.render();
  }

  showBones(enabled) {
    if (this.overlay) {
      this.scene.remove(this.overlay);
      disposeObject(this.overlay);
      this.overlay = null;
    }
    if (enabled && this.data?.bones.length) {
      const lines = [];
      const points = [];
      for (const bone of this.data.bones) {
        points.push(...bone.position);
        if (bone.parent !== null) {
          lines.push(
            ...bone.position,
            ...this.data.bones[bone.parent].position,
          );
        }
      }
      this.overlay = new THREE.Group();
      const lineGeometry = new THREE.BufferGeometry();
      lineGeometry.setAttribute(
        "position",
        new THREE.Float32BufferAttribute(lines, 3),
      );
      const line = new THREE.LineSegments(
        lineGeometry,
        new THREE.LineBasicMaterial({ color: 0x6dd4ed, depthTest: false }),
      );
      line.renderOrder = 1000;
      const pointGeometry = new THREE.BufferGeometry();
      pointGeometry.setAttribute(
        "position",
        new THREE.Float32BufferAttribute(points, 3),
      );
      const point = new THREE.Points(
        pointGeometry,
        new THREE.PointsMaterial({
          color: 0x9cddf3,
          size: 4,
          sizeAttenuation: false,
          depthTest: false,
        }),
      );
      point.renderOrder = 1001;
      this.overlay.add(line, point);
      this.scene.add(this.overlay);
    }
    if (!enabled) {
      this.highlightBone(null);
    }
    this.render();
  }

  showBoneNames(enabled) {
    this.namesVisible = enabled;
    this.updateBoneLabels();
  }

  updateBoneLabels() {
    const width = this.renderer.domElement.clientWidth;
    const height = this.renderer.domElement.clientHeight;
    this.projectedBones = (this.data?.bones || []).map((bone) => {
      const point = new THREE.Vector3(...bone.position).project(this.camera);
      if (
        ![point.x, point.y, point.z].every(Number.isFinite) ||
        point.z < -1 ||
        point.z > 1 ||
        Math.abs(point.x) > 1 ||
        Math.abs(point.y) > 1
      ) {
        return null;
      }
      return {
        x: ((point.x + 1) * width) / 2,
        y: ((1 - point.y) * height) / 2,
        depth: point.z,
      };
    });
    for (const [index, label] of this.boneLabels.entries()) {
      const point = this.projectedBones[index];
      label.hidden = !this.overlay || !this.namesVisible || !point;
      if (!label.hidden) {
        // Keep a fixed offset from the projected joint. Do not move labels to
        // other parts of the viewport to avoid overlaps while orbiting.
        label.style.left = `${point.x + 7}px`;
        label.style.top = `${point.y - 9}px`;
      }
    }
    const point = this.projectedBones[this.hoveredBone];
    this.tooltip.hidden = !this.overlay || !point || this.hoveredBone === null;
    if (!this.tooltip.hidden) {
      // Coincident joints can carry several tags. Keep all their names available
      // at the shared point rather than making the extra tags impossible to hover.
      const names = this.projectedBones.flatMap((other, index) => {
        if (other && Math.hypot(other.x - point.x, other.y - point.y) < 1) {
          return [boneLabel(this.data.bones[index], index)];
        }
        return [];
      });
      this.tooltip.textContent = [...new Set(names)].join("\n");
      this.tooltip.style.left = `${Math.max(0, Math.min(point.x + 10, width - 220))}px`;
      this.tooltip.style.top = `${Math.max(0, Math.min(point.y, height - names.length * 14 - 6))}px`;
    }
  }

  pickBone() {
    if (!this.overlay || !this.pointer || this.interacting) {
      this.highlightBone(null);
      return;
    }
    const bounds = this.renderer.domElement.getBoundingClientRect();
    const pointer = {
      x: this.pointer.x - bounds.left,
      y: this.pointer.y - bounds.top,
    };
    let nearest = null;
    let distance = 9;
    for (const [index, point] of this.projectedBones.entries()) {
      if (!point) {
        continue;
      }
      const separation = Math.hypot(point.x - pointer.x, point.y - pointer.y);
      if (separation < distance) {
        nearest = index;
        distance = separation;
      }
    }
    // Prefer a joint within nine pixels. Away from joints, a five-pixel line hit
    // identifies the child bone represented by that parent-to-child segment.
    if (nearest === null) {
      distance = 5;
      for (const [index, point] of this.projectedBones.entries()) {
        const parent = this.data.bones[index].parent;
        const start = parent === null ? null : this.projectedBones[parent];
        if (!point || !start) {
          continue;
        }
        const x = point.x - start.x;
        const y = point.y - start.y;
        const length = x * x + y * y;
        if (length === 0) {
          continue;
        }
        const along = Math.max(
          0,
          Math.min(
            1,
            ((pointer.x - start.x) * x + (pointer.y - start.y) * y) / length,
          ),
        );
        const separation = Math.hypot(
          pointer.x - start.x - along * x,
          pointer.y - start.y - along * y,
        );
        if (separation < distance) {
          nearest = index;
          distance = separation;
        }
      }
    }
    this.highlightBone(nearest);
  }

  highlightBone(index) {
    if (index === this.hoveredBone) {
      return;
    }
    this.hoveredBone = index;
    if (this.selectedBone) {
      this.scene.remove(this.selectedBone);
      disposeObject(this.selectedBone);
      this.selectedBone = null;
    }
    const bone = index === null ? null : this.data?.bones[index];
    if (bone) {
      const geometry = new THREE.BufferGeometry();
      geometry.setAttribute(
        "position",
        new THREE.Float32BufferAttribute(bone.position, 3),
      );
      this.selectedBone = new THREE.Points(
        geometry,
        new THREE.PointsMaterial({
          color: 0xffbd65,
          size: 9,
          sizeAttenuation: false,
          depthTest: false,
        }),
      );
      this.selectedBone.renderOrder = 1002;
      this.scene.add(this.selectedBone);
    }
    this.render();
  }

  offsetSurface(material) {
    material.polygonOffset = Boolean(this.meshOutline?.visible);
    material.polygonOffsetFactor = 1;
    material.polygonOffsetUnits = 1;
  }

  showMeshOutline(enabled) {
    if (!this.model) {
      return;
    }
    if (enabled && !this.meshOutline) {
      this.meshOutline = new THREE.Group();
      for (const mesh of this.model.children) {
        // WireframeGeometry reads the existing indexed triangles. It adds no
        // approximated edges and owns separate buffers, safe to dispose on clear.
        const geometry = new THREE.WireframeGeometry(mesh.geometry);
        const material = new THREE.LineBasicMaterial({
          color: 0x80b7dc,
          depthTest: true,
          depthWrite: false,
          transparent: true,
          opacity: 0.8,
        });
        const lines = new THREE.LineSegments(geometry, material);
        lines.renderOrder = 10;
        this.meshOutline.add(lines);
      }
      this.scene.add(this.meshOutline);
    }
    if (this.meshOutline) {
      this.meshOutline.visible = enabled;
    }
    for (const mesh of this.model.children) {
      this.offsetSurface(mesh.material);
    }
    this.render();
  }
}
