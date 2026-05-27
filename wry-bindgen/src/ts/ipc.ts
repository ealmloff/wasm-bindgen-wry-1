/**
 * Binary Protocol Encoder/Decoder
 *
 * The binary format uses aligned buffers for efficient memory access:
 * - First 12 bytes: three u32 offsets (u16_offset, u8_offset, str_offset)
 * - u32 buffer: from byte 12 to u16_offset
 * - u16 buffer: from u16_offset to u8_offset
 * - u8 buffer: from u8_offset to str_offset
 * - string buffer: from str_offset to end
 *
 * Message format in the u8 buffer:
 * - First u8: message type (0 = Evaluate, 1 = Respond)
 * - Remaining data depends on message type
 */

import { DataDecoder, DataEncoder } from "./encoding";
import { getFunctionRegistry, getTypeCache, CachedTypeInfo } from "./function_registry";
import { parseTypeDef, TypeClass, HeapRefType } from "./types";

enum MessageType {
  Evaluate = 0,
  Respond = 1,
}

// Type caching markers - must match Rust's TYPE_CACHED and TYPE_FULL
const TYPE_CACHED = 0xff;
const TYPE_FULL = 0xfe;

// Reserved function ID for dropping native Rust refs - must match Rust's DROP_NATIVE_REF_FN_ID
const DROP_NATIVE_REF_FN_ID = 0xffffffff;

// Reserved function ID for calling exported Rust struct methods - must match Rust's CALL_EXPORT_FN_ID
const CALL_EXPORT_FN_ID = 0xfffffffe;

interface MessageHeader {
  requestId: number;
  responseChannelId: number;
}

let nextJsRequestId = 1;
const rustCallResponseChannels: number[] = [];

const pendingTypeCacheAcks: Set<number> = new Set();
const pendingHeapRefInstallAcks: Set<number> = new Set();

function allocateJsRequestId(): number {
  const id = nextJsRequestId;
  nextJsRequestId = (nextJsRequestId + 1) >>> 0;
  if (nextJsRequestId === 0) {
    nextJsRequestId = 1;
  }
  return id;
}

function currentResponseChannelId(): number {
  return rustCallResponseChannels[rustCallResponseChannels.length - 1] ?? 0;
}

function pushMessageHeader(
  encoder: DataEncoder,
  msgType: MessageType,
  requestId: number,
  responseChannelId: number
): void {
  encoder.pushU8(msgType);
  encoder.pushU32(requestId);
  encoder.pushU32(responseChannelId);
}

function takeMessageHeader(decoder: DataDecoder): {
  msgType: MessageType;
  header: MessageHeader;
} {
  const rawMsgType = decoder.takeU8();
  return {
    msgType: rawMsgType,
    header: {
      requestId: decoder.takeU32(),
      responseChannelId: decoder.takeU32(),
    },
  };
}

function prependJsToRustPrelude(encoder: DataEncoder): void {
  const heapRefInstallAckIds = Array.from(pendingHeapRefInstallAcks);
  pendingHeapRefInstallAcks.clear();

  const typeCacheAckIds = Array.from(pendingTypeCacheAcks);
  pendingTypeCacheAcks.clear();

  const prelude: number[] = [
    encoder.deferredHeapRefRequestId(),
    encoder.deferredHeapRefCount(),
    heapRefInstallAckIds.length,
  ];
  for (const id of heapRefInstallAckIds) {
    prelude.push(id);
  }
  prelude.push(typeCacheAckIds.length);
  for (const id of typeCacheAckIds) {
    prelude.push(id);
  }
  encoder.insertU32s(2, prelude);
}

/**
 * Sends binary data to Rust and receives binary response.
 */
function sync_request_binary(
  endpoint: string,
  data: ArrayBuffer
): ArrayBuffer | null {
  const xhr = new XMLHttpRequest();
  xhr.open("POST", endpoint, false);
  // Note: Cannot set responseType on sync requests - response comes as base64 text

  // Encode as base64 for header (Android workaround)
  const bytes = new Uint8Array(data);
  let binary = "";
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  const base64 = btoa(binary);
  xhr.setRequestHeader("dioxus-data", base64);
  xhr.send();

  if (xhr.status === 200 && xhr.responseText) {
    // Decode base64 response to ArrayBuffer
    const responseBinary = atob(xhr.responseText);
    const responseBytes = new Uint8Array(responseBinary.length);
    for (let i = 0; i < responseBinary.length; i++) {
      responseBytes[i] = responseBinary.charCodeAt(i);
    }
    return responseBytes.buffer;
  }
  return null;
}

/**
 * Entry point for Rust to call JS functions using binary protocol.
 * Handles batched operations - reads and executes operations until buffer is exhausted.
 *
 * @param dataBase64 - Base64 encoded binary data containing message with operations
 */
