import { sha256 } from "@noble/hashes/sha2.js";

/** Incremental hashing bounds memory even for 4 GiB uploads. */
export async function hashFile(file: Blob): Promise<string> {
  const hash = sha256.create();
  for (let offset = 0; offset < file.size; offset += 4 * 1024 * 1024) {
    hash.update(new Uint8Array(await file.slice(offset, offset + 4 * 1024 * 1024).arrayBuffer()));
  }
  return Array.from(hash.digest(), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** Hash off the UI thread; cancellation terminates both reading and computation. */
export function uploadChecksum(file: File, signal?: AbortSignal): Promise<string> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) { reject(new DOMException("Upload paused", "AbortError")); return; }
    const worker = new Worker(new URL("./checksum.worker.ts", import.meta.url), { type: "module" });
    const cleanup = () => { worker.terminate(); signal?.removeEventListener("abort", abort); };
    const abort = () => { cleanup(); reject(new DOMException("Upload paused", "AbortError")); };
    signal?.addEventListener("abort", abort, { once: true });
    worker.onmessage = (event: MessageEvent<{ hash?: string; error?: string }>) => {
      cleanup();
      if (event.data.hash) resolve(event.data.hash);
      else reject(new Error(event.data.error || "File verification failed"));
    };
    worker.onerror = (event) => { cleanup(); reject(new Error(event.message || "File verification failed")); };
    worker.postMessage(file);
  });
}
