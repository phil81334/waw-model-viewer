// Share repeated colour-image requests across previews without retaining an
// unbounded collection of decoded images. GPU textures still belong to a model.
export class PixelCache {
  constructor(load, limit = 64 * 1024 * 1024) {
    this.load = load;
    this.limit = limit;
    this.entries = new Map();
    this.bytes = 0;
  }

  clear() {
    this.entries.clear();
    this.bytes = 0;
  }

  get(name, size) {
    const key = `${size}:${name.toLowerCase()}`;
    const existing = this.entries.get(key);
    if (existing) {
      this.entries.delete(key);
      this.entries.set(key, existing);
      return existing.promise;
    }
    const record = { bytes: 0, promise: null };
    record.promise = Promise.resolve().then(() => this.load(name, size)).then(
      (value) => {
        if (Array.isArray(value)) {
          value = Uint8Array.from(value);
        }
        // Clear cache or a later scan may have replaced this request already.
        if (this.entries.get(key) !== record) {
          return value;
        }
        record.bytes = value.byteLength ?? value.length;
        this.bytes += record.bytes;
        while (this.bytes > this.limit || this.entries.size > 128) {
          const oldest = this.entries.keys().next().value;
          this.bytes -= this.entries.get(oldest).bytes;
          this.entries.delete(oldest);
        }
        return value;
      },
      (error) => {
        if (this.entries.get(key) === record) {
          this.entries.delete(key);
        }
        throw error;
      },
    );
    this.entries.set(key, record);
    return record.promise;
  }
}
