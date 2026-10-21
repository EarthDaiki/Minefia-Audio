export type AudioInfo = {
  path: string;
  name: string;
  sizeBytes: number;
  durationSeconds: number;
  codec: string;
  sampleRate: number | null;
  channels: number | null;
  container: string;
  isVideo: boolean;
  audioTracks: AudioTrackInfo[];
  contextPrompt?: string | null;
};

export type SavedTranscript = { id: string; name: string; date: string; text: string; kind: "transcript" | "enhanced" | "summary" | "translation" | "summary_translation"; path: string };
export type LibraryFile = { path: string; name: string; date: string; sizeBytes: number; isReference: boolean; contextPrompt?: string | null };

export type AudioTrackInfo = {
  streamIndex: number;
  codec: string;
  sampleRate: number | null;
  channels: number | null;
  language: string | null;
  title: string | null;
  requiresConversion: boolean;
};

export type TranscriptionEvent = {
  kind: "preparing" | "uploading" | "retrying" | "prompt" | "delta" | "chunk_done" | "done" | "error";
  message?: string;
  delta?: string;
  text?: string;
  chunk?: number;
  totalChunks?: number;
  prompt?: string | null;
  code?: string;
};

export type TranslationEvent = {
  kind: "delta" | "done";
  delta?: string;
  chunk: number;
  totalChunks: number;
};

export type SummaryEvent = {
  kind: "start" | "delta" | "done";
  mode: "notes" | "summary" | "question";
  delta?: string;
};

export type SystemAudioEvent = {
  kind: "status" | "queue" | "warning" | "transcript" | "error" | "done";
  elapsedSeconds: number;
  chunkSeconds: number;
  completedChunks: number;
  queuedChunks: number;
  activeRequests: number;
  waitingForSilence: boolean;
  forceSplitInSeconds: number;
  estimatedCost: number;
  chunk?: number;
  text?: string;
  message?: string;
  code?: string;
};

export type RealtimeAudioEvent = {
  kind: "status" | "transcript" | "reconnecting" | "error" | "done";
  elapsedSeconds: number;
  estimatedCost: number;
  transcript?: string;
  partial: boolean;
  message?: string;
  code?: string;
};

export type InputDevice = { id: string; name: string };

export type OnlineMediaInfo = {
  url: string;
  title: string;
  filenameTitle?: string | null;
  description?: string | null;
  sourceKind: string;
  formatId: string;
  formatLabel: string;
  mediaUrl?: string | null;
  audioOnly: boolean;
  hasVideo: boolean;
  audioCodec: string;
  audioBitrateKbps?: number | null;
  sampleRateHz?: number | null;
  videoHeight?: number | null;
  requiresConversion: boolean;
  availableFormats: OnlineMediaFormat[];
};

export type OnlineMediaFormat = {
  formatId: string;
  formatLabel: string;
  mediaUrl?: string | null;
  audioOnly: boolean;
  hasVideo: boolean;
  audioCodec: string;
  audioBitrateKbps?: number | null;
  sampleRateHz?: number | null;
  videoHeight?: number | null;
  requiresConversion: boolean;
  recommended: boolean;
};

export type PageMediaCandidate = {
  url: string;
  kind: string;
  contentType?: string | null;
  durationSeconds?: number | null;
  label: string;
};

export type PageMediaInfo = {
  pageUrl: string;
  pageTitle: string;
  media: PageMediaCandidate[];
  masterPlaylists: string[];
};

export type TranscribeRequest = {
  path: string;
  compressAudio: boolean;
  audioStreamIndex: number;
  allowReencode: boolean;
  model: string;
  parallelRequests: number;
  jobId: string;
  contextPrompt?: string | null;
};

export type ResumableJob = {
  id: string;
  sourceName: string;
  model: string;
  completedChunks: number;
  totalChunks: number;
};

export type ApiKeySummary = {
  id: string;
  label: string;
  masked: string;
  isSelected: boolean;
};

export type TranscriptionModel = {
  id: "gpt-transcribe" | "gpt-4o-transcribe" | "gpt-4o-mini-transcribe" | "gpt-4o-transcribe-diarize";
  name: string;
  tag: string;
  description: string;
  pricePerMinute: number;
  pricingLabel?: string;
};

export type TranslationModel = {
  id: "gpt-6-luna" | "gpt-6.1-sol";
  name: string;
  tag: string;
  description: string;
  reasoningEffort: "none" | "low";
  inputPricePerMillion: number;
  outputPricePerMillion: number;
};

export type AvailableModels = {
  transcription: string[];
  translation: string[];
  realtime: string[];
};

export type TranscriptVersion = {
  language: string;
  text: string;
};
