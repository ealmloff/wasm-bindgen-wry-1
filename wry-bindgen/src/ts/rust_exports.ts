import { DataEncoder } from "./encoding";
import { handleBinaryResponse, MessageType, sync_request_binary, CALL_EXPORT_FN_ID } from "./ipc";
import { NullType, parseTypeDef, TypeClass } from "./types";

interface ExportSignature {
  paramTypes: TypeClass[];
  returnType: TypeClass;
}

/**
 * FinalizationRegistry to notify Rust when exported object wrappers are GC'd.
 * The callback sends a drop message to Rust with the object handle.
 */
const exportRegistry = new FinalizationRegistry<{ handle: number; className: string }>((info) => {
  // Build Evaluate message to drop the object: call ClassName::__drop with handle
  const encoder = new DataEncoder();
  encoder.pushU8(MessageType.Evaluate);
  encoder.pushU32(CALL_EXPORT_FN_ID);
  // Encode the export name as a string
  const dropName = `${info.className}::__drop`;
  encoder.pushStr(dropName);
  // Encode the handle as u32
  encoder.pushU32(info.handle);

  const response = sync_request_binary(`/__wbg__/handler`, encoder.finalize());
  handleBinaryResponse(response);
});

/**
 * Call an exported Rust method by name.
 * This is exposed as window.__wryCallExport for generated class methods to use.
 */
function callExport(exportName: string, ...args: any[]): any {
  window.jsHeap.pushBorrowFrame();

  const encoder = new DataEncoder();
  encoder.pushU8(MessageType.Evaluate);
  encoder.pushU32(CALL_EXPORT_FN_ID);
  // Encode the export name as a string
  encoder.pushStr(exportName);
  // Encode arguments - for now, we assume they're already u32 handles or primitives
  for (const arg of args) {
    if (typeof arg === "number") {
      encoder.pushU32(arg);
    } else {
      throw new Error(`Unsupported argument type: ${typeof arg}`);
    }
  }

  const response = sync_request_binary(`/__wbg__/handler`, encoder.finalize());
  const decoder = handleBinaryResponse(response);

  window.jsHeap.popBorrowFrame();

  // If we have response data, try to decode it
  // For now, try to decode as i32 if there's u32 data available
  if (decoder && decoder.hasMoreU32()) {
    return decoder.takeI32();
  }

  return undefined;
}

/**
 * Parse an encoded export signature emitted by the Rust macro layer.
 *
 * Format: [param_count: u8][param typedefs...][return typedef]
 */
function parseExportSignature(
  signatureBytes: Uint8Array | ArrayLike<number>
): ExportSignature {
  const bytes = ArrayBuffer.isView(signatureBytes)
    ? new Uint8Array(
        signatureBytes.buffer,
        signatureBytes.byteOffset,
        signatureBytes.byteLength
      )
    : Uint8Array.from(signatureBytes);
  const offset = { value: 0 };
  const paramCount = bytes[offset.value++];
  const paramTypes: TypeClass[] = [];
  for (let i = 0; i < paramCount; i++) {
    paramTypes.push(parseTypeDef(bytes, offset));
  }
  const returnType = parseTypeDef(bytes, offset);
  if (offset.value !== bytes.length) {
    throw new Error("Unprocessed data remaining after export signature parsing");
  }
  return { paramTypes, returnType };
}

/**
 * Call an exported Rust method using the encoded type signature to marshal
 * arguments and decode the return value.
 */
function callTypedExport(
  exportName: string,
  signature: ExportSignature,
  ...args: any[]
): any {
  window.jsHeap.pushBorrowFrame();

  try {
    if (args.length !== signature.paramTypes.length) {
      throw new Error(
        `Expected ${signature.paramTypes.length} export arguments for ${exportName}, got ${args.length}`
      );
    }

    const encoder = new DataEncoder();
    encoder.pushU8(MessageType.Evaluate);
    encoder.pushU32(CALL_EXPORT_FN_ID);
    encoder.pushStr(exportName);

    for (let i = 0; i < signature.paramTypes.length; i++) {
      signature.paramTypes[i].encode(encoder, args[i]);
    }

    const response = sync_request_binary(`/__wbg__/handler`, encoder.finalize());
    const decoder = handleBinaryResponse(response);

    if (!decoder) {
      return undefined;
    }

    if (signature.returnType instanceof NullType) {
      if (!decoder.isEmpty()) {
        throw new Error(`Unexpected data for unit export return: ${exportName}`);
      }
      return undefined;
    }

    const result = signature.returnType.decode(decoder);
    if (!decoder.isEmpty()) {
      throw new Error(`Unprocessed data remaining after export call: ${exportName}`);
    }
    return result;
  } finally {
    window.jsHeap.popBorrowFrame();
  }
}

/**
 * Create a JavaScript wrapper object for a Rust exported struct.
 * Uses the generated class from JsClassSpec if available, otherwise falls back to Proxy.
 */
function createWrapper(handle: number, className: string): object {
  // Try to use the generated class if available
  const ClassConstructor = (window as any)[className];
  if (ClassConstructor && typeof ClassConstructor.__wrap === 'function') {
    return ClassConstructor.__wrap(handle);
  }

  // Fallback: Create wrapper object with the handle stored (legacy Proxy approach)
  // This will be removed once all classes are migrated to use JsClassSpec
  const wrapper: any = {
    __handle: handle,
    __className: className,
  };

  // Create a Proxy to intercept method calls and property access
  const proxy = new Proxy(wrapper, {
    get(target, prop) {
      if (prop === "__handle" || prop === "__className") {
        return target[prop];
      }
      // Skip Symbol properties and common JS properties
      if (typeof prop === "symbol" || prop === "then" || prop === "toJSON") {
        return undefined;
      }
      // Return a function that calls the Rust export when invoked
      return (...args: any[]) => {
        const exportName = `${className}::${String(prop)}`;
        // Pass the handle as the first argument (for self methods)
        return callExport(exportName, handle, ...args);
      };
    },
  });

  // Register for GC notification
  exportRegistry.register(proxy, { handle, className });

  return proxy;
}

// Expose callExport and exportRegistry as window globals for generated classes to use
(window as any).__wryCallExport = callExport;
(window as any).__wryCallTypedExport = callTypedExport;
(window as any).__wryParseExportSignature = parseExportSignature;
(window as any).__wryExportRegistry = exportRegistry;

/**
 * RustExports manager - provides wrapper creation for exported structs.
 */
const rustExports = {
  createWrapper,
  callExport,
  callTypedExport,
  parseExportSignature,
};

export {
  rustExports,
  createWrapper,
  callExport,
  callTypedExport,
  parseExportSignature,
};
