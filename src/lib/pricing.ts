import { TRANSCRIPTION_MODELS, TRANSLATION_MODELS } from "./models";

const PART_TARGET_BYTES = 19_000_000;
const PART_TARGET_SECONDS = 5 * 60;
const COMPRESSED_BYTES_PER_SECOND = 64_000 / 8;
const TRANSLATION_CHARS_PER_CHUNK = 12_000;
const APPROX_CHARS_PER_TOKEN = 3;
const INSTRUCTION_TOKENS_PER_CHUNK = 60;

export type TranslationCostEstimate = {
  total: number;
  inputTokens: number;
  outputTokens: number;
};

export function estimateTranscriptionCost(modelId: string, durationSeconds: number): number {
  const model = TRANSCRIPTION_MODELS.find((item) => item.id === modelId);
  if (!model || model.pricingLabel) return 0;
  return (durationSeconds / 60) * (model?.pricePerMinute ?? 0);
}

export function estimatePartCount(sizeBytes: number, durationSeconds: number, compressed: boolean): number {
  const effectiveSize = compressed ? durationSeconds * COMPRESSED_BYTES_PER_SECOND : sizeBytes;
  return Math.max(
    1,
    Math.ceil(effectiveSize / PART_TARGET_BYTES),
    Math.ceil(durationSeconds / PART_TARGET_SECONDS),
  );
}

/**
 * Gives a planning estimate before the API returns exact usage. Translation
 * length varies by language, so input and output both use a conservative
 * character-to-token approximation.
 */
export function estimateTranslationCost(modelId: string, text: string): TranslationCostEstimate {
  const model = TRANSLATION_MODELS.find((item) => item.id === modelId);
  if (!model || !text) return { total: 0, inputTokens: 0, outputTokens: 0 };

  const characters = Array.from(text).length;
  const chunks = Math.max(1, Math.ceil(characters / TRANSLATION_CHARS_PER_CHUNK));
  const inputTokens = Math.ceil(characters / APPROX_CHARS_PER_TOKEN) + chunks * INSTRUCTION_TOKENS_PER_CHUNK;
  const outputTokens = Math.ceil(characters / APPROX_CHARS_PER_TOKEN);
  const total = (
    inputTokens * model.inputPricePerMillion
    + outputTokens * model.outputPricePerMillion
  ) / 1_000_000;

  return { total, inputTokens, outputTokens };
}

export function formatUsd(value: number): string {
  if (value > 0 && value < 0.001) return "< $0.001";
  if (value < 0.01) return `$${value.toFixed(4)}`;
  return `$${value.toFixed(3)}`;
}