function evaluate_from_rust_binary(dataBase64: string) {
  // Decode base64 to ArrayBuffer
  const binary = atob(dataBase64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  const remaining = handleBinaryResponse(bytes.buffer);
  if (remaining) {
    throw new Error("Unprocessed data remaining after Evaluate handling");
  }
}

/**
 * Parse type information from the decoder.
 * Handles both cached and full type definitions.
 */
function parseTypeInfo(decoder: DataDecoder): CachedTypeInfo {
  const typeCache = getTypeCache();
  const typeMarker = decoder.takeU8();

  if (typeMarker === TYPE_CACHED) {
    // Cached type - look up by ID
    const typeId = decoder.takeU32();
    const cached = typeCache.get(typeId);
    if (!cached) {
      throw new Error(`Unknown cached type ID: ${typeId}`);
    }
    return cached;
  } else if (typeMarker === TYPE_FULL) {
    // Full type definition - parse and cache
    const typeId = decoder.takeU32();
    const paramCount = decoder.takeU8();

    // Get the remaining bytes for parsing type definitions
    const typeBytes = decoder.getRemainingBytes();
    const offset = { value: 0 };

    const paramTypes: TypeClass[] = [];
    for (let i = 0; i < paramCount; i++) {
      paramTypes.push(parseTypeDef(typeBytes, offset));
    }
    const returnType = parseTypeDef(typeBytes, offset);

    // Advance the decoder past the type definition bytes we consumed
    decoder.skipBytes(offset.value);

    const cached: CachedTypeInfo = { paramTypes, returnType };
    typeCache.set(typeId, cached);
    pendingTypeCacheAcks.add(typeId);
    return cached;
  } else {
    throw new Error(`Unknown type marker: ${typeMarker}`);
  }
}

function takeIdList(decoder: DataDecoder): number[] {
  const count = decoder.takeU32();
  const ids: number[] = [];
  for (let i = 0; i < count; i++) {
    ids.push(decoder.takeU64());
  }
  return ids;
}

function installDeferredHeapRefFrames(decoder: DataDecoder): void {
  const frameCount = decoder.takeU32();
  for (let i = 0; i < frameCount; i++) {
    const requestId = decoder.takeU32();
    const ids = takeIdList(decoder);
    const dropAfterInstall = takeIdList(decoder);
    window.jsHeap.resolveDeferredHeapRefs(requestId, ids);
    for (const id of dropAfterInstall) {
      window.jsHeap.remove(id);
    }
    pendingHeapRefInstallAcks.add(requestId);
  }
}

/**
 * Handle binary response from Rust.
 * May contain nested Evaluate calls (for callbacks).
 */
function handleBinaryResponse(
  response: ArrayBuffer | null,
  expectedRespondRequestId: number = 0
): DataDecoder | null {
  if (!response || response.byteLength === 0) {
    if (expectedRespondRequestId !== 0) {
      throw new Error(`Missing response for request ID ${expectedRespondRequestId}`);
    }
    return null;
  }

  const decoder = new DataDecoder(response);
  const { msgType, header } = takeMessageHeader(decoder);

  if (msgType === MessageType.Respond) {
    if (
      expectedRespondRequestId !== 0 &&
      header.requestId !== expectedRespondRequestId
    ) {
      throw new Error(
        `Response ID mismatch: expected ${expectedRespondRequestId}, got ${header.requestId}`
      );
    }
    installDeferredHeapRefFrames(decoder);
    // Respond - just return the decoder for further processing
    return decoder;
  } else if (msgType === MessageType.Evaluate) {
    // Evaluate - Rust is calling JS functions (possibly multiple)

    installDeferredHeapRefFrames(decoder);

    // Read the explicit placeholder IDs Rust reserved for this batch.
    const reservedIds = takeIdList(decoder);
    window.jsHeap.pushReservationScope(reservedIds);

    const encoder = new DataEncoder();
    pushMessageHeader(
      encoder,
      MessageType.Respond,
      header.requestId,
      currentResponseChannelId()
    );

    // Push a single borrow frame for this entire Evaluate message
    // This frame persists across all operations and nested calls
    window.jsHeap.pushBorrowFrame();

    // Process all operations
    while (decoder.hasMoreU32()) {
      const fnId = decoder.takeU32();
      // Parse type information (cached or full)
      const typeInfo = parseTypeInfo(decoder);

      // Get the raw JS function
      const functionRegistry = getFunctionRegistry();
      const jsFunction = functionRegistry[fnId];
      if (!jsFunction) {
        throw new Error("Unknown function ID in response: " + fnId);
      }

      // Decode parameters using their respective types
      let params: unknown[];
      try {
        params = typeInfo.paramTypes.map((paramType) => paramType.decode(decoder));
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        const source = String(jsFunction).replace(/\s+/g, " ").slice(0, 160);
        throw new Error(
          `Failed to decode parameters for function ID ${fnId} (${source}): ${message}`
        );
      }

      // Call the original JS function with decoded parameters
      const result = jsFunction(...params);

      // If return type is HeapRef and Rust reserved slots, fill the next reserved slot.
      // Otherwise fall back to normal encode() behavior.
      if (typeInfo.returnType instanceof HeapRefType && reservedIds.length > 0) {
        window.jsHeap.fillNextReserved(result);
      } else {
        // Encode the result using the return type
        typeInfo.returnType.encode(encoder, result);
      }
    }

    // Pop the borrow frame after all operations complete
    window.jsHeap.popBorrowFrame();

    // Pop the reservation scope
    window.jsHeap.popReservationScope();

    prependJsToRustPrelude(encoder);

    const nextResponse = sync_request_binary(
      `/__wbg__/handler`,
      encoder.finalize()
    );
    return handleBinaryResponse(nextResponse, expectedRespondRequestId);
  }

  if (!decoder.isEmpty()) {
    throw new Error("Unprocessed data remaining after Evaluate handling");
  }

  return null;
}

export {
  evaluate_from_rust_binary,
  handleBinaryResponse,
  sync_request_binary,
  MessageType,
  DROP_NATIVE_REF_FN_ID,
  CALL_EXPORT_FN_ID,
  prependJsToRustPrelude,
  allocateJsRequestId,
  pushMessageHeader,
  rustCallResponseChannels,
};
