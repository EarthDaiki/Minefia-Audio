type StructuredError = { code?: unknown; message?: unknown };

export function errorMessage(value: unknown): string {
  let parsed = value;
  if (typeof value === "string") {
    try { parsed = JSON.parse(value) as unknown; } catch { return value; }
  }
  if (parsed && typeof parsed === "object") {
    const error = parsed as StructuredError;
    const message = typeof error.message === "string" ? error.message : "The operation failed.";
    return typeof error.code === "string" ? `[${error.code}] ${message}` : message;
  }
  return String(value);
}
