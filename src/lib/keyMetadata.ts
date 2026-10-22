export const OPENAI_API_KEY_ID_STORAGE = "minefia-openai-api-key-id";

/** Store only OpenAI's non-secret key identifier in browser storage. */
export function rememberOpenAIKeyId(id: string | null | undefined) {
  if (id) localStorage.setItem(OPENAI_API_KEY_ID_STORAGE, id);
  else localStorage.removeItem(OPENAI_API_KEY_ID_STORAGE);
}
