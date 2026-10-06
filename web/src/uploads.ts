// Upload transport: one XHR per file (fetch can't report upload progress).
// The server streams the raw body to disk, so no multipart framing is used.

import type { WireAttachment } from "./state";

export interface UploadHandle {
  done: Promise<WireAttachment>;
  abort: () => void;
}

// The server answers failures with `{"error": "..."}`; anything else (a proxy
// error page) falls back to the status code.
function errorMessage(status: number, body: string): string {
  try {
    const parsed = JSON.parse(body) as { error?: unknown };
    if (typeof parsed.error === "string" && parsed.error) return parsed.error;
  } catch {
    // not JSON — fall through
  }
  return `Upload failed (HTTP ${status})`;
}

/** `onProgress` receives the 0..1 fraction of the request body sent. */
export function uploadFile(
  convo: string,
  file: File,
  onProgress: (fraction: number) => void,
): UploadHandle {
  const xhr = new XMLHttpRequest();
  const done = new Promise<WireAttachment>((resolve, reject) => {
    xhr.upload.onprogress = (e) => {
      if (e.total > 0) onProgress(e.loaded / e.total);
    };
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        try {
          resolve(JSON.parse(xhr.responseText) as WireAttachment);
        } catch {
          reject(new Error("Upload failed (bad server response)"));
        }
      } else {
        reject(new Error(errorMessage(xhr.status, xhr.responseText)));
      }
    };
    xhr.onerror = () => reject(new Error("Upload failed — remove and retry"));
    xhr.onabort = () => reject(new Error("Upload cancelled"));
  });
  xhr.open(
    "POST",
    `/api/uploads?convo=${encodeURIComponent(convo)}&name=${encodeURIComponent(file.name)}`,
  );
  xhr.setRequestHeader("Content-Type", "application/octet-stream");
  xhr.send(file);
  return { done, abort: () => xhr.abort() };
}
