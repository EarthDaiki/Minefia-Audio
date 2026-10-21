import type { TranscriptionModel, TranslationModel } from "../types";

export const TRANSCRIPTION_MODELS: TranscriptionModel[] = [
  {
    id: "gpt-transcribe",
    name: "GPT Transcribe",
    tag: "Recommended",
    description: "High-accuracy model for file transcription, with strong multilingual and domain-term handling.",
    pricePerMinute: 0.0045,
  },
  {
    id: "gpt-4o-transcribe",
    name: "GPT-4o Transcribe",
    tag: "Accurate",
    description: "Reliable GPT-4o speech recognition with improved accuracy over the original Whisper models. API shutdown: 2027-02-26.",
    pricePerMinute: 0.006,
  },
  {
    id: "gpt-4o-mini-transcribe",
    name: "GPT-4o Mini Transcribe",
    tag: "Fast",
    description: "A lighter, faster option for everyday transcription while retaining strong language recognition. API shutdown: 2027-02-26.",
    pricePerMinute: 0.003,
  },
  {
    id: "gpt-4o-transcribe-diarize",
    name: "GPT-4o Transcribe Diarize",
    tag: "Speaker labels",
    description: "Transcribes speech and labels each speaker. Use Standard Mode; Live Mode does not support diarization. Labels may reset at chunk boundaries. API shutdown: 2027-02-26.",
    pricePerMinute: 0,
    pricingLabel: "Usage-based",
  },
];

export const TRANSLATION_MODELS: TranslationModel[] = [
  {
    id: "gpt-6-luna",
    name: "GPT-6 Luna",
    tag: "Default",
    description: "Fast, economical translation for everyday transcripts and high-volume work.",
    reasoningEffort: "none",
    inputPricePerMillion: 0.1,
    outputPricePerMillion: 0.5,
  },
  {
    id: "gpt-6.1-sol",
    name: "GPT-6.1 Sol",
    tag: "Quality",
    description: "Quality-focused option for nuance, specialized terminology, and difficult context.",
    reasoningEffort: "low",
    inputPricePerMillion: 2,
    outputPricePerMillion: 10,
  },
];
