// Reserved indices - must match Rust's value.rs constants
const JSIDX_OFFSET = 128;
const JSIDX_UNDEFINED = JSIDX_OFFSET;
const JSIDX_NULL = JSIDX_OFFSET + 1;
const JSIDX_TRUE = JSIDX_OFFSET + 2;
const JSIDX_FALSE = JSIDX_OFFSET + 3;
const JSIDX_RESERVED = JSIDX_OFFSET + 4;

// Object store implementation for JS heap types
class DeferredHeapRefs {
  declare private heap: JSHeap;
  declare private requestId: number;
  declare private values: unknown[];
  declare private resolved: boolean;

  constructor(heap: JSHeap, requestId: number) {
    this.heap = heap;
    this.requestId = requestId;
    this.values = [];
    this.resolved = false;
  }

  rawId(): number {
    return this.requestId;
  }

  count(): number {
    return this.values.length;
  }

  push(value: unknown): void {
    if (this.resolved) {
      throw new Error("Deferred heap refs already resolved");
    }
    this.values.push(value);
  }

  isEmpty(): boolean {
    return this.values.length === 0;
  }

  resolve(ids: number[]): void {
    if (this.resolved) {
      throw new Error("Deferred heap refs already resolved");
    }
    this.resolved = true;

    if (this.values.length !== ids.length) {
      throw new Error(
        `Heap-ref install count mismatch: ${ids.length} IDs for ${this.values.length} values`
      );
    }

    for (let i = 0; i < ids.length; i++) {
      this.heap.insertAt(ids[i], this.values[i]);
    }
  }

  finishIfEmpty(): void {
    if (this.resolved) {
      return;
    }

    this.resolved = this.values.length === 0;
  }
}

class JSHeap {
  declare private slots: Map<number, unknown>;
  declare private heapObjectCount: number;
  // Borrow stack uses indices 1-127, growing downward from 127 to 1
  declare private borrowStackPointer: number;
  // Frame stack for nested operations - saves borrow stack pointers
  declare private borrowFrameStack: number[];
  // Stack of reservation scopes: each scope tracks exact IDs reserved by Rust
  declare private reservationStack: { ids: number[]; nextIndex: number }[];
  declare private deferredHeapRefs: Map<number, DeferredHeapRefs[]>;

  constructor() {
    // Slots 0-127 are for borrow stack (1-127 usable), slots 128-131
    // are reserved for special values.
    // A Map avoids sparse array slowdowns as Rust assigns high heap IDs.
    this.slots = new Map();

    this.slots.set(JSIDX_NULL, null);
    this.slots.set(JSIDX_TRUE, true);
    this.slots.set(JSIDX_FALSE, false);
    this.slots.set(JSIDX_UNDEFINED, undefined);

    this.heapObjectCount = 0;
    // Borrow stack pointer starts at 128 (just below reserved values)
    this.borrowStackPointer = JSIDX_OFFSET;
    // Frame stack starts empty
    this.borrowFrameStack = [];
    // Reservation stack starts empty
    this.reservationStack = [];
    this.deferredHeapRefs = new Map();
  }

  insertAt(id: number, value: unknown): void {
    if (id < JSIDX_RESERVED) {
      throw new Error(`Cannot install heap ref into special slot ${id}`);
    }
    if (id >= JSIDX_RESERVED && !this.slots.has(id)) {
      this.heapObjectCount++;
    }
    this.slots.set(id, value);
  }

  deferHeapRefs(requestId: number): DeferredHeapRefs {
    const refs = new DeferredHeapRefs(this, requestId);
    let frames = this.deferredHeapRefs.get(requestId);
    if (!frames) {
      frames = [];
      this.deferredHeapRefs.set(requestId, frames);
    }
    frames.push(refs);
    return refs;
  }

