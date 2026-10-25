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

export type SavedTranscript = { id: string; name: string; date: string; text: string; kind: "transcript" | "enhanced" | "summary" | "notes" | "study_notes" | "translation" | "summary_translation" | "notes_translation"; path: string };
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
  taskId: string;
  taskSlot: "file" | "online" | "system";
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
  kind: "delta" | "done" | "prompt_cache";
  delta?: string;
  chunk: number;
  totalChunks: number;
  cachedTokens?: number;
};

export type SummaryEvent = {
  kind: "start" | "delta" | "done" | "prompt_cache";
  mode: "notes" | "summary" | "question";
  delta?: string;
  cachedTokens?: number;
};

export type ProjectCosts = { projectIds: string[]; days: { date: string; models: { model: string; amountUsd: number }[]; totalUsd: number }[]; totalUsd: number; currency: string };

export type SystemAudioEvent = {
  kind: "status" | "queue" | "warning" | "transcript" | "capture_stopped" | "error" | "done";
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

export type InputDevice = {
  id: string;
  name: string;
  supportedSampleRates: number[];
  maxSupportedSampleRate: number | null;
  supportedChannels: number[];
  defaultSampleRate: number | null;
  defaultChannels: number | null;
};

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
  language?: string | null;
  audioBitrateKbps?: number | null;
  sampleRateHz?: number | null;
  videoHeight?: number | null;
  requiresConversion: boolean;
  availableFormats: OnlineMediaFormat[];
  captions: OnlineCaption[];
};

export type OnlineCaption = {
  id: string;
  language: string;
  label: string;
  automatic: boolean;
};

export type OnlineMediaFormat = {
  formatId: string;
  formatLabel: string;
  mediaUrl?: string | null;
  audioOnly: boolean;
  hasVideo: boolean;
  audioCodec: string;
  language?: string | null;
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
  audioQuality: "low" | "standard" | "high" | "best";
  audioStreamIndex: number;
  allowReencode: boolean;
  model: string;
  parallelRequests: number;
  jobId: string;
  taskId: string;
  taskSlot: "file" | "online";
  taskComplete: boolean;
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
  openaiKeyId?: string | null;
};

export type AdminApiKeySummary = ApiKeySummary;
export type OpenAIProjectSummary = { id: string; name: string; status: string };
export type MinefiaServiceAccount = { serviceAccountId: string; serviceAccountName: string; apiKeyId: string };
export type MinefiaProject = { projectId: string; projectName: string; createdByApp: boolean; serviceAccounts: MinefiaServiceAccount[] };
export type CreatedProjectKey = { project: MinefiaProject; key: ApiKeySummary };

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
