import * as THREE from "three";

// WaW interpolates packed quaternion components. Three's AnimationMixer uses
// SLERP instead, so playback samples the verified native-style tracks directly.
export function sampleKeys(keys, frame, rotation = false) {
  const values = keys?.values || [];
  if (values.length === 0) {
    return rotation ? [0, 0, 0, 1] : [0, 0, 0];
  }
  let value;
  if (values.length === 1 || frame <= keys.indices[0]) {
    value = [...values[0]];
  } else if (frame >= keys.indices[keys.indices.length - 1]) {
    value = [...values[values.length - 1]];
  } else {
    let left = 0;
    let right = keys.indices.length - 1;
    while (right - left > 1) {
      const middle = Math.floor((left + right) / 2);
      if (keys.indices[middle] <= frame) {
        left = middle;
      } else {
        right = middle;
      }
    }
    const span = keys.indices[right] - keys.indices[left];
    if (span <= 0) {
      throw new Error("Animation key times are not ordered.");
    }
    const fraction = (frame - keys.indices[left]) / span;
    value = values[left].map((component, axis) => {
      return component + (values[right][axis] - component) * fraction;
    });
  }
  if (rotation) {
    const length = Math.hypot(...value);
    if (!Number.isFinite(length) || length === 0) {
      throw new Error("Animation contains an unusable rotation.");
    }
    value = value.map((component) => component / length);
  }
  if (!value.every(Number.isFinite)) {
    throw new Error("Animation produced a non-finite pose.");
  }
  return value;
}

function makeBones(source) {
  const root = new THREE.Group();
  const bones = source.map((bone) => {
    const node = new THREE.Bone();
    node.name = bone.name;
    node.position.fromArray(bone.localPosition);
    node.quaternion.fromArray(bone.localRotation);
    return node;
  });
  source.forEach((bone, index) => {
    if (bone.parent !== null && (bone.parent < 0 || bone.parent >= index)) {
      throw new Error("Animation rig has an invalid bone parent.");
    }
    const parent = bone.parent === null ? root : bones[bone.parent];
    parent.add(bones[index]);
  });
  root.updateMatrixWorld(true);
  return { root, bones, source };
}

function poseRig(rig, tracks, frame) {
  rig.bones.forEach((bone, index) => {
    const bind = rig.source[index];
    const track = tracks.get(bind.name.toLowerCase());
    bone.position.fromArray(bind.localPosition);
    if (track) {
      const offset = sampleKeys(track.translation, frame);
      bone.position.add(new THREE.Vector3(...offset));
      bone.quaternion.fromArray(sampleKeys(track.rotation, frame, true));
    } else {
      // An absent track retains the bind rotation. An explicit NO_QUAT track
      // above evaluates to identity; they are intentionally different cases.
      bone.quaternion.fromArray(bind.localRotation);
    }
  });
}

export class PlaybackRig {
  constructor(group, data, onChange = () => {}) {
    if (!data.bones.length) {
      throw new Error("This model has no usable animation rig.");
    }
    this.group = group;
    this.data = data;
    this.onChange = onChange;
    this.rig = makeBones(data.bones);
    this.bones = this.rig.bones;
    this.skeleton = new THREE.Skeleton(this.bones);
    this.skeleton.calculateInverses();
    this.basePosition = group.position.clone();
    this.baseRotation = group.quaternion.clone();
    this.meshes = [];
    for (const surface of data.surfaces) {
      const count = surface.positions.length / 3;
      if (
        surface.skinIndices?.length !== count * 4 ||
        surface.skinWeights?.length !== count * 4
      ) {
        throw new Error("Skin data does not match the model vertices.");
      }
      if (surface.skinIndices.some((index) => index >= this.bones.length)) {
        throw new Error("A skin index is outside the model rig.");
      }
    }
    // Inverse binds belong to the standalone model. Calculate them BEFORE
    // attaching a weapon to its associated hands rig's tag_weapon context.
    for (const mesh of [...group.children]) {
      if (!mesh.isMesh) {
        continue;
      }
      const surface = data.surfaces[this.meshes.length];
      mesh.geometry.setAttribute(
        "skinIndex",
        new THREE.Uint16BufferAttribute(surface.skinIndices, 4),
      );
      mesh.geometry.setAttribute(
        "skinWeight",
        new THREE.Float32BufferAttribute(surface.skinWeights, 4),
      );
      const skinned = new THREE.SkinnedMesh(mesh.geometry, mesh.material);
      skinned.userData = mesh.userData;
      skinned.renderOrder = mesh.renderOrder;
      skinned.bind(this.skeleton, new THREE.Matrix4());
      // Animated bounds change every pose. Avoid stale bind-pose culling;
      // Reset view computes the current bounds explicitly when requested.
      skinned.frustumCulled = false;
      group.remove(mesh);
      group.add(skinned);
      this.meshes.push(skinned);
    }
    group.add(this.rig.root);
    this.playback = null;
    this.parents = null;
    this.frame = 0;
    this.playing = false;
    this.loop = false;
    this.rootMotion = false;
    this.request = 0;
    this.lastTick = null;
    this.tracks = new Map();
    this.refresh();
  }

