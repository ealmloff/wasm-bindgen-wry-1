import { JSHeap } from "./heap.ts";
import "./ipc.ts";
import { evaluate_from_rust_binary } from "./ipc.ts";
import { RawJsFunction, setFunctionRegistry } from "./function_registry.ts";
import { TypeClass } from "./types.ts";
import { rustExports } from "./rust_exports.ts";

window.setFunctionRegistry = setFunctionRegistry;
window.evaluate_from_rust_binary = evaluate_from_rust_binary;
window.jsHeap = new JSHeap();
window.rustExports = rustExports;
window.__wryCallExport = rustExports.callExport;
window.__wryCallTypedExport = rustExports.callTypedExport;
window.__wryParseExportSignature = rustExports.parseExportSignature;

declare global {
  interface ExportSignature {
    paramTypes: TypeClass[];
    returnType: TypeClass;
  }

  interface Window {
    setFunctionRegistry: (registry: RawJsFunction[]) => void;
    evaluate_from_rust_binary: (dataBase64: string) => unknown;
    jsHeap: JSHeap;
    rustExports: typeof rustExports;
    __wryCallExport: (exportName: string, ...args: any[]) => unknown;
    __wryCallTypedExport: (
      exportName: string,
      signature: ExportSignature,
      ...args: any[]
    ) => unknown;
    __wryParseExportSignature: (
      signatureBytes: Uint8Array | ArrayLike<number>
    ) => ExportSignature;
  }
}