  resolveDeferredHeapRefs(requestId: number, ids: number[]): boolean {
    const frames = this.deferredHeapRefs.get(requestId);
    if (!frames) {
      return false;
    }

    let frameIndex = -1;
    for (let i = frames.length - 1; i >= 0; i--) {
      if (frames[i].count() === ids.length) {
        frameIndex = i;
        break;
      }
    }
    if (frameIndex < 0) {
      return false;
    }

    const refs = frames[frameIndex];
    refs.resolve(ids);
    frames.splice(frameIndex, 1);
    if (frames.length === 0) {
      this.deferredHeapRefs.delete(requestId);
    }
    return true;
  }

  releaseEmptyDeferredHeapRefs(refs: DeferredHeapRefs): void {
    refs.finishIfEmpty();
    if (refs.count() === 0) {
      const frames = this.deferredHeapRefs.get(refs.rawId());
      if (!frames) {
        return;
      }
      const frameIndex = frames.indexOf(refs);
      if (frameIndex >= 0) {
        frames.splice(frameIndex, 1);
      }
      if (frames.length === 0) {
        this.deferredHeapRefs.delete(refs.rawId());
      }
    }
  }

  // Push a reservation scope for exact IDs allocated by Rust
  pushReservationScope(ids: number[]): void {
    this.reservationStack.push({ ids, nextIndex: 0 });
    for (const id of ids) {
      if (id < JSIDX_RESERVED) {
        throw new Error(`Cannot reserve special heap slot ${id}`);
      }
      if (this.slots.has(id)) {
        throw new Error(`Reserved heap slot ${id} is already occupied`);
      }
    }
  }

  popReservationScope(): void {
    const scope = this.reservationStack.pop();
    if (scope && scope.nextIndex !== scope.ids.length) {
      throw new Error(
        `Only filled ${scope.nextIndex} of ${scope.ids.length} reserved heap slots`
      );
    }
  }

  // Fill the next reserved slot in the current scope
  fillNextReserved(value: unknown): void {
    const scope = this.reservationStack[this.reservationStack.length - 1];
    if (!scope || scope.nextIndex >= scope.ids.length) {
      throw new Error("No reserved slots available");
    }
    const id = scope.ids[scope.nextIndex];
    scope.nextIndex++;
    this.insertAt(id, value);
  }

  get(id: number): unknown | undefined {
    return this.slots.get(id);
  }

  remove(id: number): unknown | undefined {
    // Never remove reserved slots
    if (id < JSIDX_RESERVED) {
      return this.slots.get(id);
    }

    if (!this.has(id)) {
      return undefined;
    }

    const value = this.slots.get(id);

    this.slots.delete(id);
    this.heapObjectCount--;
    return value;
  }

  has(id: number): boolean {
    return this.slots.has(id);
  }

  heapObjectsAlive(): number {
    return this.heapObjectCount;
  }

  // Add a borrowed reference to the borrow stack (indices 1-127)
  // Returns the stack slot index
  addBorrowedRef(obj: unknown): number {
    if (this.borrowStackPointer <= 1) {
      throw new Error(
        "Borrow stack overflow: too many borrowed references in a single operation"
      );
    }
    this.borrowStackPointer--;
    this.slots.set(this.borrowStackPointer, obj);
    return this.borrowStackPointer;
  }

  // Push a borrow frame before a nested operation that may add borrowed refs
  // This saves the current borrow stack pointer so we can restore it later
  pushBorrowFrame(): void {
    this.borrowFrameStack.push(this.borrowStackPointer);
  }

  // Pop a borrow frame after a nested operation completes
  // This clears only the borrowed refs from this frame and restores the pointer
  popBorrowFrame(): void {
    const savedPointer = this.borrowFrameStack.pop();
    if (savedPointer !== undefined) {
      // Clear refs from this frame only (from current pointer up to saved pointer)
      for (let i = this.borrowStackPointer; i < savedPointer; i++) {
        this.slots.delete(i);
      }
      this.borrowStackPointer = savedPointer;
    }
  }

  // Get the current borrow stack pointer (for testing)
  getBorrowStackPointer(): number {
    return this.borrowStackPointer;
  }
}

export { DeferredHeapRefs, JSHeap };