  state() {
    const clip = this.playback?.clip;
    return {
      name: clip?.name || "",
      frame: this.frame,
      lastFrame: clip?.lastFrame || 0,
      fps: clip?.fps || 0,
      playing: this.playing,
      loop: this.loop,
      rootMotion: this.rootMotion,
      hasRootMotion: Boolean(
        clip?.delta?.rotation?.values.length ||
        clip?.delta?.translation?.values.length,
      ),
    };
  }

  setClip(playback) {
    this.clearClip();
    this.playback = playback;
    this.loop = playback.clip.looped;
    this.tracks = new Map(
      playback.clip.bones.map((track) => [track.name.toLowerCase(), track]),
    );
    if (playback.parentBones.length) {
      this.parents = makeBones(playback.parentBones);
      const attachment = this.parents.bones.find((bone) => {
        return bone.name.toLowerCase() === "tag_weapon";
      });
      if (!attachment) {
        throw new Error("The weapon's tag_weapon parent is missing.");
      }
      this.group.add(this.parents.root);
      attachment.add(this.rig.root);
    }
    this.seek(0);
  }

  refresh() {
    this.group.updateMatrixWorld(true);
    this.skeleton.update();
    for (const mesh of this.meshes) {
      mesh.boundingBox = null;
      mesh.boundingSphere = null;
    }
  }

  seek(frame) {
    if (!this.playback) {
      return;
    }
    const clip = this.playback.clip;
    if (!Number.isFinite(Number(frame))) {
      throw new Error("Animation position must be a finite number.");
    }
    this.frame = Math.max(0, Math.min(Number(frame), clip.lastFrame));
    const time = clip.lastFrame ? Math.fround(this.frame / clip.lastFrame) : 0;
    const sampledFrame = Math.fround(time * clip.lastFrame);
    if (this.parents) {
      poseRig(this.parents, this.tracks, sampledFrame);
    }
    poseRig(this.rig, this.tracks, sampledFrame);
    this.group.position.copy(this.basePosition);
    this.group.quaternion.copy(this.baseRotation);
    if (this.rootMotion && clip.delta) {
      const start = new THREE.Quaternion(
        ...sampleKeys(clip.delta.rotation, 0, true),
      );
      const rotation = new THREE.Quaternion(
        ...sampleKeys(clip.delta.rotation, sampledFrame, true),
      );
      const origin = new THREE.Vector3(
        ...sampleKeys(clip.delta.translation, 0),
      );
      const position = new THREE.Vector3(
        ...sampleKeys(clip.delta.translation, sampledFrame),
      );
      // A preview starts at the model's own origin, not the source cutscene's
      // scene coordinates. Relative movement uses the verified native yaw space.
      const inverseStart = start.clone().invert();
      position
        .sub(origin)
        .applyQuaternion(inverseStart)
        .applyQuaternion(this.baseRotation);
      this.group.position.add(position);
      this.group.quaternion.multiply(inverseStart.multiply(rotation));
    }
    this.refresh();
    this.onChange(this.state());
  }

  setPlaying(enabled) {
    const clip = this.playback?.clip;
    this.playing = Boolean(enabled && clip?.lastFrame > 0);
    cancelAnimationFrame(this.request);
    this.request = 0;
    this.lastTick = null;
    if (this.playing) {
      if (this.frame >= clip.lastFrame) {
        this.seek(0);
      }
      this.request = requestAnimationFrame((now) => this.tick(now));
    }
    this.onChange(this.state());
  }

  tick(now) {
    if (!this.playing || !this.playback) {
      return;
    }
    const clip = this.playback.clip;
    const elapsed = this.lastTick === null ? 0 : (now - this.lastTick) / 1000;
    this.lastTick = now;
    let frame = this.frame + elapsed * clip.fps;
    if (frame >= clip.lastFrame) {
      if (this.loop) {
        frame %= clip.lastFrame;
      } else {
        frame = clip.lastFrame;
        this.playing = false;
      }
    }
    try {
      this.seek(frame);
    } catch (error) {
      this.setPlaying(false);
      this.onError?.(error);
      return;
    }
    if (this.playing) {
      this.request = requestAnimationFrame((next) => this.tick(next));
    }
  }

  setLoop(enabled) {
    this.loop = Boolean(enabled);
    this.onChange(this.state());
  }

  setRootMotion(enabled) {
    this.rootMotion = Boolean(enabled);
    this.seek(this.frame);
  }

  clearClip() {
    this.setPlaying(false);
    this.group.add(this.rig.root);
    if (this.parents) {
      this.group.remove(this.parents.root);
    }
    this.parents = null;
    this.playback = null;
    this.tracks = new Map();
    this.frame = 0;
    this.loop = false;
    this.rootMotion = false;
    this.group.position.copy(this.basePosition);
    this.group.quaternion.copy(this.baseRotation);
    poseRig(this.rig, this.tracks, 0);
    this.refresh();
    this.onChange(this.state());
  }

  dispose() {
    // A removed preview must not overwrite the controls for the next model.
    this.onChange = () => {};
    this.setPlaying(false);
    this.skeleton.dispose();
  }
}
