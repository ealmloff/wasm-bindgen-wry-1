// Reserved indices - must match Rust's value.rs constants
const JSIDX_OFFSET = 128;
const JSIDX_UNDEFINED = JSIDX_OFFSET;
const JSIDX_NULL = JSIDX_OFFSET + 1;
const JSIDX_TRUE = JSIDX_OFFSET + 2;
const JSIDX_FALSE = JSIDX_OFFSET + 3;
const JSIDX_RESERVED = JSIDX_OFFSET + 4;

// Object store implementation for JS heap types
class DeferredHeapRefs {
  private heap: JSHeap;
  private requestId: number;
  private values: unknown[];
  private resolved: boolean;

  constructor(heap: JSHeap, requestId: number) {
    this.heap = heap;
    this.requestId = requestId;
    this.values = [];
    this.resolved = false;
  }

  id(): number {
    return this.values.length === 0 ? 0 : this.requestId;
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
  private slots: Map<number, unknown>;
  private freeIds: Set<number>;
  private heapObjectCount: number;
  private maxId: number;
  // Borrow stack uses indices 1-127, growing downward from 127 to 1
  private borrowStackPointer: number;
  // Frame stack for nested operations - saves borrow stack pointers
  private borrowFrameStack: number[];
  // Stack of reservation scopes: each scope tracks exact IDs reserved by Rust
  private reservationStack: { ids: number[]; nextIndex: number }[];
  private deferredHeapRefRequestId: number;
  private deferredHeapRefs: Map<number, DeferredHeapRefs>;

  constructor() {
    // Slots 0-127 are for borrow stack (1-127 usable), slots 128-131
    // are reserved for special values, and heap allocation starts at 132.
    // A Map avoids sparse array slowdowns as Rust reserves high placeholder IDs.
    this.slots = new Map();

    this.slots.set(JSIDX_NULL, null);
    this.slots.set(JSIDX_TRUE, true);
    this.slots.set(JSIDX_FALSE, false);
    this.slots.set(JSIDX_UNDEFINED, undefined);

    this.freeIds = new Set();
    this.heapObjectCount = 0;
    // Start allocating from JSIDX_RESERVED (132)
    this.maxId = JSIDX_RESERVED;
    // Borrow stack pointer starts at 128 (just below reserved values)
    this.borrowStackPointer = JSIDX_OFFSET;
    // Frame stack starts empty
    this.borrowFrameStack = [];
    // Reservation stack starts empty
    this.reservationStack = [];
    this.deferredHeapRefRequestId = 1;
    this.deferredHeapRefs = new Map();
  }

  insert(value: unknown): number {
    const freeId = this.freeIds.values().next();
    let id: number;
    if (freeId.done) {
      id = this.maxId;
      this.maxId++;
    } else {
      id = freeId.value;
      this.freeIds.delete(id);
    }
    this.slots.set(id, value);
    this.heapObjectCount++;
    return id;
  }

  insertAt(id: number, value: unknown): void {
    if (id >= JSIDX_RESERVED && !this.slots.has(id)) {
      this.heapObjectCount++;
    }
    this.freeIds.delete(id);
    this.slots.set(id, value);
    this.maxId = Math.max(this.maxId, id + 1);
  }

  deferHeapRefs(): DeferredHeapRefs {
    const requestId = this.deferredHeapRefRequestId++;
    const refs = new DeferredHeapRefs(this, requestId);
    this.deferredHeapRefs.set(requestId, refs);
    return refs;
  }

  resolveDeferredHeapRefs(requestId: number, ids: number[]): void {
    const refs = this.deferredHeapRefs.get(requestId);
    if (!refs) {
      return;
    }

    refs.resolve(ids);
    this.deferredHeapRefs.delete(requestId);
  }

  releaseEmptyDeferredHeapRefs(refs: DeferredHeapRefs): void {
    refs.finishIfEmpty();
    if (refs.count() === 0) {
      this.deferredHeapRefs.delete(refs.rawId());
    }
  }

  // Push a reservation scope for exact IDs allocated by Rust
  pushReservationScope(ids: number[]): void {
    this.reservationStack.push({ ids, nextIndex: 0 });
    for (const id of ids) {
      this.freeIds.delete(id);
      this.maxId = Math.max(this.maxId, id + 1);
    }
  }

  popReservationScope(): void {
    this.reservationStack.pop();
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
    this.freeIds.add(id);
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
