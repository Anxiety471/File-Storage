import { hashFile } from "./checksum";
self.onmessage = async (event: MessageEvent<File>) => {
  try { self.postMessage({ hash: await hashFile(event.data) }); }
  catch (error) { self.postMessage({ error: error instanceof Error ? error.message : String(error) }); }
};
