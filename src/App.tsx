import { useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open, save } from "@tauri-apps/plugin-dialog";
import {
  ArrowRight,
  AudioLines,
  Check,
  CircleAlert,
  CircleDollarSign,
  ChevronDown,
  ChevronUp,
  MessageCircle,
  Download,
  FileAudio,
  FileText,
  Globe,
  FolderOpen,
  KeyRound,
  Loader2,
  Languages,
  MonitorSpeaker,
  Radio,
  RefreshCw,
  Settings,
  Sparkles,
  Upload,
  X,
  Zap,
} from "lucide-react";
import { SettingsModal } from "./components/SettingsModal";
import { LibraryModal } from "./components/LibraryModal";
import { DownloadModal } from "./components/DownloadModal";
import { ApiAccessGate } from "./components/ApiAccessGate";
import { ConfirmationModal } from "./components/ConfirmationModal";
import { UsageModal } from "./components/UsageModal";
import { formatBytes, formatDuration } from "./lib/format";
import { TRANSLATION_LANGUAGES } from "./lib/languages";
import { TRANSCRIPTION_MODELS, TRANSLATION_MODELS } from "./lib/models";
import { estimatePartCount, estimateTranscriptionCost, estimateTranslationCost, formatUsd } from "./lib/pricing";
import { splitReadableSections } from "./lib/textSections";
import { errorMessage } from "./lib/errors";
import { rememberOpenAIKeyId } from "./lib/keyMetadata";
import type { AdminApiKeySummary, ApiKeySummary, AudioInfo, AvailableModels, InputDevice, LibraryFile, OnlineMediaInfo, PageMediaInfo, ProjectCosts, RealtimeAudioEvent, ResumableJob, SavedTranscript, SummaryEvent, SystemAudioEvent, TranscriptVersion, TranscriptionEvent, TranscribeRequest, TranslationEvent } from "./types";

type OnlineDownloadProgressEvent = {
  stage: string;
  completed: number;
  total: number;
  message: string;
  downloadedBytes?: number;
  totalBytes?: number;
};

type TechnicalLogEntry = { taskId: string; taskName: string; timestamp: string; category: string; message: string };
type TechnicalTask = { id: string; name: string };
type SourceMode = "file" | "online" | "system";
const CAPTURE_QUALITY_PRESETS = [
  { key: "low", sampleRate: 16_000, bitrate: 32, label: "Low" },
  { key: "standard", sampleRate: 16_000, bitrate: 64, label: "Standard" },
  { key: "high", sampleRate: 24_000, bitrate: 96, label: "High" },
  { key: "best", sampleRate: 48_000, bitrate: 128, label: "Best" },
] as const;

function transcriptionRateLabel(model: (typeof TRANSCRIPTION_MODELS)[number]): string {
  return model.pricingLabel ?? `$${model.pricePerMinute}/min`;
}

function TranscriptSection({ section, index }: { section: string; index: number }) {
  const [open, setOpen] = useState(index === 0);
  return (
    <details open={open} onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary><span>Section {index + 1}</span><small>{section.length.toLocaleString()} characters</small></summary>
      <p>{section}</p>
    </details>
  );
}

function MarkdownSummary({ text }: { text: string }) {
  const inline = (value: string) => value.split(/(\*\*[^*]+\*\*|`[^`]+`|\*[^*]+\*|\[[^\]]+\]\([^)]+\))/g).map((part, index) => {
    if (part.startsWith("**") && part.endsWith("**")) return <strong key={index}>{part.slice(2, -2)}</strong>;
    if (part.startsWith("`") && part.endsWith("`")) return <code key={index}>{part.slice(1, -1)}</code>;
    if (part.startsWith("*") && part.endsWith("*")) return <em key={index}>{part.slice(1, -1)}</em>;
    const link = part.match(/^\[([^\]]+)\]\(([^)]+)\)$/);
    if (link) return <a key={index} href={link[2]} target="_blank" rel="noreferrer">{link[1]}</a>;
    return part;
  });
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  const blocks: React.ReactNode[] = [];
  for (let index = 0; index < lines.length;) {
    const line = lines[index];
    if (!line.trim()) { blocks.push(<div className="markdown-gap" key={`gap-${index}`} />); index += 1; continue; }
    const heading = line.match(/^(#{1,3})\s+(.+)$/);
    if (heading) {
      const Tag = heading[1].length === 1 ? "h3" : heading[1].length === 2 ? "h4" : "h5";
      blocks.push(<Tag key={`heading-${index}`}>{inline(heading[2])}</Tag>); index += 1; continue;
    }
    const listItem = line.match(/^\s*(?:[-*+]\s+|\d+[.)]\s+)(.+)$/);
    if (listItem) {
      const items: React.ReactNode[] = [];
      while (index < lines.length) {
        const match = lines[index].match(/^\s*(?:[-*+]\s+|\d+[.)]\s+)(.+)$/);
        if (!match) break;
        items.push(<li key={`li-${index}`}>{inline(match[1])}</li>);
        index += 1;
      }
      blocks.push(<ul key={`list-${index}`}>{items}</ul>); continue;
    }
    const quote = line.match(/^>\s?(.*)$/);
    if (quote) { blocks.push(<blockquote key={`quote-${index}`}>{inline(quote[1])}</blockquote>); index += 1; continue; }
    blocks.push(<p key={`paragraph-${index}`}>{inline(line)}</p>); index += 1;
  }
  return <div className="markdown-summary">{blocks}</div>;
}

export default function App() {
  const [sourceMode, setSourceMode] = useState<"file" | "online" | "system">("file");
  const [onlineInputKind, setOnlineInputKind] = useState<"url" | "hls" | "web">("url");
  const [parallelRequests, setParallelRequests] = useState(() => {
    const value = Number(localStorage.getItem("parallel-requests") ?? "2");
    return Number.isInteger(value) && value >= 1 && value <= 4 ? value : 2;
  });
  const [captureQuality, setCaptureQuality] = useState<"low" | "standard" | "high" | "best">(() => {
    const value = localStorage.getItem("capture-quality");
    return value === "low" || value === "standard" || value === "high" || value === "best" ? value : "standard";
  });
  const [compactEnabled, setCompactEnabled] = useState(() => localStorage.getItem("compact-enabled") !== "false");
  const [compactChangeDeferred, setCompactChangeDeferred] = useState(false);
  const [technicalErrors, setTechnicalErrors] = useState(() => localStorage.getItem("technical-errors") === "true");
  const [technicalDetails, setTechnicalDetails] = useState(() => localStorage.getItem("technical-details") === "true");
  const [technicalLogsBySlot, setTechnicalLogsBySlot] = useState<Record<SourceMode, TechnicalLogEntry[]>>({ file: [], online: [], system: [] });
  const [technicalTaskNamesBySlot, setTechnicalTaskNamesBySlot] = useState<Record<SourceMode, string>>({ file: "", online: "", system: "" });
  const technicalTaskRefs = useRef<Record<SourceMode, TechnicalTask | null>>({ file: null, online: null, system: null });
  const lastTechnicalLogRefs = useRef<Record<SourceMode, { signature: string; at: number } | null>>({ file: null, online: null, system: null });
  const lastLoggedDownloadProgress = useRef({ stage: "", bytes: 0, partMark: -1, percentMark: -1 });
  const [headlessBrowser, setHeadlessBrowser] = useState(() => localStorage.getItem("headless-browser") === "true");
  const [resumableJobs, setResumableJobs] = useState<ResumableJob[]>([]);
  const [captureSource, setCaptureSource] = useState<"system" | "microphone" | "both">("system");
  const [captureMode, setCaptureMode] = useState<"standard" | "live">("standard");
  const [inputDevices, setInputDevices] = useState<InputDevice[]>([]);
  const [inputDevicesLoading, setInputDevicesLoading] = useState(false);
  const [selectedInputDevice, setSelectedInputDevice] = useState("");
  const selectedCaptureInputDevice = inputDevices.find((device) => device.id === selectedInputDevice);
  const microphoneRateConstraint = captureSource !== "system";
  const microphoneMaxSampleRate = selectedCaptureInputDevice?.maxSupportedSampleRate
    ?? selectedCaptureInputDevice?.defaultSampleRate
    ?? Math.max(0, ...(selectedCaptureInputDevice?.supportedSampleRates ?? []));
  const availableCaptureQualities = microphoneRateConstraint
    ? CAPTURE_QUALITY_PRESETS.filter((preset) => preset.sampleRate <= microphoneMaxSampleRate)
        .map((preset) => preset.key)
    : CAPTURE_QUALITY_PRESETS.map((preset) => preset.key);
  const captureQualityRestrictionNote = microphoneRateConstraint
    ? `${selectedCaptureInputDevice?.name ?? "The selected microphone"} supports up to ${microphoneMaxSampleRate ? `${microphoneMaxSampleRate / 1000} kHz` : "no reported sample rate"}. Higher output presets are hidden for both PCM and AAC to avoid upsampling.`
    : "All recording presets are available for the current source and Compact settings.";
  const [inputLevel, setInputLevel] = useState(0);
  const [savedTranscripts, setSavedTranscripts] = useState<SavedTranscript[]>([]);
  const [libraryFiles, setLibraryFiles] = useState<LibraryFile[]>([]);
  const [pendingDelete, setPendingDelete] = useState<{ kind: "media" | "transcript"; id: string; name: string; isReference?: boolean } | null>(null);
  const [audio, setAudio] = useState<AudioInfo | null>(null);
  const [audioQueue, setAudioQueue] = useState<AudioInfo[]>([]);
  const audioQueueRef = useRef<AudioInfo[]>([]);
  const [audioOwner, setAudioOwner] = useState<"file" | "online">("file");
  const [selectedAudioStreamIndex, setSelectedAudioStreamIndex] = useState<number | null>(null);
  const [transcript, setTranscript] = useState("");
  const [originalTranscript, setOriginalTranscript] = useState("");
  const [enhancedTranscript, setEnhancedTranscript] = useState("");
  const [summaryMode, setSummaryMode] = useState(false);
  const [notesMode, setNotesMode] = useState(false);
  const [notesStyle, setNotesStyle] = useState<"meeting" | "study">("meeting");
  const [notesText, setNotesText] = useState("");
  const [notesTranslations, setNotesTranslations] = useState<Record<string, string>>({});
  const [notesTranslating, setNotesTranslating] = useState(false);
  const [summaryTranslationLive, setSummaryTranslationLive] = useState(false);
  const [summaryText, setSummaryText] = useState("");
  const [summaryDraft, setSummaryDraft] = useState("");
  const [summaryGenerating, setSummaryGenerating] = useState(false);
  const [summaryStatus, setSummaryStatus] = useState<"idle" | "started" | "ended" | "failed">("idle");
  const [summaryTranslations, setSummaryTranslations] = useState<Record<string, string>>({});
  const [summaryTranslating, setSummaryTranslating] = useState(false);
  const [summaryTranslationDraft, setSummaryTranslationDraft] = useState("");
  const [question, setQuestion] = useState("");
  const [questionMessages, setQuestionMessages] = useState<{ role: "user" | "assistant"; text: string }[]>([]);
  const [questionBusy, setQuestionBusy] = useState(false);
  const [questionDraft, setQuestionDraft] = useState("");
  const [translations, setTranslations] = useState<Record<string, string>>({});
  const [activeLanguage, setActiveLanguage] = useState("Original");
  const [targetLanguage, setTargetLanguage] = useState("Japanese");
  const [customLanguage, setCustomLanguage] = useState("");
  const [translating, setTranslating] = useState(false);
  const [translationEnabled, setTranslationEnabled] = useState(false);
  const [enhancementRunning, setEnhancementRunning] = useState(false);
  const [transcriptionContextPrompt, setTranscriptionContextPrompt] = useState("");
  const [transcriptionPrompts, setTranscriptionPrompts] = useState<Record<number, string | null>>({});
  const [notesUpdating, setNotesUpdating] = useState(false);
  const notesUpdatesPending = useRef(0);
  const summaryLogSlotRef = useRef<SourceMode>("file");
  const translationLogSlotRef = useRef<SourceMode>("file");
  const [translationModel, setTranslationModel] = useState(() => {
    const stored = localStorage.getItem("translation-model");
    return TRANSLATION_MODELS.some((item) => item.id === stored) ? stored! : "gpt-6-luna";
  });
  const [transcriptTranslationModel, setTranscriptTranslationModel] = useState(() => {
    const stored = localStorage.getItem("transcript-translation-model");
    return TRANSLATION_MODELS.some((item) => item.id === stored) ? stored! : "gpt-6-luna";
  });
  const [summaryTranslationModel, setSummaryTranslationModel] = useState(() => {
    const stored = localStorage.getItem("summary-translation-model");
    return TRANSLATION_MODELS.some((item) => item.id === stored) ? stored! : "gpt-6-luna";
  });
  const [questionModel, setQuestionModel] = useState(() => {
    const stored = localStorage.getItem("question-model");
    return TRANSLATION_MODELS.some((item) => item.id === stored) ? stored! : "gpt-6-luna";
  });
  const [model, setModel] = useState(() => {
    const stored = localStorage.getItem("transcription-model");
    return TRANSCRIPTION_MODELS.some((item) => item.id === stored) ? stored! : "gpt-transcribe";
  });
  const [systemModel, setSystemModel] = useState(() => {
    const stored = localStorage.getItem("system-transcription-model");
    return TRANSCRIPTION_MODELS.some((item) => item.id === stored) ? stored! : "gpt-transcribe";
  });
  const [apiKeys, setApiKeys] = useState<ApiKeySummary[]>([]);
  const [adminApiKeys, setAdminApiKeys] = useState<AdminApiKeySummary[]>([]);
  const [projectUsageReady, setProjectUsageReady] = useState(false);
  const [availableModelIds, setAvailableModelIds] = useState<string[]>([]);
  const [availableTranslationModelIds, setAvailableTranslationModelIds] = useState<string[]>([]);
  const [modelsLoading, setModelsLoading] = useState(true);
  const [modelLoadError, setModelLoadError] = useState("");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [libraryOpen, setLibraryOpen] = useState(false);
  const [usageOpen, setUsageOpen] = useState(false);
  const [projectCosts, setProjectCosts] = useState<ProjectCosts | null>(null);
  const [usageLoading, setUsageLoading] = useState(false);
  const [usageError, setUsageError] = useState("");
  const [localImporting, setLocalImporting] = useState(false);
  const [downloadOpen, setDownloadOpen] = useState(false);
  const [conversionOpen, setConversionOpen] = useState(false);
  const [largeDownloadOpen, setLargeDownloadOpen] = useState(false);
  const [onlineConversionApproved, setOnlineConversionApproved] = useState(false);
  const [pendingConversion, setPendingConversion] = useState<"file" | "online" | null>(null);
  const [onlineUrl, setOnlineUrl] = useState("");
  const [webPageInfo, setWebPageInfo] = useState<PageMediaInfo | null>(null);
  const [webPageInspecting, setWebPageInspecting] = useState(false);
  const [onlineMedia, setOnlineMedia] = useState<OnlineMediaInfo | null>(null);
  const [onlineShowAllSources, setOnlineShowAllSources] = useState(false);
  const [selectedOnlineCaptionId, setSelectedOnlineCaptionId] = useState("");
  const [onlineCaptionLoading, setOnlineCaptionLoading] = useState(false);
  const [onlineInspecting, setOnlineInspecting] = useState(false);
  const [onlineExtracting, setOnlineExtracting] = useState(false);
  const [onlineDownloadProgress, setOnlineDownloadProgress] = useState<OnlineDownloadProgressEvent | null>(null);
  const [onlineCancelRequested, setOnlineCancelRequested] = useState(false);
  const [onlineKeepVideo, setOnlineKeepVideo] = useState(false);
  const [downloadLanguage, setDownloadLanguage] = useState("Original");
  const [transcriptionBusyBySlot, setTranscriptionBusyBySlot] = useState<Record<"file" | "online", boolean>>({ file: false, online: false });
  const [transcriptionCancelBySlot, setTranscriptionCancelBySlot] = useState<Record<"file" | "online", boolean>>({ file: false, online: false });
  const busy = sourceMode !== "system" && transcriptionBusyBySlot[sourceMode];
  const transcriptionCancelRequested = sourceMode !== "system" && transcriptionCancelBySlot[sourceMode];
  const [completed, setCompleted] = useState(false);
  const [phase, setPhase] = useState("Choose an audio file to begin");
  const [progress, setProgress] = useState(0);
  const [error, setError] = useState("");
  const [dragging, setDragging] = useState(false);
  const [systemRecording, setSystemRecording] = useState(false);
  const [systemStopping, setSystemStopping] = useState(false);
  const [systemElapsed, setSystemElapsed] = useState(0);
  const [systemChunkElapsed, setSystemChunkElapsed] = useState(0);
  const [systemForceRemaining, setSystemForceRemaining] = useState(150);
  const [systemCompletedChunks, setSystemCompletedChunks] = useState(0);
  const [systemQueuedChunks, setSystemQueuedChunks] = useState(0);
  const [systemActiveRequests, setSystemActiveRequests] = useState(0);
  const [systemWaitingForSilence, setSystemWaitingForSilence] = useState(false);
  const [systemEstimatedCost, setSystemEstimatedCost] = useState(0);
  const [systemWarning, setSystemWarning] = useState("");
  const [liveRecording, setLiveRecording] = useState(false);
  const [liveStopping, setLiveStopping] = useState(false);
  const [liveElapsed, setLiveElapsed] = useState(0);
  const [liveEstimatedCost, setLiveEstimatedCost] = useState(0);
  const [liveMessage, setLiveMessage] = useState("");
  const [availableRealtimeModelIds, setAvailableRealtimeModelIds] = useState<string[]>([]);
  const transcriptParts = useRef<Record<number, string>>({});
  const completedParts = useRef<Set<number>>(new Set());
  const transcriptionTasks = useRef<Partial<Record<"file" | "online", {
    id: string; sessionId: string; sessionName: string; chunkOffset: number; totalChunks: number; fileIndex: number; fileCount: number; active: boolean;
    parts: Record<number, string>; complete: Set<number>; attempts: Record<number, number>; persistence: Promise<void>; phase: string; progress: number; error: string;
  }>>>({});
  const translatingLanguage = useRef("");
  const transcriptRef = useRef("");
  const sourceModeRef = useRef(sourceMode);
  const completedRef = useRef(completed);
  const summaryTextRef = useRef("");
  const summaryModeRef = useRef(false);
  const notesModeRef = useRef(false);
  const notesStyleRef = useRef<"meeting" | "study">("meeting");
  const summaryTranslationLiveRef = useRef(false);
  const notesTextRef = useRef("");
  const summaryModelRef = useRef(translationModel);
  const summaryTranslationModelRef = useRef(summaryTranslationModel);
  const targetLanguageRef = useRef(targetLanguage);
  const customLanguageRef = useRef(customLanguage);
  const summaryQueue = useRef<Promise<void>>(Promise.resolve());
  const liveFinalTranscript = useRef("");
  const livePersistedTranscript = useRef("");
  const systemCaptureFinalized = useRef(true);
  const transcriptSessionId = useRef("");
  const transcriptSessionName = useRef("Transcript");
  const transcriptAttempts = useRef<Record<number, number>>({});
  const transcriptionChunkOffset = useRef(0);
  const currentTranscriptionFile = useRef(1);
  const currentTranscriptionTotal = useRef(0);
  const batchTranscriptionActive = useRef(false);
  const transcriptPersistenceQueue = useRef<Promise<void>>(Promise.resolve());
  const liveTranscriptAttempt = useRef(0);
  const systemCommittedTranscript = useRef("");
  const systemCompletedChunksRef = useRef(0);
  const systemStreamedParts = useRef<Record<number, string>>({});
  const transcriptTabs = useRef<Record<"file" | "online" | "system", {
    transcript: string; originalTranscript: string; enhancedTranscript: string; summaryText: string; translations: Record<string, string>;
    summaryTranslations: Record<string, string>; questionMessages: { role: "user" | "assistant"; text: string }[];
    notesText: string; notesMode: boolean; notesStyle: "meeting" | "study"; notesTranslations: Record<string, string>; summaryTranslationLive: boolean;
    activeLanguage: string; completed: boolean; summaryMode: boolean; translationEnabled: boolean;
    contextPrompt: string; transcriptionPrompts: Record<number, string | null>;
    sessionId: string; sessionName: string;
  }>>({
    file: { transcript: "", originalTranscript: "", enhancedTranscript: "", summaryText: "", notesText: "", notesMode: false, notesStyle: "meeting", notesTranslations: {}, summaryTranslationLive: false, translations: {}, summaryTranslations: {}, questionMessages: [], activeLanguage: "Original", completed: false, summaryMode: false, translationEnabled: false, contextPrompt: "", transcriptionPrompts: {}, sessionId: "", sessionName: "Transcript" },
    online: { transcript: "", originalTranscript: "", enhancedTranscript: "", summaryText: "", notesText: "", notesMode: false, notesStyle: "meeting", notesTranslations: {}, summaryTranslationLive: false, translations: {}, summaryTranslations: {}, questionMessages: [], activeLanguage: "Original", completed: false, summaryMode: false, translationEnabled: false, contextPrompt: "", transcriptionPrompts: {}, sessionId: "", sessionName: "Transcript" },
    system: { transcript: "", originalTranscript: "", enhancedTranscript: "", summaryText: "", notesText: "", notesMode: false, notesStyle: "meeting", notesTranslations: {}, summaryTranslationLive: false, translations: {}, summaryTranslations: {}, questionMessages: [], activeLanguage: "Original", completed: false, summaryMode: false, translationEnabled: false, contextPrompt: "", transcriptionPrompts: {}, sessionId: "", sessionName: "Transcript" },
  });

  function switchSourceMode(next: "file" | "online" | "system") {
    if (next === sourceMode) return;
    transcriptTabs.current[sourceMode] = {
      transcript, originalTranscript, enhancedTranscript, summaryText, notesText, notesMode, notesStyle, notesTranslations, summaryTranslationLive, translations, summaryTranslations, questionMessages, activeLanguage, completed, summaryMode, translationEnabled,
      contextPrompt: transcriptionContextPrompt, transcriptionPrompts,
      sessionId: transcriptSessionId.current, sessionName: transcriptSessionName.current,
    };
    const saved = transcriptTabs.current[next];
    transcriptRef.current = saved.transcript;
    summaryTextRef.current = saved.summaryText;
    notesTextRef.current = saved.notesText;
    summaryModeRef.current = saved.summaryMode;
    transcriptSessionId.current = saved.sessionId;
    transcriptSessionName.current = saved.sessionName;
    setTranscript(saved.transcript);
    setOriginalTranscript(saved.originalTranscript);
    setEnhancedTranscript(saved.enhancedTranscript);
    setSummaryText(saved.summaryText);
    setNotesText(saved.notesText);
    setNotesMode(saved.notesMode);
    setNotesStyle(saved.notesStyle);
    notesStyleRef.current = saved.notesStyle;
    setNotesTranslations(saved.notesTranslations);
    notesModeRef.current = saved.notesMode;
    setSummaryTranslationLive(saved.summaryTranslationLive);
    summaryTranslationLiveRef.current = saved.summaryTranslationLive;
    setTranslations(saved.translations);
    setSummaryTranslations(saved.summaryTranslations);
    setQuestionMessages(saved.questionMessages);
    setActiveLanguage(saved.activeLanguage);
    setCompleted(saved.completed);
    setSummaryMode(saved.summaryMode);
    setTranslationEnabled(saved.translationEnabled);
    setTranscriptionContextPrompt(saved.contextPrompt);
    setTranscriptionPrompts(saved.transcriptionPrompts);
    sourceModeRef.current = next;
    setSourceMode(next);
    if (next !== "system") {
      const task = transcriptionTasks.current[next];
      if (task) { setPhase(task.phase); setProgress(task.progress); setError(task.error); }
    }
  }

  function updateContextPrompt(slot: SourceMode, value: string) {
    transcriptTabs.current[slot].contextPrompt = value;
    if (sourceModeRef.current === slot) setTranscriptionContextPrompt(value);
  }

  function clearPromptHistory(slot: SourceMode) {
    transcriptTabs.current[slot].transcriptionPrompts = {};
    if (sourceModeRef.current === slot) setTranscriptionPrompts({});
  }

  function rememberPrompt(slot: SourceMode, chunk: number, prompt: string | null) {
    transcriptTabs.current[slot].transcriptionPrompts = {
      ...transcriptTabs.current[slot].transcriptionPrompts,
      [chunk]: prompt,
    };
    if (sourceModeRef.current === slot) {
      setTranscriptionPrompts((current) => ({ ...current, [chunk]: prompt }));
    }
  }

  function startTranscriptSession(name: string) {
    transcriptSessionId.current = crypto.randomUUID();
    transcriptSessionName.current = name || "Transcript";
    clearPromptHistory(sourceModeRef.current);
    transcriptAttempts.current = {};
    liveTranscriptAttempt.current = 0;
    liveFinalTranscript.current = "";
    livePersistedTranscript.current = "";
    setOriginalTranscript("");
    setEnhancedTranscript("");
    setActiveLanguage("Original");
    return transcriptSessionId.current;
  }

  function renderSystemTranscriptPreview() {
    const committed = systemCommittedTranscript.current;
    const partial = systemStreamedParts.current[systemCompletedChunksRef.current + 1] ?? "";
    const preview = partial ? `${committed}${committed ? "\n" : ""}${partial}` : committed;
    const tab = transcriptTabs.current.system;
    tab.transcript = preview;
    tab.originalTranscript = preview;
    if (sourceModeRef.current === "system") {
      setTranscript(preview);
      setOriginalTranscript(preview);
    }
    return preview;
  }

  function commitSystemTranscriptChunk(chunk: number, text: string, completed: number) {
    const previous = systemCommittedTranscript.current;
    systemCommittedTranscript.current = `${previous}${previous ? "\n" : ""}${text}`;
    systemCompletedChunksRef.current = completed;
    delete systemStreamedParts.current[chunk];
    setSystemCompletedChunks(completed);
    return renderSystemTranscriptPreview();
  }

  function toggleCaptureInput(input: "system" | "microphone") {
    const hasSystem = captureSource === "system" || captureSource === "both";
    const hasMicrophone = captureSource === "microphone" || captureSource === "both";
    if (input === "system") {
      if (hasSystem && hasMicrophone) setCaptureSource("microphone");
      else if (hasSystem) setCaptureSource("microphone");
      else setCaptureSource("both");
    } else if (hasMicrophone && hasSystem) setCaptureSource("system");
    else if (hasMicrophone) setCaptureSource("system");
    else setCaptureSource("both");
  }

  function persistTranscriptFragment(chunk: number, text: string, attempt = transcriptAttempts.current[chunk] ?? 0) {
    const sessionId = transcriptSessionId.current;
    const sessionName = transcriptSessionName.current;
    if (!sessionId || !text) return;
    transcriptPersistenceQueue.current = transcriptPersistenceQueue.current
      .then(() => invoke("append_saved_transcript", { sessionId, chunk, attempt, name: sessionName, text }))
      .then(() => undefined)
      .catch((cause) => setError(`Transcript is visible but could not be saved: ${errorMessage(cause)}`));
  }

  function snapshotTranscriptFile() {
    const sessionId = transcriptSessionId.current;
    if (!sessionId) return;
    transcriptPersistenceQueue.current = transcriptPersistenceQueue.current
      .then(() => invoke("snapshot_saved_transcript", { sessionId, name: transcriptSessionName.current }))
      .then(() => undefined)
      .catch((cause) => setError(`Transcript text file could not be updated: ${errorMessage(cause)}`));
  }

  function cleanupSavedTranscriptChunks() {
    const sessionId = transcriptSessionId.current;
    if (!sessionId) return;
    transcriptPersistenceQueue.current = transcriptPersistenceQueue.current
      .then(() => invoke("cleanup_saved_transcript_chunks", { sessionId }))
      .then(() => undefined)
      .catch((cause) => setError(`Temporary transcript parts could not be cleaned up: ${errorMessage(cause)}`));
  }

  function replaceSavedTranscriptChunk(chunk: number, attempt: number, text: string) {
    const sessionId = transcriptSessionId.current;
    const name = transcriptSessionName.current;
    if (!sessionId) return;
    transcriptPersistenceQueue.current = transcriptPersistenceQueue.current
      .then(() => invoke("replace_saved_transcript_chunk", { sessionId, chunk, attempt, name, text }))
      .then(() => undefined)
      .catch((cause) => setError(`Transcript response could not be saved: ${errorMessage(cause)}`));
  }

  async function refreshSavedTranscripts() {
    try { setSavedTranscripts(await invoke<SavedTranscript[]>("list_saved_transcripts")); }
    catch (cause) { setError(errorMessage(cause)); }
  }

  async function refreshLibraryFiles() {
    try { setLibraryFiles(await invoke<LibraryFile[]>("list_library_files")); }
    catch (cause) { setError(errorMessage(cause)); }
  }

  async function refreshUsage() {
    setUsageLoading(true);
    setUsageError("");
    try { setProjectCosts(await invoke<ProjectCosts>("fetch_project_costs")); }
    catch (cause) { setUsageError(errorMessage(cause)); }
    finally { setUsageLoading(false); }
  }

  async function refreshAdminApiKeys() {
    try { setAdminApiKeys(await invoke<AdminApiKeySummary[]>("list_admin_api_keys")); }
    catch { setAdminApiKeys([]); }
  }

  async function refreshProjectUsage() {
    try {
      const ready = await invoke<boolean>("minefia_usage_ready");
      rememberOpenAIKeyId(null);
      setProjectUsageReady(ready);
      if (!ready) setUsageOpen(false);
    } catch {
      rememberOpenAIKeyId(null);
      setProjectUsageReady(false);
      setUsageOpen(false);
    }
  }

  async function refreshSettingsKeys() {
    await Promise.all([refreshApiKeys(), refreshAdminApiKeys(), refreshProjectUsage()]);
  }

  function isLocalTaskBusy() {
    return localImporting || transcriptionBusyBySlot.file || (sourceMode === "file" && (summaryGenerating || summaryTranslating || translating || questionBusy || enhancementRunning));
  }

  function recordTechnicalLog(category: string, message: string, slot: SourceMode = sourceModeRef.current) {
    const signature = `${category}\n${message}`;
    const nowMs = Date.now();
    const previous = lastTechnicalLogRefs.current[slot];
    if (previous?.signature === signature && nowMs - previous.at < 1_500) return;
    lastTechnicalLogRefs.current[slot] = { signature, at: nowMs };
    let task = technicalTaskRefs.current[slot];
    if (!task) {
      const now = new Date();
      const timestampId = now.toISOString().replace(/[-:]/g, "").replace(/\.\d{3}Z$/, "Z");
      task = { id: `${timestampId}_${crypto.randomUUID().slice(0, 8)}`, name: category };
      technicalTaskRefs.current[slot] = task;
      setTechnicalTaskNamesBySlot((current) => ({ ...current, [slot]: task!.name }));
      setTechnicalLogsBySlot((current) => ({ ...current, [slot]: [] }));
    }
    const entry: TechnicalLogEntry = { taskId: task.id, taskName: task.name, timestamp: new Date().toISOString(), category, message };
    setTechnicalLogsBySlot((current) => ({ ...current, [slot]: [...current[slot], entry].slice(-100) }));
    void invoke("append_technical_log", { entry }).catch(() => undefined);
  }

  function startTechnicalTask(name: string, slot: SourceMode = sourceModeRef.current) {
    const now = new Date();
    const timestampId = now.toISOString().replace(/[-:]/g, "").replace(/\.\d{3}Z$/, "Z");
    technicalTaskRefs.current[slot] = { id: `${timestampId}_${crypto.randomUUID().slice(0, 8)}`, name };
    lastTechnicalLogRefs.current[slot] = null;
    setTechnicalTaskNamesBySlot((current) => ({ ...current, [slot]: name }));
    setTechnicalLogsBySlot((current) => ({ ...current, [slot]: [] }));
    recordTechnicalLog("Task", "Started.", slot);
  }

  function setCompactPreference(value: boolean) {
    if (systemRecording || liveRecording || systemStopping || liveStopping) setCompactChangeDeferred(true);
    setCompactEnabled(value);
  }

  function updateNotesPending(delta: 1 | -1) {
    notesUpdatesPending.current = Math.max(0, notesUpdatesPending.current + delta);
    setNotesUpdating(notesUpdatesPending.current > 0);
  }

  function openSavedTranscript(item: SavedTranscript) {
    switchSourceMode("file");
    const isSummary = item.kind === "summary";
    const isNotes = item.kind === "notes";
    const isStudyNotes = item.kind === "study_notes";
    const isNotesTranslation = item.kind === "notes_translation";
    const requestedNotesStyle: "meeting" | "study" = isStudyNotes || (isNotesTranslation && item.id.split(":")[2] === "study") ? "study" : "meeting";
    const isSummaryTranslation = item.kind === "summary_translation";
    const isTranslation = item.kind === "translation";
    const isEnhanced = item.kind === "enhanced";
    const itemSessionId = item.id.split(":")[1] ?? item.id;
    const hasSessionId = /^[0-9a-f]{8}-[0-9a-f-]{27}$/i.test(itemSessionId);
    transcriptSessionId.current = hasSessionId ? itemSessionId : item.kind === "transcript" ? item.id : "";
    const sessionItems = transcriptSessionId.current
      ? savedTranscripts.filter((saved) => saved.id === transcriptSessionId.current || saved.id.startsWith(`enhanced:${transcriptSessionId.current}`) || saved.id.startsWith(`summary:${transcriptSessionId.current}`) || saved.id.startsWith(`notes:${transcriptSessionId.current}`) || saved.id.startsWith(`study-notes:${transcriptSessionId.current}`) || saved.id.startsWith(`translation:${transcriptSessionId.current}:`) || saved.id.startsWith(`summary-translation:${transcriptSessionId.current}:`) || saved.id.startsWith(`notes-translation:${transcriptSessionId.current}:`))
      : [item];
    const originalItem = sessionItems.find((saved) => saved.kind === "transcript");
    const enhancedItem = sessionItems.find((saved) => saved.kind === "enhanced");
    const summaryItem = sessionItems.find((saved) => saved.kind === "summary");
    const notesItem = sessionItems.find((saved) => saved.kind === (requestedNotesStyle === "study" ? "study_notes" : "notes")) ?? sessionItems.find((saved) => saved.kind === "notes" || saved.kind === "study_notes");
    const originalText = originalItem?.text ?? (item.kind === "transcript" ? item.text : "");
    const enhancedText = enhancedItem?.text ?? (isEnhanced ? item.text : "");
    const summaryTextForSession = summaryItem?.text ?? (isSummary ? item.text : isSummaryTranslation ? "" : "");
    const notesTextForSession = notesItem?.text ?? (isNotes || isStudyNotes ? item.text : "");
    const transcriptTranslationItems = sessionItems.filter((saved) => saved.kind === "translation");
    const savedTranscriptTranslations = Object.fromEntries(transcriptTranslationItems.map((saved) => [saved.name.split(" → ").at(-1) ?? "Translation", saved.text]));
    const savedSummaryTranslationItems = sessionItems.filter((saved) => saved.kind === "summary_translation");
    const savedSummaryTranslations = Object.fromEntries(savedSummaryTranslationItems.map((saved) => {
      const language = saved.name.split(" → ").at(-1) ?? "Translation";
      return [`${language} · ${selectedSummaryTranslationModel?.name ?? "gpt-6-luna"}`, saved.text];
    }));
    const savedNotesTranslations = Object.fromEntries(sessionItems.filter((saved) => saved.kind === "notes_translation" && saved.id.split(":")[2] === requestedNotesStyle).map((saved) => [saved.name.split(" → ").at(-1) ?? "Translation", saved.text]));
    const showingSummary = Boolean(summaryTextForSession) || isSummary || isSummaryTranslation;
    const selectedTranslationLanguage = isTranslation || isSummaryTranslation || isNotesTranslation ? item.name.split(" → ").at(-1) ?? "Original" : undefined;
    transcriptSessionName.current = (originalItem?.name ?? item.name).replace(/\.txt$/i, "");
    setTranscript(originalText || (isTranslation ? item.text : ""));
    setOriginalTranscript(originalText);
    setEnhancedTranscript(enhancedText);
    transcriptRef.current = originalText || (isTranslation ? item.text : "");
    setActiveLanguage(isEnhanced ? "Enhanced" : isTranslation ? selectedTranslationLanguage ?? "Original" : "Original");
    setCompleted(true);
    const savedTranslationLanguage = item.name.split(" → ").at(-1) ?? "Translation";
    setTranslations(isTranslation && !savedTranscriptTranslations[savedTranslationLanguage] ? { ...savedTranscriptTranslations, [savedTranslationLanguage]: item.text } : savedTranscriptTranslations);
    setTranslationEnabled(false);
    setSummaryMode(showingSummary);
    summaryModeRef.current = showingSummary;
    setSummaryText(summaryTextForSession);
    summaryTextRef.current = summaryTextForSession;
    setNotesText(notesTextForSession);
    notesTextRef.current = notesTextForSession;
    const loadedNotesStyle = notesItem?.kind === "study_notes" ? "study" : requestedNotesStyle;
    setNotesStyle(loadedNotesStyle);
    notesStyleRef.current = loadedNotesStyle;
    setNotesTranslations(isNotesTranslation && !Object.keys(savedNotesTranslations).length
      ? { [item.name.split(" → ").at(-1) ?? "Translation"]: item.text }
      : savedNotesTranslations);
    setNotesMode(false);
    notesModeRef.current = false;
    setSummaryStatus(showingSummary ? "ended" : "idle");
    setSummaryTranslations(isSummaryTranslation && !Object.keys(savedSummaryTranslations).length ? { [`${savedTranslationLanguage} · ${selectedSummaryTranslationModel?.name ?? "gpt-6-luna"}`]: item.text } : savedSummaryTranslations);
    if (selectedTranslationLanguage) setTargetLanguage(selectedTranslationLanguage);
    setSummaryDraft("");
    setPhase(`Opened saved ${showingSummary ? "summary" : isTranslation ? "translation" : "transcript"} · ${item.name}`);
    transcriptTabs.current[sourceMode] = {
      transcript: originalText || (isTranslation ? item.text : ""), originalTranscript: originalText, enhancedTranscript: enhancedText,
      summaryText: summaryTextForSession,
      translations: savedTranscriptTranslations,
      summaryTranslations: savedSummaryTranslations,
      questionMessages: [],
      activeLanguage: isEnhanced ? "Enhanced" : isTranslation ? selectedTranslationLanguage ?? "Original" : "Original", completed: true, summaryMode: showingSummary, notesText: notesTextForSession, notesMode: false, notesStyle: loadedNotesStyle, notesTranslations: savedNotesTranslations, summaryTranslationLive: false, translationEnabled: false,
      contextPrompt: "", transcriptionPrompts: {},
      sessionId: transcriptSessionId.current, sessionName: transcriptSessionName.current,
    };
    updateContextPrompt(sourceModeRef.current, "");
    clearPromptHistory(sourceModeRef.current);
  }

  async function importLocalFile(path: string, append = false) {
    if (transcriptionBusyBySlot.file) {
      setError("Local file is busy with a transcription. Wait for it to finish before importing another file.");
      return;
    }
    startTechnicalTask("Local media import");
    setLocalImporting(true);
    setProgress(4);
    setPhase("Checking local file");
    try {
      const imported = await invoke<LibraryFile>("import_local_media", { path });
      await refreshLibraryFiles();
      switchSourceMode("file");
      const ready = await inspectAudio(imported.path, append);
      const info = ready ? await invoke<AudioInfo>("inspect_audio", { path: imported.path }) : null;
      if (info?.isVideo && info.audioTracks[0]) {
        setPhase("Separating audio without changing the original");
        const extracted = await invoke<string>("extract_local_audio", { path: imported.path, audioStreamIndex: info.audioTracks[0].streamIndex });
        await refreshLibraryFiles();
        if (append) {
          audioQueueRef.current = audioQueueRef.current.slice(0, -1);
          setAudioQueue(audioQueueRef.current);
        }
        await inspectAudio(extracted, append);
        setPhase("Extracted audio is saved in Minefia · ready to transcribe");
      } else if (ready) setPhase("Using the original local file · ready to transcribe");
      recordTechnicalLog("Local media", "File preparation completed.", "file");
    } catch (cause) {
      setError(errorMessage(cause));
      recordTechnicalLog("Local media", `Preparation failed: ${errorMessage(cause)}`, "file");
    } finally {
      setLocalImporting(false);
      setProgress(0);
    }
  }

  async function deleteLibraryItem() {
    if (!pendingDelete) return;
    try {
      if (pendingDelete.kind === "media") await invoke("delete_library_file", { path: pendingDelete.id });
      else await invoke("delete_saved_transcript", { id: pendingDelete.id });
      setPendingDelete(null);
      await Promise.all([refreshLibraryFiles(), refreshSavedTranscripts()]);
      if (pendingDelete.kind === "media" && audio?.path === pendingDelete.id) reset();
    } catch (cause) { setError(errorMessage(cause)); }
  }

  useEffect(() => { summaryModeRef.current = summaryMode; }, [summaryMode]);
  useEffect(() => { sourceModeRef.current = sourceMode; }, [sourceMode]);
  useEffect(() => { completedRef.current = completed; }, [completed]);
  useEffect(() => { notesModeRef.current = notesMode; }, [notesMode]);
  useEffect(() => { summaryTranslationLiveRef.current = summaryTranslationLive; }, [summaryTranslationLive]);
  useEffect(() => { summaryModelRef.current = translationModel; }, [translationModel]);
  useEffect(() => { summaryTranslationModelRef.current = summaryTranslationModel; }, [summaryTranslationModel]);
  useEffect(() => { targetLanguageRef.current = targetLanguage; }, [targetLanguage]);
  useEffect(() => { customLanguageRef.current = customLanguage; }, [customLanguage]);

  async function translateRunningSummary(text: string) {
    const language = targetLanguageRef.current === "Other" ? customLanguageRef.current.trim() : targetLanguageRef.current;
    const modelId = summaryTranslationModelRef.current;
    if (!text.trim() || !language || !["gpt-6-luna", "gpt-6.1-sol"].includes(modelId)) return;
    const taskSlot = sourceModeRef.current;
    translationLogSlotRef.current = taskSlot;
    recordTechnicalLog("Summary translation", `Started · ${language}.`, taskSlot);
    const key = `${language} · ${modelId}`;
    setSummaryTranslating(true);
    setSummaryTranslations((current) => ({ ...current, [key]: "" }));
    setSummaryTranslationDraft("");
    translatingLanguage.current = `summary:${key}`;
    try {
      const translated = await invoke<string>("translate_transcript", { text, targetLanguage: language, model: modelId });
      setSummaryTranslations((current) => ({ ...current, [key]: translated }));
      await invoke("save_summary_translation", { sessionId: transcriptSessionId.current, sourceName: transcriptSessionName.current, language, text: translated });
      await refreshSavedTranscripts();
    } catch (cause) {
      setError(`Summary translation failed: ${errorMessage(cause)}`);
    } finally {
      translatingLanguage.current = "";
      setSummaryTranslating(false);
      setSummaryTranslationDraft("");
    }
  }

  function queueRunningNotes(text: string) {
    const addition = text.trim();
    if (!addition || !summaryModeRef.current || (sourceModeRef.current !== "system" && !completedRef.current)) return;
    summaryLogSlotRef.current = sourceModeRef.current;
    updateNotesPending(1);
    summaryQueue.current = summaryQueue.current.then(async () => {
      const notes = await invoke<string>("update_summary_notes", {
        previousNotes: summaryTextRef.current,
        newTranscript: addition,
        model: summaryModelRef.current,
        noteStyle: "running_summary",
      });
      summaryTextRef.current = notes;
      setSummaryText(notes);
      setSummaryDraft("");
      await invoke("save_transcript_summary", {
        sessionId: transcriptSessionId.current,
        name: transcriptSessionName.current,
        text: notes,
      });
      await refreshSavedTranscripts();
      const liveLanguage = targetLanguageRef.current === "Other" ? customLanguageRef.current.trim() : targetLanguageRef.current;
      const liveModel = summaryTranslationModelRef.current;
      if (summaryTranslationLiveRef.current && liveLanguage && ["gpt-6-luna", "gpt-6.1-sol"].includes(liveModel)) await translateRunningSummary(notes);
    }).catch((cause) => { setError(`Running notes failed: ${errorMessage(cause)}`); }).finally(() => updateNotesPending(-1));
  }

  function queueMeetingNotes(text: string) {
    const addition = text.trim();
    if (!addition || !notesModeRef.current || (sourceModeRef.current !== "system" && !completedRef.current)) return;
    summaryLogSlotRef.current = sourceModeRef.current;
    updateNotesPending(1);
    summaryQueue.current = summaryQueue.current.then(async () => {
      const selectedNotesStyle = notesStyleRef.current;
      const notes = await invoke<string>("update_summary_notes", { previousNotes: notesTextRef.current, newTranscript: addition, model: summaryModelRef.current, noteStyle: selectedNotesStyle });
      notesTextRef.current = notes;
      setNotesText(notes);
      await invoke("save_transcript_notes", { sessionId: transcriptSessionId.current, name: transcriptSessionName.current, text: notes, noteStyle: selectedNotesStyle });
      await refreshSavedTranscripts();
    }).catch((cause) => setError(`Notes update failed: ${errorMessage(cause)}`)).finally(() => updateNotesPending(-1));
  }

  async function generateFinalNotesForSlot(slot: SourceMode, text: string, style: "meeting" | "study") {
    const tab = transcriptTabs.current[slot];
    if (!text.trim() || !tab.sessionId) return;
    updateNotesPending(1);
    recordTechnicalLog("Notes", `Generating detailed ${style === "study" ? "study notes" : "meeting minutes"} from the completed transcript.`, slot);
    try {
      const notes = await invoke<string>("generate_final_notes", {
        text,
        model: summaryModelRef.current,
        noteStyle: style,
      });
      tab.notesText = notes;
      tab.notesStyle = style;
      if (sourceModeRef.current === slot) {
        notesTextRef.current = notes;
        setNotesText(notes);
      }
      await invoke("save_transcript_notes", {
        sessionId: tab.sessionId,
        name: tab.sessionName,
        text: notes,
        noteStyle: style,
      });
      await refreshSavedTranscripts();
      recordTechnicalLog("Notes", "Detailed notes completed; interim notes replaced.", slot);
    } catch (cause) {
      recordTechnicalLog("Notes", `Final notes failed; interim notes were kept: ${errorMessage(cause)}`, slot);
      if (sourceModeRef.current === slot) setError(`Final notes failed; interim notes were kept: ${errorMessage(cause)}`);
    } finally {
      updateNotesPending(-1);
    }
  }

  function finishSystemNotesAfterPendingUpdates() {
    const pending = summaryQueue.current;
    void pending.then(async () => {
      const tab = transcriptTabs.current.system;
      if (tab.notesMode) {
        await generateFinalNotesForSlot("system", systemCommittedTranscript.current || tab.transcript, tab.notesStyle);
      }
    }).finally(() => {
      transcriptTabs.current.system.notesMode = false;
      if (sourceModeRef.current === "system") {
        notesModeRef.current = false;
        setNotesMode(false);
      }
    });
  }

  function startNotes() {
    const slot = sourceModeRef.current;
    setNotesMode(true);
    notesModeRef.current = true;
    transcriptTabs.current[slot].notesMode = true;
    if (completedRef.current && transcriptRef.current.trim()) {
      void generateFinalNotesForSlot(slot, transcriptRef.current, notesStyleRef.current).finally(() => {
        transcriptTabs.current[slot].notesMode = false;
        if (sourceModeRef.current === slot) {
          notesModeRef.current = false;
          setNotesMode(false);
        }
      });
    } else if (transcriptRef.current.trim()) queueMeetingNotes(transcriptRef.current);
  }

  function chooseNotesStyle(style: "meeting" | "study") {
    if (notesModeRef.current) return;
    notesStyleRef.current = style;
    setNotesStyle(style);
    const prefix = style === "study" ? "study-notes:" : "notes:";
    const savedNotes = savedTranscripts.find((item) => item.id === `${prefix}${transcriptSessionId.current}`);
    notesTextRef.current = savedNotes?.text ?? "";
    setNotesText(notesTextRef.current);
    const translationPrefix = `notes-translation:${transcriptSessionId.current}:${style}:`;
    setNotesTranslations(Object.fromEntries(savedTranscripts
      .filter((item) => item.kind === "notes_translation" && item.id.startsWith(translationPrefix))
      .map((item) => [item.name.split(" → ").at(-1) ?? "Translation", item.text])));
  }

  async function translateNotes() {
    const language = targetLanguage === "Other" ? customLanguage.trim() : targetLanguage;
    if (!notesText.trim() || !language || !selectedTranscriptTranslationModel || notesTranslating) return;
    setNotesTranslating(true);
    const taskSlot = sourceModeRef.current;
    translationLogSlotRef.current = taskSlot;
    recordTechnicalLog("Notes translation", `Started · ${language}.`, taskSlot);
    recordTechnicalLog("Model", `Selected ${transcriptTranslationModel}.`, taskSlot);
    try {
      const translated = await invoke<string>("translate_transcript", {
        text: notesText,
        targetLanguage: language,
        model: transcriptTranslationModel,
      });
      setNotesTranslations((current) => ({ ...current, [language]: translated }));
      await invoke("save_notes_translation", {
        sessionId: transcriptSessionId.current,
        sourceName: transcriptSessionName.current,
        language,
        text: translated,
        noteStyle: notesStyleRef.current,
      });
      await refreshSavedTranscripts();
    } catch (cause) {
      setError(`Notes translation failed: ${errorMessage(cause)}`);
      recordTechnicalLog("Notes translation", `Failed: ${errorMessage(cause)}`, taskSlot);
    } finally {
      setNotesTranslating(false);
    }
  }

  async function generateFinalSummary(text: string) {
    if (!text.trim()) return;
    const taskSlot = sourceModeRef.current;
    summaryLogSlotRef.current = taskSlot;
    recordTechnicalLog("Summary", `Started · full transcript · ${summaryModelRef.current}.`, taskSlot);
    await summaryQueue.current;
    setSummaryGenerating(true);
    setSummaryStatus("started");
    setSummaryDraft("");
    try {
      const result = await invoke<string>("summarize_transcript", { text, model: summaryModelRef.current });
      summaryTextRef.current = result;
      setSummaryText(result);
      setSummaryDraft("");
      await invoke("save_transcript_summary", {
        sessionId: transcriptSessionId.current,
        name: transcriptSessionName.current,
        text: result,
      });
      await refreshSavedTranscripts();
      setSummaryStatus("ended");
      if (summaryTranslationLiveRef.current) await translateRunningSummary(result);
    } catch (cause) {
      setSummaryStatus("failed");
      setError(`Summary failed: ${errorMessage(cause)}`);
      recordTechnicalLog("Summary", `Failed: ${errorMessage(cause)}`, taskSlot);
    } finally {
      setSummaryGenerating(false);
    }
  }

  async function translateSummary(force = false) {
    if (!summaryText.trim() || !selectedSummaryTranslationModel || !requestedTargetLanguage) return;
    const key = `${requestedTargetLanguage} · ${selectedSummaryTranslationModel.name}`;
    if (!force && summaryTranslations[key]) return;
    const taskSlot = sourceModeRef.current;
    translationLogSlotRef.current = taskSlot;
    recordTechnicalLog("Summary translation", `Started · ${requestedTargetLanguage}.`, taskSlot);
    recordTechnicalLog("Model", `Selected ${summaryTranslationModel}.`, taskSlot);
    setSummaryTranslating(true);
    setSummaryTranslationDraft("");
    setSummaryTranslations((current) => ({ ...current, [key]: "" }));
    translatingLanguage.current = `summary:${key}`;
    try {
      const translated = await invoke<string>("translate_transcript", {
        text: summaryText,
        targetLanguage: requestedTargetLanguage,
        model: summaryTranslationModel,
      });
      setSummaryTranslations((current) => ({ ...current, [key]: translated }));
      await invoke("save_summary_translation", {
        sessionId: transcriptSessionId.current,
        sourceName: transcriptSessionName.current,
        language: requestedTargetLanguage,
        text: translated,
      });
      await refreshSavedTranscripts();
    } catch (cause) {
      setSummaryTranslations((current) => { const next = { ...current }; delete next[key]; return next; });
      setError(`Summary translation failed: ${errorMessage(cause)}`);
    } finally {
      translatingLanguage.current = "";
      setSummaryTranslating(false);
      setSummaryTranslationDraft("");
    }
  }

  async function askAboutTranscript() {
    const sourceText = transcriptRef.current.trim();
    const summarySource = summaryTextRef.current.trim();
    const prompt = question.trim();
    if ((!sourceText && !summarySource) || !prompt || !selectedQuestionModel || questionBusy) return;
    const conversation = questionMessages.map((message) => `${message.role === "user" ? "User" : "Assistant"}: ${message.text}`).join("\n\n");
    setQuestionMessages((current) => [...current, { role: "user", text: prompt }]);
    setQuestion("");
    setQuestionDraft("");
    setQuestionBusy(true);
    try {
      const answer = await invoke<string>("ask_about_transcript", {
        transcript: sourceText,
        summary: summarySource,
        question: prompt,
        conversation,
        model: questionModel,
      });
      setQuestionMessages((current) => [...current, { role: "assistant", text: answer }]);
      setQuestionDraft("");
    } catch (cause) {
      setQuestionDraft("");
      setError(`Question failed: ${errorMessage(cause)}`);
    } finally {
      setQuestionBusy(false);
    }
  }

  async function enhanceTranscript(text: string): Promise<string> {
    if (!text.trim()) return text;
    const sessionId = transcriptSessionId.current;
    const sessionName = transcriptSessionName.current;
    if (!sessionId) return text;
    const original = savedTranscripts.find((item) => item.kind === "transcript" && item.id === sessionId)?.text ?? text;
    setEnhancementRunning(true);
    startTechnicalTask("Transcript review");
    setPhase("Reviewing transcript for clear transcription errors");
    try {
      await transcriptPersistenceQueue.current;
      // Save the source transcript before creating a separate reviewed copy.
      await invoke("snapshot_saved_transcript", {
        sessionId,
        name: sessionName,
      });
      const enhanced = await invoke<string>("enhance_transcript", { text: original });
      await invoke("save_enhanced_transcript", {
        sessionId,
        name: sessionName,
        text: enhanced,
      });
      await invoke("cleanup_saved_transcript_chunks", { sessionId });
      if (transcriptSessionId.current === sessionId) {
        setOriginalTranscript(original);
        setEnhancedTranscript(enhanced);
        setActiveLanguage("Enhanced");
        transcriptRef.current = enhanced;
        setTranscript(enhanced);
        setPhase("Enhanced transcript saved · original retained");
      }
      await refreshSavedTranscripts();
      return enhanced;
    } catch (cause) {
      recordTechnicalLog("Transcript review", `Failed: ${errorMessage(cause)}`, sourceModeRef.current);
      if (transcriptSessionId.current === sessionId) {
        setError(`Transcript review failed; the original transcript was kept. ${errorMessage(cause)}`);
        setPhase("Transcript review failed · original retained");
      }
      return text;
    } finally {
      setEnhancementRunning(false);
    }
  }

  function renderOrderedTranscript(totalChunks?: number) {
    const ordered: string[] = [];
    const total = totalChunks ?? Object.keys(transcriptParts.current).length;
    for (let chunk = 1; chunk <= total; chunk += 1) {
      ordered.push(transcriptParts.current[chunk] ?? "");
      if (!completedParts.current.has(chunk)) break;
    }
    const text = ordered.join("\n");
    setTranscript(text);
    setOriginalTranscript(text);
    transcriptRef.current = text;
  }

  function selectTranscriptVersion(language: string) {
    setActiveLanguage(language);
    const selected = language === "Original"
      ? originalTranscript || transcript
      : language === "Enhanced"
        ? enhancedTranscript || transcript
        : translations[language];
    if (selected !== undefined) {
      setTranscript(selected);
      transcriptRef.current = selected;
    }
  }

  async function refreshApiKeys() {
    setModelsLoading(true);
    setModelLoadError("");
    // Never leave the previous key's models selectable while refreshing.
    setAvailableModelIds([]);
    setAvailableTranslationModelIds([]);
    setAvailableRealtimeModelIds([]);
    try {
      const keys = await invoke<ApiKeySummary[]>("list_api_keys");
      setApiKeys(keys);
      if (keys.some((key) => key.isSelected)) {
        try {
          const models = await invoke<AvailableModels>("list_available_models");
          setAvailableModelIds(models.transcription);
          setAvailableTranslationModelIds(models.translation);
          setAvailableRealtimeModelIds(models.realtime ?? []);
        } catch (cause) {
          setModelLoadError(errorMessage(cause));
        }
      } else {
        setAvailableModelIds([]);
        setAvailableRealtimeModelIds([]);
      }
    } catch (cause) {
      setAvailableModelIds([]);
      setAvailableTranslationModelIds([]);
      setAvailableRealtimeModelIds([]);
      setModelLoadError(errorMessage(cause));
    } finally {
      setModelsLoading(false);
    }
  }

  async function refreshResumableJobs() {
    try { setResumableJobs(await invoke<ResumableJob[]>("list_resumable_transcriptions")); }
    catch { setResumableJobs([]); }
  }

  useEffect(() => {
    void refreshApiKeys();
    void refreshAdminApiKeys();
    void refreshProjectUsage();
    void refreshResumableJobs();
    void refreshSavedTranscripts();
    void refreshLibraryFiles();

    const unlistenEvent = listen<TranscriptionEvent>("transcription-progress", ({ payload }) => {
      if (payload.taskSlot === "system") {
        if (payload.kind === "prompt") {
          rememberPrompt("system", payload.chunk ?? 0, payload.prompt ?? null);
        } else if (payload.kind === "delta" && payload.chunk && payload.delta) {
          systemStreamedParts.current[payload.chunk] = (systemStreamedParts.current[payload.chunk] ?? "") + payload.delta;
          renderSystemTranscriptPreview();
        }
        return;
      }
      if (payload.taskSlot !== "file" && payload.taskSlot !== "online") return;
      const task = transcriptionTasks.current[payload.taskSlot];
      if (!task || !task.active || task.id !== payload.taskId) return;
      const globalChunk = (payload.chunk ?? 1) + task.chunkOffset;
      const isVisibleTask = sourceModeRef.current === payload.taskSlot;
      if (payload.totalChunks) {
        task.totalChunks = payload.totalChunks;
        if (isVisibleTask) currentTranscriptionTotal.current = payload.totalChunks;
      }
      if (["preparing", "retrying", "chunk_done", "done", "error"].includes(payload.kind)) {
        const part = payload.chunk ? ` · part ${payload.chunk}${payload.totalChunks ? `/${payload.totalChunks}` : ""}` : "";
        recordTechnicalLog(`Transcription · ${payload.taskSlot}`, `${payload.kind}${part}${payload.code ? ` · [${payload.code}]` : ""}${payload.message ? ` · ${payload.message}` : ""}`, payload.taskSlot);
      }
      const renderTask = () => {
        const ordered: string[] = [];
        for (let chunk = 1; chunk <= task.chunkOffset + task.totalChunks; chunk += 1) {
          ordered.push(task.parts[chunk] ?? "");
          if (!task.complete.has(chunk)) break;
        }
        const text = ordered.join("\n");
        const tab = transcriptTabs.current[payload.taskSlot];
        tab.transcript = text;
        tab.originalTranscript = text;
        if (isVisibleTask) {
          transcriptParts.current = { ...task.parts };
          completedParts.current = new Set(task.complete);
          transcriptRef.current = text;
          setTranscript(text);
          setOriginalTranscript(text);
        }
        return text;
      };
      if (payload.kind === "prompt") {
        rememberPrompt(payload.taskSlot, payload.chunk ? globalChunk : 0, payload.prompt ?? null);
      } else if (payload.kind === "preparing") {
        task.phase = `File ${task.fileIndex}/${task.fileCount} · ${payload.message ?? "Preparing audio"}`;
        task.progress = 12;
        if (isVisibleTask) { setPhase(task.phase); setProgress(task.progress); }
      } else if (payload.kind === "uploading") {
        task.phase = `File ${task.fileIndex}/${task.fileCount} · part ${payload.chunk} of ${payload.totalChunks}`;
        task.progress = 20 + ((payload.chunk ?? 1) - 1) / (payload.totalChunks ?? 1) * 70;
        if (isVisibleTask) { setPhase(task.phase); setProgress(task.progress); }
      } else if (payload.kind === "retrying") {
        if (payload.chunk) {
          task.parts[globalChunk] = "";
          task.complete.delete(globalChunk);
          task.attempts[globalChunk] = (task.attempts[globalChunk] ?? 0) + 1;
          if (isVisibleTask) transcriptAttempts.current[globalChunk] = task.attempts[globalChunk];
          renderTask();
        }
        task.phase = payload.message ?? "A timed-out part is being retried";
        if (isVisibleTask) setPhase(task.phase);
      } else if (payload.kind === "delta" && payload.delta) {
        task.parts[globalChunk] = (task.parts[globalChunk] ?? "") + payload.delta;
        task.persistence = task.persistence.then(() => invoke("append_saved_transcript", { sessionId: task.sessionId, chunk: globalChunk, attempt: task.attempts[globalChunk] ?? 0, name: task.sessionName, text: payload.delta })).then(() => undefined).catch((cause) => { if (isVisibleTask) setError(`Transcript is visible but could not be saved: ${errorMessage(cause)}`); });
        renderTask();
      } else if (payload.kind === "chunk_done") {
        if (payload.chunk) {
          task.complete.add(globalChunk);
          if (payload.text !== undefined) {
            task.parts[globalChunk] = payload.text;
            task.persistence = task.persistence.then(() => invoke("replace_saved_transcript_chunk", { sessionId: task.sessionId, chunk: globalChunk, attempt: task.attempts[globalChunk] ?? 0, name: task.sessionName, text: payload.text })).then(() => invoke("snapshot_saved_transcript", { sessionId: task.sessionId, name: task.sessionName })).then(() => undefined).catch((cause) => { if (isVisibleTask) setError(`Transcript response could not be saved: ${errorMessage(cause)}`); });
          }
        }
        renderTask();
        if (isVisibleTask && payload.text?.trim()) { queueRunningNotes(payload.text); queueMeetingNotes(payload.text); }
        task.progress = 20 + task.complete.size / (task.totalChunks || 1) * 70;
        if (isVisibleTask) setProgress(task.progress);
      } else if (payload.kind === "done") {
        task.phase = "Transcription complete";
        task.progress = 100;
        if (isVisibleTask) { setPhase(task.phase); setProgress(task.progress); }
        task.persistence = task.persistence.then(() => invoke("snapshot_saved_transcript", { sessionId: task.sessionId, name: task.sessionName })).then(() => undefined).catch(() => undefined);
        void task.persistence.then(() => refreshSavedTranscripts());
      }
    });

    const unlistenOnlineDownload = listen<OnlineDownloadProgressEvent>("online-download-progress", ({ payload }) => {
      setOnlineDownloadProgress(payload);
      setPhase(payload.message);
      const previous = lastLoggedDownloadProgress.current;
      const percentMark = payload.totalBytes && payload.downloadedBytes !== undefined
        ? Math.floor((payload.downloadedBytes / payload.totalBytes) * 20)
        : -1;
      const partStep = payload.total > 0 ? Math.max(1, Math.ceil(payload.total / 10)) : 1;
      const partMark = payload.total > 0 ? Math.floor(payload.completed / partStep) : -1;
      const shouldLog = payload.stage !== previous.stage
        || payload.stage === "retrying"
        || (payload.downloadedBytes !== undefined && (percentMark >= 0
          ? percentMark !== previous.percentMark
          : payload.downloadedBytes - previous.bytes >= 2 * 1024 * 1024))
        || (payload.total > 0 && partMark !== previous.partMark);
      const byteDetails = payload.downloadedBytes !== undefined
        ? ` · ${formatBytes(payload.downloadedBytes)}${payload.totalBytes ? ` / ${formatBytes(payload.totalBytes)}` : " received"}`
        : payload.total > 0 ? ` · ${payload.completed}/${payload.total} parts` : "";
      if (shouldLog) {
        recordTechnicalLog("Download", `${payload.stage}: ${payload.message}${byteDetails}`, "online");
        lastLoggedDownloadProgress.current = {
          stage: payload.stage,
          bytes: payload.downloadedBytes ?? previous.bytes,
          partMark,
          percentMark,
        };
      }
      if (payload.stage === "retrying") return;
      if (payload.downloadedBytes !== undefined) {
        if (payload.totalBytes && payload.totalBytes > 0) {
          const ratio = Math.max(0, Math.min(1, payload.downloadedBytes / payload.totalBytes));
          setProgress(8 + ratio * 82);
        } else {
          setProgress(8);
        }
      } else if (payload.total > 0) {
        const ratio = Math.max(0, Math.min(1, payload.completed / payload.total));
        setProgress(8 + ratio * 82);
      } else if (payload.stage === "audio") {
        setProgress(94);
      } else {
        setProgress(4);
      }
    });

    const unlistenTranslation = listen<TranslationEvent>("translation-progress", ({ payload }) => {
      if (payload.kind === "done") { recordTechnicalLog("Translation", `Completed ${payload.totalChunks} part(s).`, translationLogSlotRef.current); void refreshUsage(); }
      if (payload.kind === "prompt_cache" && payload.cachedTokens) recordTechnicalLog("Prompt caching", `Used · ${payload.cachedTokens.toLocaleString()} cached input tokens (${payload.chunk}/${payload.totalChunks}).`, translationLogSlotRef.current);
      const language = translatingLanguage.current;
      if (payload.kind === "delta" && payload.delta && language) {
        if (language.startsWith("summary:")) {
          const key = language.slice("summary:".length);
          setSummaryTranslationDraft((current) => current + payload.delta);
          setSummaryTranslations((current) => ({ ...current, [key]: (current[key] ?? "") + payload.delta }));
        } else setTranslations((current) => ({ ...current, [language]: (current[language] ?? "") + payload.delta }));
      }
    });

    const unlistenSystemAudio = listen<SystemAudioEvent>("system-audio-progress", ({ payload }) => {
      if (["warning", "error", "done", "capture_stopped", "final_chunk_queued"].includes(payload.kind)) {
        const detail = payload.kind === "done" ? ` · ${payload.completedChunks} part(s) processed` : "";
        recordTechnicalLog("Audio capture", `${payload.kind}${payload.chunk ? ` · part ${payload.chunk}` : ""}${detail}${payload.code ? ` · [${payload.code}]` : ""}${payload.message ? ` · ${payload.message}` : ""}`, "system");
      }
      if (sourceModeRef.current !== "system") {
        if (payload.kind === "transcript" && payload.text !== undefined) {
          const tab = transcriptTabs.current.system;
          const chunk = payload.chunk ?? payload.completedChunks;
          commitSystemTranscriptChunk(chunk, payload.text, payload.completedChunks);
          const sessionId = tab.sessionId;
          const name = tab.sessionName;
          if (sessionId) void invoke("replace_final_transcript", { sessionId, name, text: systemCommittedTranscript.current }).then(refreshSavedTranscripts).catch((cause) => setError(`Background capture transcript save failed: ${errorMessage(cause)}`));
          return;
        }
        if (payload.kind === "done") {
          systemCaptureFinalized.current = true;
          const tab = transcriptTabs.current.system;
          tab.completed = true;
          tab.summaryMode = false;
          setSystemRecording(false);
          setSystemStopping(false);
          setCompactChangeDeferred(false);
          finishSystemNotesAfterPendingUpdates();
          return;
        }
        if (payload.kind !== "error" && payload.kind !== "warning") return;
      }
      if (payload.kind === "queue") {
        setSystemQueuedChunks(payload.queuedChunks);
        setSystemActiveRequests(payload.activeRequests);
      } else if (payload.kind === "capture_stopped") {
        // A late capture-stop notification must not undo the terminal queue event.
        if (systemCaptureFinalized.current) return;
        setSystemRecording(false);
        setSystemStopping(true);
        setSystemQueuedChunks(payload.queuedChunks);
        setSystemActiveRequests(payload.activeRequests);
        setPhase("Audio input stopped; finishing captured parts");
      } else if (payload.kind === "status") {
        setSystemElapsed(payload.elapsedSeconds);
        setSystemChunkElapsed(payload.chunkSeconds);
        setSystemForceRemaining(payload.forceSplitInSeconds);
        setSystemQueuedChunks(payload.queuedChunks);
        setSystemActiveRequests(payload.activeRequests);
        setSystemWaitingForSilence(payload.waitingForSilence);
        setSystemEstimatedCost(payload.estimatedCost);
        setPhase(payload.waitingForSilence ? "Waiting for silence after 90 seconds" : "Recording audio input");
        setProgress(Math.min(99, payload.chunkSeconds / 150 * 100));
      } else if (payload.kind === "warning") {
        setSystemWarning(payload.message ?? "A chunk was force-split without silence.");
      } else if (payload.kind === "transcript" && payload.text !== undefined) {
        recordTechnicalLog("Audio capture", `Part ${payload.completedChunks} transcribed; ${payload.queuedChunks} queued, ${payload.activeRequests} active.`, "system");
        commitSystemTranscriptChunk(payload.chunk ?? payload.completedChunks, payload.text, payload.completedChunks);
        transcriptRef.current = systemCommittedTranscript.current;
        persistTranscriptFragment(payload.chunk ?? systemCompletedChunksRef.current, payload.text);
        snapshotTranscriptFile();
        queueRunningNotes(payload.text);
        queueMeetingNotes(payload.text);
        setSystemQueuedChunks(payload.queuedChunks);
      } else if (payload.kind === "error") {
        setError(`${payload.code ? `[${payload.code}] ` : ""}${payload.message ?? "System audio capture failed."}`);
        void transcriptPersistenceQueue.current.then(refreshSavedTranscripts);
      } else if (payload.kind === "done") {
        systemCaptureFinalized.current = true;
        setSystemRecording(false);
        setSystemStopping(false);
        setCompactChangeDeferred(false);
        setSystemCompletedChunks(payload.completedChunks);
        setSystemQueuedChunks(0);
        setSystemActiveRequests(0);
        setCompleted(true);
        setProgress(100);
        setPhase("System audio transcription complete");
        snapshotTranscriptFile();
        cleanupSavedTranscriptChunks();
        if (summaryModeRef.current) void generateFinalSummary(transcriptRef.current);
        finishSystemNotesAfterPendingUpdates();
        void transcriptPersistenceQueue.current.then(refreshSavedTranscripts);
      }
    });

    const unlistenRealtime = listen<RealtimeAudioEvent>("realtime-audio-progress", ({ payload }) => {
      if (["reconnecting", "error", "done"].includes(payload.kind)) {
        recordTechnicalLog("Live audio", `${payload.kind}${payload.code ? ` · [${payload.code}]` : ""}${payload.message ? ` · ${payload.message}` : ""}`, "system");
      }
      if (sourceModeRef.current !== "system") {
        const tab = transcriptTabs.current.system;
        if (payload.kind === "transcript" && payload.transcript !== undefined && !payload.partial) {
          tab.transcript = payload.transcript;
          tab.originalTranscript = payload.transcript;
          if (tab.sessionId) void invoke("replace_final_transcript", { sessionId: tab.sessionId, name: tab.sessionName, text: payload.transcript }).then(refreshSavedTranscripts).catch((cause) => setError(`Background capture transcript save failed: ${errorMessage(cause)}`));
          return;
        }
        if (payload.kind === "done") {
          tab.completed = true;
          setLiveRecording(false);
          setLiveStopping(false);
          setCompactChangeDeferred(false);
          finishSystemNotesAfterPendingUpdates();
          return;
        }
        if (payload.kind !== "error") return;
      }
      setLiveElapsed(payload.elapsedSeconds);
      setLiveEstimatedCost(payload.estimatedCost);
      if (payload.kind === "transcript" && payload.transcript !== undefined) {
        transcriptRef.current = payload.transcript;
        setTranscript(payload.transcript);
        setOriginalTranscript(payload.transcript);
        if (!payload.partial) {
          if (payload.transcript.startsWith(liveFinalTranscript.current)) {
            queueRunningNotes(payload.transcript.slice(liveFinalTranscript.current.length));
            queueMeetingNotes(payload.transcript.slice(liveFinalTranscript.current.length));
          }
          liveFinalTranscript.current = payload.transcript;
        }
        if (payload.transcript.startsWith(livePersistedTranscript.current)) {
          persistTranscriptFragment(0, payload.transcript.slice(livePersistedTranscript.current.length), liveTranscriptAttempt.current);
        } else {
          liveTranscriptAttempt.current += 1;
          persistTranscriptFragment(0, payload.transcript, liveTranscriptAttempt.current);
        }
        livePersistedTranscript.current = payload.transcript;
        if (!payload.partial) snapshotTranscriptFile();
      }
      if (payload.kind === "reconnecting" || payload.kind === "status") setLiveMessage(payload.message ?? "Streaming audio to the transcription service");
      if (payload.kind === "error") {
        setLiveMessage(payload.message ?? "Live transcription failed.");
        setError(`${payload.code ? `[${payload.code}] ` : ""}${payload.message ?? "Live transcription failed."}`);
        void transcriptPersistenceQueue.current.then(refreshSavedTranscripts);
      }
      if (payload.kind === "done") {
        setLiveRecording(false);
        setLiveStopping(false);
        setCompactChangeDeferred(false);
        setCompleted(true);
        setProgress(100);
        setPhase("Live transcription complete");
        snapshotTranscriptFile();
        cleanupSavedTranscriptChunks();
        if (summaryModeRef.current) void generateFinalSummary(transcriptRef.current);
        finishSystemNotesAfterPendingUpdates();
        void transcriptPersistenceQueue.current.then(refreshSavedTranscripts);
      }
    });

    const unlistenSummary = listen<SummaryEvent>("summary-progress", ({ payload }) => {
      if (payload.kind === "start" || payload.kind === "done") recordTechnicalLog("Summary", `${payload.mode} ${payload.kind}.`, summaryLogSlotRef.current);
      if (payload.kind === "prompt_cache" && payload.cachedTokens) recordTechnicalLog("Prompt caching", `Used · ${payload.cachedTokens.toLocaleString()} cached input tokens (${payload.mode}).`, summaryLogSlotRef.current);
      if (payload.kind === "done") void refreshUsage();
      if (payload.mode === "question" && payload.kind === "delta" && payload.delta) {
        setQuestionDraft((current) => current + payload.delta);
      } else if (payload.mode === "summary" && payload.kind === "delta" && payload.delta) {
        setSummaryDraft((value) => value + payload.delta);
      }
    });
    const unlistenInputLevel = listen<number>("audio-input-level", ({ payload }) => setInputLevel(Math.max(0, Math.min(1, payload))));
    const unlistenCaptureFormat = listen<{ source: string; sampleRate: number; channels: number; targetSampleRate: number; targetChannels: number; captureEncoding: string }>("audio-capture-format", ({ payload }) => {
      recordTechnicalLog("Audio capture", `Microphone input: ${payload.sampleRate / 1000} kHz / ${payload.channels} ch → ${payload.targetSampleRate / 1000} kHz mono ${payload.captureEncoding}.`, "system");
    });
    const unlistenSavedCaptures = listen<string[]>("audio-recordings-saved", ({ payload }) => {
      const newestRecording = payload.at(-1);
      const recordingName = newestRecording?.split(/[\\/]/).pop();
      if (recordingName) transcriptSessionName.current = recordingName;
      void refreshLibraryFiles();
    });

    const unlistenDrop = getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === "over") setDragging(true);
      if (event.payload.type === "leave") setDragging(false);
      if (event.payload.type === "drop") {
        setDragging(false);
        const paths = event.payload.paths;
        void (async () => {
          for (const [index, path] of paths.entries()) await importLocalFile(path, index > 0);
          if (paths.length > 1 && audioQueueRef.current[0]) {
            setAudio(audioQueueRef.current[0]);
            setSelectedAudioStreamIndex(audioQueueRef.current[0].audioTracks[0]?.streamIndex ?? null);
          }
        })();
      }
    });

    return () => {
      void unlistenEvent.then((fn) => fn());
      void unlistenOnlineDownload.then((fn) => fn());
      void unlistenTranslation.then((fn) => fn());
      void unlistenSystemAudio.then((fn) => fn());
      void unlistenRealtime.then((fn) => fn());
      void unlistenSummary.then((fn) => fn());
      void unlistenInputLevel.then((fn) => fn());
      void unlistenCaptureFormat.then((fn) => fn());
      void unlistenSavedCaptures.then((fn) => fn());
      void unlistenDrop.then((fn) => fn());
    };
  }, []);

  useEffect(() => {
    if (sourceMode !== "system" || captureSource === "system") return;
    void refreshInputDevices();
  }, [sourceMode, captureSource]);

  useEffect(() => {
    if (!microphoneRateConstraint || availableCaptureQualities.length === 0 || availableCaptureQualities.includes(captureQuality)) return;
    const currentRate = CAPTURE_QUALITY_PRESETS.find((preset) => preset.key === captureQuality)?.sampleRate ?? 0;
    const fallback = [...availableCaptureQualities].reverse().find((key) => {
      const rate = CAPTURE_QUALITY_PRESETS.find((preset) => preset.key === key)?.sampleRate ?? 0;
      return rate <= currentRate;
    }) ?? availableCaptureQualities[0];
    setCaptureQuality(fallback);
  }, [microphoneRateConstraint, availableCaptureQualities.join(","), captureQuality]);

  useEffect(() => {
    localStorage.setItem("transcription-model", model);
  }, [model]);

  useEffect(() => {
    localStorage.setItem("system-transcription-model", systemModel);
  }, [systemModel]);

  useEffect(() => {
    localStorage.setItem("translation-model", translationModel);
  }, [translationModel]);
  useEffect(() => { localStorage.setItem("transcript-translation-model", transcriptTranslationModel); }, [transcriptTranslationModel]);
  useEffect(() => { localStorage.setItem("summary-translation-model", summaryTranslationModel); }, [summaryTranslationModel]);
  useEffect(() => { localStorage.setItem("question-model", questionModel); }, [questionModel]);

  useEffect(() => { localStorage.setItem("parallel-requests", String(parallelRequests)); }, [parallelRequests]);
  useEffect(() => { localStorage.setItem("capture-quality", captureQuality); }, [captureQuality]);
  useEffect(() => { localStorage.setItem("compact-enabled", String(compactEnabled)); }, [compactEnabled]);
  useEffect(() => {
    // Remove the legacy per-source settings so only the shared preference remains.
    localStorage.removeItem("compact-microphone");
    localStorage.removeItem("compact-online-download");
  }, []);
  useEffect(() => { localStorage.removeItem("recording-quality"); }, []);
  useEffect(() => { localStorage.setItem("technical-errors", String(technicalErrors)); }, [technicalErrors]);
  useEffect(() => {
    localStorage.setItem("technical-details", String(technicalDetails));
  }, [technicalDetails]);
  useEffect(() => { localStorage.setItem("headless-browser", String(headlessBrowser)); }, [headlessBrowser]);

  const availableModels = useMemo(
    () => TRANSCRIPTION_MODELS.filter((item) => availableModelIds.includes(item.id)),
    [availableModelIds],
  );
  const availableTranslationModels = useMemo(
    () => TRANSLATION_MODELS.filter((item) => availableTranslationModelIds.includes(item.id)),
    [availableTranslationModelIds],
  );

  useEffect(() => {
    if (availableModels.length > 0 && !availableModels.some((item) => item.id === model)) {
      setModel(availableModels[0].id);
    }
  }, [availableModels, model]);

  useEffect(() => {
    if (availableModels.length > 0 && !availableModels.some((item) => item.id === systemModel)) {
      setSystemModel(availableModels[0].id);
    }
  }, [availableModels, systemModel]);

  useEffect(() => {
    if (availableTranslationModels.length > 0 && !availableTranslationModels.some((item) => item.id === translationModel)) {
      setTranslationModel(availableTranslationModels[0].id);
    }
  }, [availableTranslationModels, translationModel]);
  useEffect(() => {
    if (availableTranslationModels.length === 0) return;
    if (!availableTranslationModels.some((item) => item.id === transcriptTranslationModel)) setTranscriptTranslationModel(availableTranslationModels[0].id);
    if (!availableTranslationModels.some((item) => item.id === summaryTranslationModel)) setSummaryTranslationModel(availableTranslationModels[0].id);
    if (!availableTranslationModels.some((item) => item.id === questionModel)) setQuestionModel(availableTranslationModels[0].id);
  }, [availableTranslationModels, transcriptTranslationModel, summaryTranslationModel, questionModel]);

  const selectedKey = apiKeys.find((key) => key.isSelected);
  const workspaceReady = !modelsLoading && Boolean(selectedKey) && !modelLoadError && (availableModels.length > 0 || availableRealtimeModelIds.length > 0);
  const accessState = modelsLoading ? "loading" : modelLoadError ? "error" : apiKeys.length === 0 ? "missing" : "error";
  const accessError = modelLoadError || (!selectedKey
    ? "No saved API key is selected."
    : "The Models API returned no supported transcription models. Add a key with access to a supported model.");
  const selectedModel = availableModels.find((item) => item.id === model);
  const selectedSystemModel = availableModels.find((item) => item.id === systemModel);
  const liveModelAvailable = availableRealtimeModelIds.includes("gpt-live-transcribe");
  const selectedAudioTrack = audio?.audioTracks.find((track) => track.streamIndex === selectedAudioStreamIndex);
  const queuedDuration = audioOwner === "file" && audioQueue.length > 1 ? audioQueue.reduce((sum, item) => sum + item.durationSeconds, 0) : audio?.durationSeconds ?? 0;
  const queuedSize = audioOwner === "file" && audioQueue.length > 1 ? audioQueue.reduce((sum, item) => sum + item.sizeBytes, 0) : audio?.sizeBytes ?? 0;
  const estimatedCost = audio && audioOwner === sourceMode ? estimateTranscriptionCost(model, queuedDuration) : 0;
  const estimatedParts = audio && audioOwner === sourceMode ? estimatePartCount(queuedSize, queuedDuration, false) : 1;
  const displayedText = activeLanguage === "Original"
    ? originalTranscript || transcript
    : activeLanguage === "Enhanced"
      ? enhancedTranscript || transcript
      : translations[activeLanguage] ?? transcript;
  const displayedSections = useMemo(() => splitReadableSections(displayedText), [displayedText]);
  const displayedTextStreaming = busy || enhancementRunning || systemRecording || liveRecording || (translating && activeLanguage === translatingLanguage.current);
  const selectedTranslationModel = availableTranslationModels.find((item) => item.id === translationModel);
  const selectedTranscriptTranslationModel = availableTranslationModels.find((item) => item.id === transcriptTranslationModel);
  const selectedSummaryTranslationModel = availableTranslationModels.find((item) => item.id === summaryTranslationModel);
  const selectedQuestionModel = availableTranslationModels.find((item) => item.id === questionModel);
  const onlineFormatSourceLabel = onlineMedia?.availableFormats.find((format) => format.formatId === onlineMedia.formatId)?.sourceLabel ?? onlineMedia?.sourceLabel ?? "";
  // Keep the backend discovery order so the first HLS playlist shown matches
  // the first playlist observed on the page.
  const onlineSourceLabels = onlineMedia ? [...new Set(onlineMedia.availableFormats.map((format) => format.sourceLabel).filter((label): label is string => Boolean(label)))] : [];
  const onlineHlsSourceLabels = onlineSourceLabels.filter((label) => label.startsWith("HLS playlist "));
  const visibleOnlineSourceLabels = onlineHlsSourceLabels.length > 0 && !onlineShowAllSources ? [onlineHlsSourceLabels[0]] : onlineSourceLabels;
  const hiddenOnlineSourceCount = onlineSourceLabels.length - visibleOnlineSourceLabels.length;
  const selectedOnlineSourceFormats = onlineMedia?.availableFormats.filter((format) => format.sourceLabel === onlineFormatSourceLabel) ?? [];
  const summaryModelLocked = summaryGenerating || notesUpdating || ((systemRecording || liveRecording || systemStopping || liveStopping)
    && (summaryMode || notesMode || transcriptTabs.current.system.summaryMode || transcriptTabs.current.system.notesMode));
  const translationEstimate = estimateTranslationCost(transcriptTranslationModel, transcript);
  const summaryEstimate = estimateTranslationCost(translationModel, summaryText || transcript);
  const summaryTranslationEstimate = estimateTranslationCost(summaryTranslationModel, summaryText);
  const requestedTargetLanguage = targetLanguage === "Other" ? customLanguage.trim() : targetLanguage;
  const targetTranslationKey = selectedTranscriptTranslationModel && requestedTargetLanguage
    ? `${requestedTargetLanguage} · ${selectedTranscriptTranslationModel.name}`
    : requestedTargetLanguage;
  const transcriptVersions: TranscriptVersion[] = [
    ...((originalTranscript || transcript) ? [{ language: "Original", text: originalTranscript || transcript }] : []),
    ...(enhancedTranscript ? [{ language: "Enhanced", text: enhancedTranscript }] : []),
    ...Object.entries(translations).map(([language, text]) => ({ language, text })),
  ];
  const progressStyle = useMemo(() => ({ "--progress": `${progress}%` } as React.CSSProperties), [progress]);
  const progressFinished = completed
    || phase === "Media inspected · audio stream confirmed"
    || phase === "Page inspected · choose a loaded audio or video source";
  const backgroundWorkActive = transcriptionBusyBySlot.file || transcriptionBusyBySlot.online || onlineExtracting || onlineInspecting || webPageInspecting || localImporting || systemRecording || liveRecording || systemStopping || liveStopping || translating || summaryTranslating || questionBusy || summaryGenerating || enhancementRunning;
  const onlineDownloadLabel = (() => {
    if (!onlineExtracting || !onlineDownloadProgress) return null;
    if (onlineDownloadProgress.downloadedBytes !== undefined) {
      const downloaded = formatBytes(onlineDownloadProgress.downloadedBytes);
      if (onlineDownloadProgress.totalBytes && onlineDownloadProgress.totalBytes > 0) {
        const total = onlineDownloadProgress.totalBytes;
        const percent = Math.round(Math.min(1, onlineDownloadProgress.downloadedBytes / total) * 100);
        return `${percent}% · ${downloaded} / ${formatBytes(total)}`;
      }
      return `${downloaded} received`;
    }
    if (onlineDownloadProgress.total > 0) {
      return `${onlineDownloadProgress.completed} / ${onlineDownloadProgress.total} parts`;
    }
    return null;
  })();
  const onlineDownloadIndeterminate = onlineExtracting
    && onlineDownloadProgress?.stage === "downloading"
    && onlineDownloadProgress.downloadedBytes !== undefined
    && !(onlineDownloadProgress.totalBytes && onlineDownloadProgress.totalBytes > 0);

  async function inspectAudio(path: string, append = false): Promise<boolean> {
    setError("");
    systemCaptureFinalized.current = captureMode !== "standard";
    if (!append) {
      setTranscript("");
      setOriginalTranscript("");
      setEnhancedTranscript("");
      updateContextPrompt("file", "");
      transcriptRef.current = "";
      liveFinalTranscript.current = "";
      livePersistedTranscript.current = "";
      setSummaryText("");
      summaryTextRef.current = "";
      setNotesText(""); notesTextRef.current = ""; setNotesMode(false); notesModeRef.current = false; notesStyleRef.current = "meeting"; setNotesStyle("meeting"); setNotesTranslations({}); setSummaryTranslationLive(false); summaryTranslationLiveRef.current = false;
      setSummaryDraft("");
      setSummaryStatus("idle");
      setSummaryTranslations({});
      setQuestionMessages([]);
      setQuestionDraft("");
      transcriptParts.current = {};
      completedParts.current.clear();
      setTranslations({});
      setTranslationEnabled(false);
      setActiveLanguage("Original");
      setCompleted(false);
      setProgress(0);
      audioQueueRef.current = [];
      setAudioQueue([]);
    }
    try {
      await invoke("allow_media_preview", { path });
      const info = await invoke<AudioInfo>("inspect_audio", { path });
      if (!append) setAudio(info);
      audioQueueRef.current = append ? [...audioQueueRef.current, info] : [info];
      setAudioQueue(audioQueueRef.current);
      setAudioOwner("file");
      if (!append) setSelectedAudioStreamIndex(info.audioTracks[0]?.streamIndex ?? null);
      setPhase("Ready to transcribe");
      return true;
    } catch (cause) {
      if (!append) setAudio(null);
      setSelectedAudioStreamIndex(null);
      setError(errorMessage(cause));
      return false;
    }
  }

  async function chooseAudio(appendToQueue = false) {
    if (transcriptionBusyBySlot.file) return;
    const selected = await open({
      multiple: true,
      directory: false,
    });
    const paths = Array.isArray(selected) ? selected : selected ? [selected] : [];
    const appendExisting = appendToQueue && audioQueueRef.current.length > 0;
    for (const [index, path] of paths.entries()) await importLocalFile(path, appendExisting || index > 0);
    if (paths.length > 1 && audioQueueRef.current[0]) {
      setAudio(audioQueueRef.current[0]);
      setSelectedAudioStreamIndex(audioQueueRef.current[0].audioTracks[0]?.streamIndex ?? null);
    }
  }

  function moveQueuedAudio(index: number, direction: -1 | 1) {
    const target = index + direction;
    if (target < 0 || target >= audioQueueRef.current.length) return;
    const next = [...audioQueueRef.current];
    [next[index], next[target]] = [next[target], next[index]];
    audioQueueRef.current = next;
    setAudioQueue(next);
    setAudio(next[0]);
    setSelectedAudioStreamIndex(next[0].audioTracks[0]?.streamIndex ?? null);
  }

  function removeQueuedAudio(index: number) {
    const next = audioQueueRef.current.filter((_, itemIndex) => itemIndex !== index);
    audioQueueRef.current = next;
    setAudioQueue(next);
    setAudio(next[0] ?? null);
    setSelectedAudioStreamIndex(next[0]?.audioTracks[0]?.streamIndex ?? null);
  }

  function onlineErrorMessage(cause: unknown) {
    const message = errorMessage(cause);
    if (technicalErrors) return message;
    if (message.includes("NO_AUDIO_STREAM")) return "No audio stream was found. Video-only sources are not downloaded.";
    if (message.includes("PROTECTED") || message.toLowerCase().includes("protected")) return "This media appears to be protected. Minefia Audio does not bypass DRM or access controls.";
    if (message.includes("REENCODE_APPROVAL_REQUIRED")) return "This audio format needs conversion. Review and approve conversion before continuing.";
    if (message.toLowerCase().includes("forbidden") || message.toLowerCase().includes("unauthorized")) return "This URL is not accessible. Check its permissions or use a directly accessible media URL.";
    if (message.toLowerCase().includes("canceled")) return message.toLowerCase().includes("inspect") ? "Online media inspection was canceled." : "Online media extraction was canceled.";
    return message;
  }

  function classifyOnlineUrl(value: string): "url" | "hls" | "web" | "unknown" {
    try {
      const parsed = new URL(value);
      const host = parsed.hostname.toLowerCase();
      if (parsed.pathname.toLowerCase().endsWith(".m3u8")) return "hls";
      if (host === "youtu.be" || host.endsWith(".youtu.be") || host === "youtube.com" || host.endsWith(".youtube.com")) return "url";
      if (/\.(mp3|m4a|aac|wav|flac|ogg|opus|mp4|mov|webm|mkv|ts|m4s)$/i.test(parsed.pathname)) return "url";
      return "unknown";
    } catch {
      return "unknown";
    }
  }

  async function inspectOnline(urlOverride?: string) {
    const url = (urlOverride ?? onlineUrl).trim();
    if (!url || onlineInspecting || webPageInspecting || onlineExtracting || transcriptionBusyBySlot.online) return;
    if (urlOverride) setOnlineUrl(url);
    setOnlineCancelRequested(false);
    setOnlineShowAllSources(false);
    startTechnicalTask(classifyOnlineUrl(url) === "web" ? "Web page inspection" : "Online media inspection");
    setError("");
    setProgress(0);
    clearPromptHistory("online");
    if (audioOwner === "online") setAudio(null);
    setTranscript("");
    setOriginalTranscript("");
    setEnhancedTranscript("");
    transcriptRef.current = "";
    transcriptParts.current = {};
    completedParts.current.clear();
    setSummaryText("");
    summaryTextRef.current = "";
    setSummaryDraft("");
    setNotesText("");
    notesTextRef.current = "";
    setNotesMode(false);
    notesModeRef.current = false;
    setSummaryMode(false);
    summaryModeRef.current = false;
    setSummaryStatus("idle");
    setTranslations({});
    setTranslationEnabled(false);
    setActiveLanguage("Original");
    setSummaryTranslations({});
    setNotesTranslations({});
    setQuestionMessages([]);
    setQuestionDraft("");
    setCompleted(false);
    transcriptSessionId.current = "";
    transcriptSessionName.current = "Transcript";
    updateContextPrompt("online", "");
    transcriptTabs.current.online = {
      ...transcriptTabs.current.online,
      transcript: "", originalTranscript: "", enhancedTranscript: "", summaryText: "", notesText: "",
      translations: {}, summaryTranslations: {}, notesTranslations: {}, questionMessages: [], completed: false,
      notesMode: false, summaryMode: false, translationEnabled: false, activeLanguage: "Original", sessionId: "", sessionName: "Transcript",
    };
    setOnlineMedia(null);
    setSelectedOnlineCaptionId("");
    setWebPageInfo(null);
    setOnlineInspecting(true);
    const kind = classifyOnlineUrl(url);
    setOnlineInputKind(kind === "unknown" ? "url" : kind);
    setPhase(kind === "web" ? "Opening the page and checking its loaded media" : "Inspecting media formats without downloading the source");
    try {
      if (kind === "web") {
        setOnlineInspecting(false);
        setWebPageInspecting(true);
        const info = await invoke<PageMediaInfo>("inspect_web_page", { url, headlessMode: headlessBrowser });
        setWebPageInfo(info);
        updateContextPrompt("online", info.pageTitle.trim() ? `Title: ${info.pageTitle.trim()}` : "");
        setOnlineInputKind("web");
        setProgress(100);
        setPhase("Page inspected · choose a loaded audio or video source");
      } else {
        try {
          const media = await invoke<OnlineMediaInfo>("inspect_online_media", { url, headlessMode: headlessBrowser });
          setOnlineMedia(media);
          setSelectedOnlineCaptionId(media.captions[0]?.id ?? "");
          updateContextPrompt("online", [
            media.title.trim() ? `Title: ${media.title.trim()}` : "",
            media.description?.trim() ? `Description: ${media.description.trim().slice(0, 700)}` : "",
          ].filter(Boolean).join("\n"));
          setOnlineKeepVideo(false);
          setProgress(100);
          setPhase("Media inspected · audio stream confirmed");
        } catch (mediaCause) {
          // Unknown URLs may be watch pages or signed direct media URLs; try the private browser only as a fallback.
          if (kind !== "unknown" || errorMessage(mediaCause).toLowerCase().includes("canceled")) throw mediaCause;
          setOnlineInspecting(false);
          setWebPageInspecting(true);
          setPhase("Checking the page for media that is currently loaded");
          const info = await invoke<PageMediaInfo>("inspect_web_page", { url, headlessMode: headlessBrowser });
          setWebPageInfo(info);
          updateContextPrompt("online", info.pageTitle.trim() ? `Title: ${info.pageTitle.trim()}` : "");
          setOnlineInputKind("web");
          setProgress(100);
          setPhase("Page inspected · choose a loaded audio or video source");
        }
      }
    } catch (cause) {
      const message = errorMessage(cause);
      recordTechnicalLog("Online media", `Inspection failed: ${message}`, "online");
      if (message.toLowerCase().includes("canceled")) {
        setError("Online media inspection was canceled.");
        setPhase("Inspection canceled");
        return;
      }
      setError(message.includes("MASTER_PLAYLIST_NOT_FOUND")
        ? "A blob video was found, but no master.m3u8 playlist appeared in the observed network traffic."
        : message.includes("WEB_MEDIA_NOT_FOUND")
          ? "No loaded direct video source or HLS master playlist was found on this page."
          : onlineErrorMessage(cause));
      setPhase("Online media could not be inspected");
    } finally {
      setOnlineInspecting(false);
      setWebPageInspecting(false);
      setOnlineCancelRequested(false);
    }
  }

  function selectOnlineFormat(formatId: string) {
    setOnlineMedia((current) => {
      if (!current) return current;
      const choice = current.availableFormats.find((format) => format.formatId === formatId);
      return choice ? { ...current, ...choice } : current;
    });
  }

  function selectOnlineSource(sourceLabel: string) {
    setOnlineMedia((current) => {
      if (!current) return current;
      const candidates = current.availableFormats.filter((format) => format.sourceLabel === sourceLabel);
      const choice = candidates.find((format) => format.recommended) ?? candidates[0];
      return choice ? { ...current, ...choice } : current;
    });
  }

  function inspectPageSource(url: string) {
    switchSourceMode("online");
    void inspectOnline(url);
  }

  async function useSelectedOnlineCaption() {
    const media = onlineMedia;
    const caption = media?.captions.find((item) => item.id === selectedOnlineCaptionId);
    if (!media || !caption || onlineCaptionLoading || onlineExtracting || transcriptionBusyBySlot.online) return;
    setError("");
    setOnlineCaptionLoading(true);
    setProgress(8);
    setPhase(`Loading ${caption.label}`);
    startTechnicalTask("Online caption transcript");
    recordTechnicalLog("Caption", `Using ${caption.label} as the transcript source.`, "online");
    try {
      const text = await invoke<string>("download_online_caption", {
        url: media.url,
        language: caption.language,
        automatic: caption.automatic,
      });
      if (!text.trim()) throw new Error("The selected caption contained no transcript text.");
      const name = media.filenameTitle ?? media.title;
      const sessionId = startTranscriptSession(name);
      setTranscript(text);
      setOriginalTranscript(text);
      setEnhancedTranscript("");
      transcriptRef.current = text;
      setSummaryText("");
      summaryTextRef.current = "";
      setNotesText("");
      notesTextRef.current = "";
      setCompleted(true);
      setActiveLanguage("Original");
      setTranslations({});
      setTranslationEnabled(false);
      transcriptTabs.current.online = {
        ...transcriptTabs.current.online,
        transcript: text,
        originalTranscript: text,
        enhancedTranscript: "",
        summaryText: "",
        notesText: "",
        translations: {},
        summaryTranslations: {},
        notesTranslations: {},
        questionMessages: [],
        activeLanguage: "Original",
        completed: true,
        summaryMode: false,
        translationEnabled: false,
        sessionId,
        sessionName: name,
      };
      await invoke("replace_final_transcript", { sessionId, name, text });
      await refreshSavedTranscripts();
      setProgress(100);
      setPhase(`Caption transcript ready · ${caption.label}`);
      recordTechnicalLog("Caption", `Transcript saved to Library · ${text.length.toLocaleString()} characters.`, "online");
    } catch (cause) {
      const message = errorMessage(cause);
      setError(`Could not use the selected caption: ${message}`);
      setProgress(0);
      setPhase("Caption transcript could not be created");
      recordTechnicalLog("Caption", `Failed: ${message}`, "online");
    } finally {
      setOnlineCaptionLoading(false);
    }
  }

  async function extractOnlineAudio(_allowReencode = false, downloadApproved = false) {
    if (!onlineMedia || onlineExtracting || transcriptionBusyBySlot.online) return;
    if (!downloadApproved) {
      setOnlineConversionApproved(false);
      setLargeDownloadOpen(true);
      return;
    }
    setError("");
    startTechnicalTask("Online media download");
    setOnlineCancelRequested(false);
    setOnlineDownloadProgress(null);
    lastLoggedDownloadProgress.current = { stage: "", bytes: 0, partMark: -1, percentMark: -1 };
    recordTechnicalLog("Download", `Source: ${onlineMedia.title}`, "online");
    recordTechnicalLog("Download", "Preserving the selected source audio codec without re-encoding.", "online");
    setOnlineExtracting(true);
    setProgress(2);
    setPhase("Preparing the download");
    try {
      const extracted = await invoke<AudioInfo>("extract_online_media", {
        request: { media: onlineMedia, keepVideo: onlineKeepVideo, headlessMode: headlessBrowser },
      });
      await refreshLibraryFiles();
      setAudio(extracted);
      setAudioOwner("online");
      updateContextPrompt("online", extracted.contextPrompt ?? "");
      setSelectedAudioStreamIndex(extracted.audioTracks[0]?.streamIndex ?? null);
      setTranscript("");
      setOriginalTranscript("");
      setEnhancedTranscript("");
      transcriptRef.current = "";
      setSummaryText("");
      summaryTextRef.current = "";
      setNotesText(""); notesTextRef.current = ""; setNotesMode(false); notesModeRef.current = false; notesStyleRef.current = "meeting"; setNotesStyle("meeting"); setNotesTranslations({}); setSummaryTranslationLive(false); summaryTranslationLiveRef.current = false;
      setSummaryDraft("");
      transcriptParts.current = {};
      completedParts.current.clear();
      setTranslations({});
      setTranslationEnabled(false);
      setActiveLanguage("Original");
      setCompleted(false);
      setProgress(0);
      setPhase("Audio saved in app storage · ready to transcribe");
      setOnlineMedia(null);
      setSelectedOnlineCaptionId("");
    } catch (cause) {
      const message = onlineErrorMessage(cause);
      if (message.includes("canceled")) {
        // A canceled extraction returns to the inspected-media state; don't leave a stale percent behind.
        setProgress(0);
        setOnlineDownloadProgress(null);
        recordTechnicalLog("Download", "Canceled by user; resumable download data kept.", "online");
        setPhase("Media inspected · audio stream confirmed");
      } else {
        setError(message);
        recordTechnicalLog("Download", `Failed: ${message}`, "online");
        setPhase("Audio extraction failed");
      }
    } finally {
      setOnlineExtracting(false);
      setOnlineCancelRequested(false);
    }
  }

  async function cancelOnlineExtraction() {
    if (!(onlineExtracting || onlineInspecting || webPageInspecting) || onlineCancelRequested) return;
    setOnlineCancelRequested(true);
    setPhase(onlineExtracting ? "Canceling and cleaning up temporary media" : "Canceling media inspection");
    try { await invoke("cancel_online_media"); }
    catch (cause) {
      const message = onlineErrorMessage(cause);
      setError(message);
      recordTechnicalLog("Download", `Cancel failed: ${message}`, "online");
    }
  }

  async function refreshInputDevices() {
    if (inputDevicesLoading) return;
    const startedAt = performance.now();
    setInputDevicesLoading(true);
    try {
      const devices = await invoke<InputDevice[]>("list_audio_input_devices");
      setInputDevices(devices);
      setSelectedInputDevice((current) => devices.some((device) => device.id === current) ? current : devices[0]?.id ?? "");
    } catch (cause) {
      setInputDevices([]);
      setError(errorMessage(cause));
    } finally {
      const remainingRotationMs = Math.max(0, 1000 - (performance.now() - startedAt));
      if (remainingRotationMs > 0) {
        await new Promise((resolve) => window.setTimeout(resolve, remainingRotationMs));
      }
      setInputDevicesLoading(false);
    }
  }

  async function transcribe() {
    if (sourceMode === "system" || transcriptionBusyBySlot[sourceMode]) return;
    if (!audio || !selectedAudioTrack) return;
    if (!selectedKey) {
      setSettingsOpen(true);
      return;
    }
    if (!selectedModel) {
      setError("The selected API key has no supported transcription models available.");
      return;
    }
    const queuedTracks = audioOwner === "file" && audioQueue.length > 0
      ? audioQueue.map((item) => item.path === audio.path ? selectedAudioTrack : item.audioTracks[0])
      : [selectedAudioTrack];
    if (queuedTracks.some((track) => track?.requiresConversion)) {
      setPendingConversion("file");
      setConversionOpen(true);
      return;
    }
    await runTranscription(false);
  }

  async function runTranscription(allowReencode: boolean) {
    if (!audio || !selectedModel || selectedAudioStreamIndex === null) return;
    const taskSlot = audioOwner;
    if (transcriptionBusyBySlot[taskSlot]) return;
    const taskId = crypto.randomUUID();
    const sources = taskSlot === "file" && audioQueue.length ? audioQueue : [audio];
    startTechnicalTask(sources.length > 1 ? `Transcription · ${sources.length} audio files` : `Transcription ${audio.name}`);
    recordTechnicalLog(`Model · ${taskSlot}`, `Selected ${model}.`, taskSlot);
    setTranscriptionBusyBySlot((current) => ({ ...current, [taskSlot]: true }));
    setTranscriptionCancelBySlot((current) => ({ ...current, [taskSlot]: false }));
    setCompleted(false);
    setError("");
    setTranscript("");
    setOriginalTranscript("");
    setEnhancedTranscript("");
    transcriptRef.current = "";
    setSummaryText("");
    summaryTextRef.current = "";
    setNotesText(""); notesTextRef.current = ""; setNotesMode(false); notesModeRef.current = false; notesStyleRef.current = "meeting"; setNotesStyle("meeting"); setNotesTranslations({}); setSummaryTranslationLive(false); summaryTranslationLiveRef.current = false;
    setSummaryDraft("");
    setSummaryStatus(summaryModeRef.current ? "started" : "idle");
    setSummaryTranslations({});
    setQuestionMessages([]);
    setQuestionDraft("");
    transcriptParts.current = {};
    completedParts.current.clear();
    setTranslations({});
    setTranslationEnabled(false);
    setActiveLanguage("Original");
    const sessionName = sources.length > 1 ? `${sources[0].name} + ${sources.length - 1} parts` : sources[0].name;
    const sessionId = startTranscriptSession(sessionName);
    const task = { id: taskId, sessionId, sessionName, chunkOffset: 0, totalChunks: 0, fileIndex: 1, fileCount: sources.length, active: true, parts: {} as Record<number, string>, complete: new Set<number>(), attempts: {} as Record<number, number>, persistence: Promise.resolve(), phase: "Starting transcription", progress: 4, error: "" };
    transcriptionTasks.current[taskSlot] = task;
    Object.assign(transcriptTabs.current[taskSlot], { transcript: "", originalTranscript: "", enhancedTranscript: "", summaryText: "", notesText: "", notesMode: false, notesStyle: "meeting", notesTranslations: {}, translations: {}, summaryTranslations: {}, questionMessages: [], activeLanguage: "Original", completed: false, summaryMode: false, sessionId, sessionName });
    transcriptionChunkOffset.current = 0;
    currentTranscriptionTotal.current = 0;
    batchTranscriptionActive.current = sources.length > 1;
    setProgress(4);
    setPhase("Starting transcription");
    try {
      const results: string[] = [];
      for (const [index, source] of sources.entries()) {
        task.fileIndex = index + 1;
        currentTranscriptionFile.current = index + 1;
        currentTranscriptionTotal.current = 0;
        setCompleted(false);
        setPhase(sources.length > 1 ? `Starting file ${index + 1} of ${sources.length}` : "Starting transcription");
        const request: TranscribeRequest = {
          path: source.path,
          compressAudio: false,
          audioQuality: captureQuality,
          audioStreamIndex: source.path === audio.path ? selectedAudioStreamIndex : (source.audioTracks[0]?.streamIndex ?? 0),
          allowReencode,
          model,
          parallelRequests,
          jobId: crypto.randomUUID(),
          taskId,
          taskSlot,
          taskComplete: index === sources.length - 1,
          contextPrompt: source.contextPrompt || (source.path === audio.path ? transcriptionContextPrompt : "") || null,
        };
        results.push(await invoke<string>("transcribe_audio", { request }));
        task.chunkOffset += task.totalChunks;
      }
      const finalText = results.filter((text) => text.trim()).join("\n\n");
      Object.assign(transcriptTabs.current[taskSlot], { transcript: finalText, originalTranscript: finalText, completed: true });
      task.persistence = task.persistence.then(() => invoke("replace_final_transcript", { sessionId: task.sessionId, name: task.sessionName, text: finalText })).then(() => invoke("cleanup_saved_transcript_chunks", { sessionId: task.sessionId })).then(() => undefined).catch((cause) => { if (sourceModeRef.current === taskSlot) setError(`Transcript completed but could not be finalized in Library: ${errorMessage(cause)}`); });
      await task.persistence;
      if (sourceModeRef.current === taskSlot && transcriptionTasks.current[taskSlot]?.id === taskId) {
        setTranscript(finalText);
        transcriptRef.current = finalText;
        setOriginalTranscript(finalText);
        setCompleted(true);
        setPhase("Transcription complete");
        setProgress(100);
      }
      await refreshSavedTranscripts();
      if (sourceModeRef.current === taskSlot && summaryModeRef.current) await generateFinalSummary(finalText);
      await refreshResumableJobs();
    } catch (cause) {
      const message = errorMessage(cause);
      recordTechnicalLog(`Transcription · ${taskSlot}`, `Failed: ${message}`, taskSlot);
      if (message.includes("REENCODE_APPROVAL_REQUIRED")) {
        task.phase = "Conversion approval required";
        task.error = message;
        setPendingConversion(taskSlot);
        if (sourceModeRef.current === taskSlot) { setConversionOpen(true); setPhase("Conversion approval required"); }
      } else {
        task.phase = "Transcription could not be completed";
        task.error = message;
        if (sourceModeRef.current === taskSlot) { setError(message); setPhase("Transcription could not be completed"); }
        void refreshResumableJobs();
      }
    } finally {
      batchTranscriptionActive.current = false;
      setTranscriptionBusyBySlot((current) => ({ ...current, [taskSlot]: false }));
      setTranscriptionCancelBySlot((current) => ({ ...current, [taskSlot]: false }));
      task.active = false;
      await task.persistence;
      await refreshSavedTranscripts();
    }
  }

  async function cancelTranscription() {
    if (sourceMode === "system" || !busy || transcriptionCancelRequested) return;
    const taskSlot = sourceMode;
    const taskId = transcriptionTasks.current[taskSlot]?.id;
    if (!taskId) return;
    setTranscriptionCancelBySlot((current) => ({ ...current, [taskSlot]: true }));
    setPhase("Canceling this task and saving completed chunks");
    try { await invoke("cancel_transcription", { taskId, taskSlot }); }
    catch (cause) { setError(errorMessage(cause)); }
  }

  async function resumeJob(job: ResumableJob) {
    if (transcriptionBusyBySlot.file) return;
    const taskSlot = "file" as const;
    const taskId = crypto.randomUUID();
    setTranscriptionBusyBySlot((current) => ({ ...current, file: true }));
    setTranscriptionCancelBySlot((current) => ({ ...current, file: false }));
    setCompleted(false);
    setError("");
    setTranscript("");
    transcriptRef.current = "";
    const sessionId = startTranscriptSession(job.sourceName);
    const task = { id: taskId, sessionId, sessionName: job.sourceName, chunkOffset: 0, totalChunks: job.totalChunks, fileIndex: 1, fileCount: 1, active: true, parts: {} as Record<number, string>, complete: new Set<number>(), attempts: {} as Record<number, number>, persistence: Promise.resolve(), phase: `Resuming ${job.sourceName}`, progress: 4, error: "" };
    transcriptionTasks.current.file = task;
    Object.assign(transcriptTabs.current.file, { transcript: "", originalTranscript: "", sessionId, sessionName: job.sourceName, completed: false });
    transcriptParts.current = {};
    completedParts.current.clear();
    setProgress(4);
    setPhase(`Resuming ${job.sourceName} · ${job.completedChunks}/${job.totalChunks} chunks already complete`);
    try {
      const finalText = await invoke<string>("resume_transcription", { jobId: job.id, taskId, taskSlot });
      task.persistence = task.persistence.then(() => invoke("replace_final_transcript", { sessionId, name: job.sourceName, text: finalText })).then(() => invoke("cleanup_saved_transcript_chunks", { sessionId })).then(() => undefined).catch((cause) => { if (sourceModeRef.current === "file") setError(`Transcript completed but could not be finalized in Library: ${errorMessage(cause)}`); });
      await task.persistence;
      Object.assign(transcriptTabs.current.file, { transcript: finalText, originalTranscript: finalText, completed: true, sessionId, sessionName: job.sourceName });
      if (sourceModeRef.current === "file") { setTranscript(finalText); transcriptRef.current = finalText; setCompleted(true); setProgress(100); setPhase("Transcription complete"); }
      await refreshSavedTranscripts();
      if (sourceModeRef.current === "file" && summaryModeRef.current) await generateFinalSummary(finalText);
    } catch (cause) {
      task.phase = "Transcription could not be resumed";
      task.error = errorMessage(cause);
      if (sourceModeRef.current === "file") { setError(errorMessage(cause)); setPhase(task.phase); }
    } finally {
      setTranscriptionBusyBySlot((current) => ({ ...current, file: false }));
      setTranscriptionCancelBySlot((current) => ({ ...current, file: false }));
      task.active = false;
      await refreshResumableJobs();
      await transcriptPersistenceQueue.current;
      await refreshSavedTranscripts();
    }
  }

  async function startSystemAudio() {
    if (microphoneRateConstraint && !availableCaptureQualities.includes(captureQuality)) {
      setError("The selected microphone does not support a high enough sample rate for this quality. Choose a listed quality or select another microphone.");
      return;
    }
    if (captureMode === "standard" && !selectedSystemModel) return;
    if (captureMode === "live" && !liveModelAvailable) return;
    if ((captureSource === "microphone" || captureSource === "both") && !selectedInputDevice) {
      setError("Connect and select a microphone before recording.");
      return;
    }
    setError("");
    startTechnicalTask(captureMode === "live" ? `Live audio ${captureSource}` : `Standard audio capture ${captureSource}`);
    setSystemWarning("");
    setTranscript("");
    transcriptRef.current = "";
    liveFinalTranscript.current = "";
    setSummaryText("");
    summaryTextRef.current = "";
    setNotesText(""); notesTextRef.current = ""; setNotesMode(false); notesModeRef.current = false; notesStyleRef.current = "meeting"; setNotesStyle("meeting"); setNotesTranslations({}); setSummaryTranslationLive(false); summaryTranslationLiveRef.current = false;
    setSummaryDraft("");
    setTranslations({});
    setTranslationEnabled(false);
    setActiveLanguage("Original");
    setCompleted(false);
    setSystemElapsed(0);
    setSystemChunkElapsed(0);
    setSystemForceRemaining(150);
    setSystemCompletedChunks(0);
    systemCommittedTranscript.current = "";
    systemCompletedChunksRef.current = 0;
    systemStreamedParts.current = {};
    transcriptTabs.current.system.transcript = "";
    transcriptTabs.current.system.originalTranscript = "";
    setSystemQueuedChunks(0);
    setSystemActiveRequests(0);
    setSystemEstimatedCost(0);
    setProgress(0);
    setLiveElapsed(0);
    setLiveEstimatedCost(0);
    setLiveMessage("");
    setInputLevel(0);
    startTranscriptSession(captureSource === "both" ? "System Audio + Microphone" : captureSource === "system" ? "System Audio" : "Microphone");
    setPhase(captureMode === "live" ? "Starting live transcription" : "Starting audio capture");
    const capturePreset = CAPTURE_QUALITY_PRESETS.find((preset) => preset.key === captureQuality)!;
    const { sampleRate: captureRate, bitrate: captureBitrate, label: captureLabel } = capturePreset;
    const compactApplies = compactEnabled;
    setCompactChangeDeferred(false);
    recordTechnicalLog(captureMode === "live" ? "Live audio" : "Audio capture", compactApplies
      ? `Selected quality: ${captureLabel} · mono AAC · ${captureRate / 1000} kHz / ${captureBitrate} kbps; encoded during capture.`
      : `Selected quality: ${captureLabel} · mono PCM/WAV · ${captureRate / 1000} kHz; Compact is off.`, "system");
    if (captureMode === "live") recordTechnicalLog("Live audio", "OpenAI Realtime input: 24 kHz · 16-bit · mono PCM; saved recording keeps the selected capture quality.", "system");
    if (captureMode === "standard") setSystemRecording(true);
    else setLiveRecording(true);
    try {
      if (captureMode === "standard") {
        await invoke("start_system_audio", { model: systemModel, source: captureSource, inputDevice: captureSource !== "system" ? selectedInputDevice : null, parallelRequests, quality: captureQuality, compactMicrophone: compactEnabled, compactSystemAudio: compactEnabled });
      } else {
        await invoke("start_realtime_audio", { source: captureSource, inputDevice: captureSource !== "system" ? selectedInputDevice : null, quality: captureQuality, compactMicrophone: compactEnabled, compactSystemAudio: compactEnabled });
      }
    } catch (cause) {
      systemCaptureFinalized.current = true;
      setCompactChangeDeferred(false);
      recordTechnicalLog("Audio capture", `Start failed: ${errorMessage(cause)}`, "system");
      setSystemRecording(false);
      setLiveRecording(false);
      setError(errorMessage(cause));
      setPhase("Audio capture could not be started");
    }
  }

  async function stopSystemAudio() {
    if ((!systemRecording && !liveRecording) || systemStopping || liveStopping) return;
    if (liveRecording) {
      recordTechnicalLog("Live audio", "Stop requested.", "system");
      setLiveStopping(true);
      setPhase("Finalizing live transcription");
      try {
        await invoke("stop_realtime_audio");
        recordTechnicalLog("Live audio", "Stop signal accepted; waiting for final transcript.", "system");
      } catch (cause) {
        const message = errorMessage(cause);
        setLiveStopping(false);
        setError(message);
        recordTechnicalLog("Live audio", `Stop failed: ${message}`, "system");
      }
      return;
    }
    recordTechnicalLog("Audio capture", "Stop requested.", "system");
    setSystemStopping(true);
    setPhase("Finishing the remaining audio and queued chunks");
    try {
      await invoke("stop_system_audio");
      recordTechnicalLog("Audio capture", "Stop signal accepted; waiting for queued transcription.", "system");
    } catch (cause) {
      const message = errorMessage(cause);
      setSystemStopping(false);
      setError(message);
      recordTechnicalLog("Audio capture", `Stop failed: ${message}`, "system");
    }
  }

  function toggleTranslation() {
    if (translationEnabled) setActiveLanguage("Original");
    setTranslationEnabled((enabled) => !enabled);
  }

  async function translateTranscript() {
    if (!completed || !transcript || !requestedTargetLanguage || !selectedTranscriptTranslationModel) return;
    if (translations[targetTranslationKey]) {
      setActiveLanguage(targetTranslationKey);
      return;
    }
    setTranslating(true);
    const taskSlot = sourceModeRef.current;
    translationLogSlotRef.current = taskSlot;
    recordTechnicalLog("Translation", `Started · ${requestedTargetLanguage}.`, taskSlot);
    recordTechnicalLog("Model", `Selected ${transcriptTranslationModel}.`, taskSlot);
    setError("");
    translatingLanguage.current = targetTranslationKey;
    setTranslations((current) => ({ ...current, [targetTranslationKey]: "" }));
    setActiveLanguage(targetTranslationKey);
    try {
      const translated = await invoke<string>("translate_transcript", {
        text: transcript,
        targetLanguage: requestedTargetLanguage,
        model: transcriptTranslationModel,
      });
      setTranslations((current) => ({ ...current, [targetTranslationKey]: translated }));
      setActiveLanguage(targetTranslationKey);
      await invoke("save_transcript_translation", {
        sessionId: transcriptSessionId.current,
        sourceName: transcriptSessionName.current,
        language: requestedTargetLanguage,
        text: translated,
      });
      await refreshSavedTranscripts();
    } catch (cause) {
      recordTechnicalLog("Translation", `Failed: ${errorMessage(cause)}`, taskSlot);
      setTranslations((current) => {
        const next = { ...current };
        delete next[targetTranslationKey];
        return next;
      });
      setActiveLanguage("Original");
      setError(errorMessage(cause));
    } finally {
      translatingLanguage.current = "";
      setTranslating(false);
    }
  }

  function openDownloadDialog() {
    if (!completed) return;
    setDownloadLanguage(transcriptVersions.some((version) => version.language === activeLanguage) ? activeLanguage : "Original");
    setDownloadOpen(true);
  }

  async function downloadTranscript() {
    const version = transcriptVersions.find((item) => item.language === downloadLanguage);
    if (!completed || !version) return;
    const timestamp = Date.now();
    const transcriptBaseName = transcriptSessionName.current.trim().replace(/[/\\:*?"<>|]/g, "_") || `transcript_${timestamp}`;
    const destination = await save({ defaultPath: `${transcriptBaseName}.txt`, filters: [{ name: "Text", extensions: ["txt"] }] });
    if (destination) {
      await invoke("write_transcript", { path: destination, content: version.text });
      setDownloadOpen(false);
    }
  }

  function reset() {
    setAudio(null);
    setAudioQueue([]);
    audioQueueRef.current = [];
    setSelectedAudioStreamIndex(null);
    setConversionOpen(false);
    setTranscript("");
    setOriginalTranscript("");
    setEnhancedTranscript("");
    transcriptRef.current = "";
    liveFinalTranscript.current = "";
    setSummaryText("");
    summaryTextRef.current = "";
    setNotesText(""); notesTextRef.current = ""; setNotesMode(false); notesModeRef.current = false; notesStyleRef.current = "meeting"; setNotesStyle("meeting"); setNotesTranslations({}); setSummaryTranslationLive(false); summaryTranslationLiveRef.current = false;
    setSummaryDraft("");
    setSummaryStatus("idle");
    setSummaryTranslations({});
    setQuestionMessages([]);
    setQuestionDraft("");
    transcriptParts.current = {};
    completedParts.current.clear();
    setTranslations({});
    setTranslationEnabled(false);
    setActiveLanguage("Original");
    setCompleted(false);
    setError("");
    setProgress(0);
    setPhase("Choose an audio file to begin");
  }

  return (
    <div className="app-shell">
      <nav>
        <div className="brand"><span className="brand-mark"><AudioLines size={19} /></span>Minefia <em>Audio</em></div>
        {(workspaceReady || accessState === "error") && <div className="nav-actions">
          <button className="settings-button library-nav-button" onClick={() => { setLibraryOpen(true); void refreshLibraryFiles(); void refreshSavedTranscripts(); }}><FolderOpen size={16} /> Library</button>
          {adminApiKeys.length > 0 && projectUsageReady && <button className="settings-button" onClick={() => { setUsageOpen(true); void refreshUsage(); }}><CircleDollarSign size={16} /> Usage</button>}
          {(systemRecording || liveRecording) && <button className="settings-button capture-stop-nav" onClick={() => void stopSystemAudio()} disabled={systemStopping || liveStopping}><Radio size={15} /> {systemStopping || liveStopping ? "Finishing capture…" : "Stop capture"}</button>}
          <button className="settings-button" onClick={() => setSettingsOpen(true)}>
            <span className={`status-dot ${workspaceReady ? "connected" : selectedKey ? "error" : ""}`} />
            {selectedKey ? selectedKey.label : "Set up API key"}
            <Settings size={17} />
          </button>
        </div>}
      </nav>

      <main className={workspaceReady ? undefined : "access-main"}>
        {!workspaceReady ? (
          <ApiAccessGate
            state={accessState}
            keyLabel={selectedKey?.label}
            error={accessState === "error" ? accessError : undefined}
            adminKeys={adminApiKeys}
            onOpenSettings={() => setSettingsOpen(true)}
            onRetry={refreshSettingsKeys}
          />
        ) : <>
          <header className="hero">
          <div className="stream-pill"><span className="pulse-dot" /> STREAM = TRUE</div>
          <h1>Turn audio into <span>useful words.</span></h1>
          <p>Transcribe local media, system audio, or a selected microphone with standard or low-latency live processing.</p>
        </header>

        <div className="source-mode-tabs" role="tablist" aria-label="Transcription source">
          <button className={sourceMode === "file" ? "selected" : ""} onClick={() => switchSourceMode("file")} disabled={summaryGenerating || summaryTranslating || questionBusy || translating || enhancementRunning || onlineInspecting || onlineExtracting || systemStopping || liveStopping}><FileAudio size={15} /> Local file{transcriptionBusyBySlot.file && <i className="tab-busy-dot" />}</button>
          <button className={sourceMode === "online" ? "selected" : ""} onClick={() => switchSourceMode("online")} disabled={summaryGenerating || summaryTranslating || questionBusy || translating || enhancementRunning || onlineInspecting || onlineExtracting || webPageInspecting || systemStopping || liveStopping}><Globe size={15} /> Online media{transcriptionBusyBySlot.online && <i className="tab-busy-dot" />}</button>
          <button className={sourceMode === "system" ? "selected" : ""} onClick={() => switchSourceMode("system")} disabled={summaryGenerating || summaryTranslating || questionBusy || translating || enhancementRunning || onlineInspecting || onlineExtracting || systemStopping || liveStopping}><MonitorSpeaker size={15} /> Audio capture</button>
        </div>

        <section className="workspace-grid">
          <div className="input-column">
            {resumableJobs.length > 0 && <div className="resumable-jobs card"><strong>Interrupted transcription jobs</strong><p>Completed chunks are saved and will not be uploaded again.</p>{resumableJobs.map((job) => <div className="resumable-job" key={job.id}><span><b>{job.sourceName}</b><small>{job.completedChunks}/{job.totalChunks} chunks · {job.model}</small></span><button onClick={() => void resumeJob(job)} disabled={transcriptionBusyBySlot.file}>{transcriptionBusyBySlot.file ? "Working…" : "Resume"}</button></div>)}</div>}
            {sourceMode === "file" ? <>
            {!audio || audioOwner !== "file" ? (
              <button className={`drop-zone ${dragging ? "dragging" : ""}`} onClick={() => void chooseAudio()} disabled={localImporting || transcriptionBusyBySlot.file}>
                <span className="upload-orbit">{localImporting ? <Loader2 className="spin" size={24} /> : <Upload size={24} />}</span>
                <strong>Drop an audio or video file here</strong>
              <span>or click to choose one or more files</span>
                <small>MP4 · MOV · WEBM · MKV · FLAC · MP3 · M4A · OGG · WAV · FFmpeg formats</small>
                <small>Minefia keeps an app copy; your original file stays untouched.</small>
                <small>If one recording is split across files, add every part and arrange them in order.</small>
              </button>
            ) : (
              <div className="file-card card">
                <div className="file-card-top">
                  <div className="file-icon"><FileAudio size={24} /></div>
                  <div className="file-title"><strong>{audio.name}</strong><span>{audio.isVideo ? "VIDEO" : "AUDIO"} · {formatBytes(audio.sizeBytes)} · {formatDuration(audio.durationSeconds)}</span></div>
                  {!busy && <button className="icon-button" onClick={reset} aria-label="Remove file"><X size={18} /></button>}
                </div>
                {audio.audioTracks.length > 1 && <label className="audio-track-field">
                  <span>Audio track</span>
                  <select value={selectedAudioStreamIndex ?? ""} onChange={(event) => setSelectedAudioStreamIndex(Number(event.target.value))} disabled={busy}>
                    {audio.audioTracks.map((track, index) => (
                      <option key={track.streamIndex} value={track.streamIndex}>
                        Track {index + 1} · {track.title || track.language || track.codec.toUpperCase()} · {track.codec.toUpperCase()}
                      </option>
                    ))}
                  </select>
                </label>}
                {selectedAudioTrack?.requiresConversion && <p className="conversion-required-note">This track needs conversion. You will be asked before any re-encoding starts.</p>}
                <div className="waveform" aria-hidden="true">
                  {Array.from({ length: 58 }, (_, index) => <i key={index} style={{ height: `${15 + ((index * 29) % 70)}%` }} />)}
                </div>
                <div className="metadata-row">
                  <span><small>CONTAINER</small>{audio.container.split(",")[0]?.toUpperCase() || "—"}</span>
                  <span><small>CODEC</small>{selectedAudioTrack?.codec.toUpperCase() ?? "—"}</span>
                  <span><small>SAMPLE RATE</small>{selectedAudioTrack?.sampleRate ? `${(selectedAudioTrack.sampleRate / 1000).toFixed(1)} kHz` : "—"}</span>
                  <span><small>CHANNELS</small>{selectedAudioTrack?.channels === 1 ? "MONO" : selectedAudioTrack?.channels === 2 ? "STEREO" : selectedAudioTrack?.channels ?? "—"}</span>
                </div>
                <div className="media-preview">
                  <div className="section-label"><AudioLines size={15} /><span>Preview · seek and check duration</span></div>
                  {audio.isVideo ? (
                    <video className="media-preview-video" controls preload="metadata" src={convertFileSrc(audio.path)} aria-label={`Preview ${audio.name}`} />
                  ) : (
                    <audio className="media-preview-audio" controls preload="metadata" src={convertFileSrc(audio.path)} aria-label={`Preview ${audio.name}`} />
                  )}
                </div>
              </div>
            )}
            {sourceMode === "file" && audioQueue.length > 1 && <div className="multi-audio-queue card">
              <div className="section-label"><FileAudio size={15} /><span>{audioQueue.length} files · processed in selected order</span></div>
              <p>For recordings split into parts, select every part and keep them in the correct order. Each file is transcribed in sequence and combined into one transcript.</p>
              <ol>{audioQueue.map((item, index) => <li key={`${item.path}-${index}`}><span>{String(index + 1).padStart(2, "0")}</span><strong title={item.name}>{item.name}</strong><small>{formatDuration(item.durationSeconds)}</small><button aria-label={`Move ${item.name} up`} title="Move up" onClick={() => moveQueuedAudio(index, -1)} disabled={busy || index === 0}><ChevronUp size={13} /></button><button aria-label={`Move ${item.name} down`} title="Move down" onClick={() => moveQueuedAudio(index, 1)} disabled={busy || index === audioQueue.length - 1}><ChevronDown size={13} /></button><button aria-label={`Remove ${item.name} from queue`} title="Remove from queue" onClick={() => removeQueuedAudio(index)} disabled={busy}><X size={12} /></button></li>)}</ol>
            </div>}
            {sourceMode === "file" && audio && audioOwner === "file" && <button className="add-audio-button" onClick={() => void chooseAudio(true)} disabled={localImporting || busy}><Upload size={14} /> Add audio files</button>}

            <div className="model-card card">
      <div className="section-label"><Zap size={15} /><span>Transcription model</span></div>
              <div className="model-options">
                {availableModels.map((item) => (
                  <button key={item.id} className={`model-option ${model === item.id ? "selected" : ""}`} onClick={() => setModel(item.id)} disabled={busy || modelsLoading}>
                    <span className="model-radio">{model === item.id && <i />}</span>
                    <span className="model-copy"><strong>{item.name}<em>{item.tag}</em></strong><small>{item.description}</small><b>{transcriptionRateLabel(item)}{audio && !item.pricingLabel ? ` · ${formatUsd(estimateTranscriptionCost(item.id, queuedDuration))} est.` : ""}</b></span>
                  </button>
                ))}
                {modelsLoading && <p className="validation-note">Loading models available to this API key…</p>}
                {!modelsLoading && selectedKey && availableModels.length === 0 && <p className="form-error">This API key returned no supported transcription models.</p>}
                {!modelsLoading && !selectedKey && <p className="validation-note">Add or select an API key to load its available models.</p>}
              </div>
            </div>

            {audio && audioOwner === "file" && selectedModel && (
              <div className="cost-card card">
                <div className="cost-icon"><CircleDollarSign size={19} /></div>
                <div className="cost-main">
                  <span>{selectedModel.pricingLabel ? "Transcription pricing" : "Estimated transcription cost"}</span>
                  <strong>{selectedModel.pricingLabel ? "Varies by usage" : <>{formatUsd(estimatedCost)} <small>USD</small></>}</strong>
                  <p>{selectedModel.name} · {formatDuration(queuedDuration)} · about {estimatedParts} upload {estimatedParts === 1 ? "part" : "parts"}{audioQueue.length > 1 ? ` · ${audioQueue.length} files` : ""}</p>
                </div>
                <div className="cost-rate"><small>RATE</small>{transcriptionRateLabel(selectedModel)}</div>
                <p className="estimate-note">{selectedModel.pricingLabel ? "Token-based pricing; the final amount depends on audio and transcript token usage." : "Duration-based estimate from OpenAI pricing. Actual billing may vary."} Translation is billed separately by text tokens.</p>
              </div>
            )}

            {audio && audioOwner === "file" && <div className="transcription-prompt-card card">
              <label htmlFor="transcription-context-prompt"><div className="section-label"><FileText size={15} /><span>Transcription prompt context</span></div></label>
              <p>Optional context sent with each audio part. YouTube title and description are prefilled when available; edit them or add your own terms.</p>
              <textarea id="transcription-context-prompt" value={transcriptionContextPrompt} onChange={(event) => updateContextPrompt("file", event.target.value)} placeholder="Names, terminology, or context to help recognize the audio…" disabled={busy} rows={5} />
              <small>This is context for transcription, not an instruction to translate or summarize. The actual prompt sent is shown below after transcription starts.</small>
            </div>}

            <button className="primary-button transcribe-button" onClick={transcribe} disabled={!audio || audioOwner !== "file" || !selectedAudioTrack || busy || !selectedModel}>
              {busy ? <Loader2 className="spin" size={19} /> : <AudioLines size={19} />}
              {busy ? "Transcribing…" : "Start transcription"}
              {!busy && <ArrowRight size={18} />}
            </button>
            {!selectedKey && <button className="key-hint" onClick={() => setSettingsOpen(true)}><KeyRound size={14} />Add an API key to get started</button>}
            </> : sourceMode === "online" ? <>
              {onlineMedia && <div className="online-media-card card">
                <div className="system-audio-heading"><span className="system-audio-icon"><AudioLines size={21} /></span><div><p>{onlineMedia.sourceKind}</p><h3>{onlineMedia.title}</h3></div><span className="standard-badge"><i />AUDIO FOUND</span></div>
                {onlineMedia.captions.length > 0 && <div className="online-quality-select caption-picker">
                  <span>Captions found · {onlineMedia.captions.length} available</span>
                  <p>Captions are available. Select one to use as the transcript—no request to the Transcriptions API is needed.</p>
                  <select value={selectedOnlineCaptionId} onChange={(event) => setSelectedOnlineCaptionId(event.target.value)} disabled={onlineCaptionLoading || onlineExtracting || transcriptionBusyBySlot.online}>
                    {onlineMedia.captions.map((caption) => <option key={caption.id} value={caption.id}>{caption.label}</option>)}
                  </select>
                  <button className="primary-button transcribe-button" onClick={() => void useSelectedOnlineCaption()} disabled={!selectedOnlineCaptionId || onlineCaptionLoading || onlineExtracting || transcriptionBusyBySlot.online}>
                    {onlineCaptionLoading ? <Loader2 className="spin" size={18} /> : <FileText size={18} />}
                    {onlineCaptionLoading ? "Loading caption…" : "Use selected caption as transcript"}
                  </button>
                </div>}
                {onlineMedia.sourceKind.toLowerCase().includes("web page") && onlineSourceLabels.length > 0 ? <div className="online-source-selectors">
                  <label className="online-quality-select online-source-choice"><span>Detected media source</span><select value={onlineFormatSourceLabel} onChange={(event) => selectOnlineSource(event.target.value)} disabled={onlineExtracting || transcriptionBusyBySlot.online}>{visibleOnlineSourceLabels.map((label) => <option value={label} key={label}>{label}</option>)}</select><small>{onlineHlsSourceLabels.length > 0 && !onlineShowAllSources ? "The first detected HLS playlist is selected. Other playlists and direct video/audio responses are hidden." : "Choose a detected HLS playlist or a direct video/audio response."}</small></label>
                  {onlineFormatSourceLabel.startsWith("HLS playlist ") && selectedOnlineSourceFormats.length > 1 && <label className="online-quality-select online-hls-quality"><span>Audio quality in this playlist</span><select value={onlineMedia.formatId} onChange={(event) => selectOnlineFormat(event.target.value)} disabled={onlineExtracting || transcriptionBusyBySlot.online}>{selectedOnlineSourceFormats.map((format) => <option value={format.formatId} key={format.formatId}>{format.recommended ? `Best available · ${format.formatLabel}` : format.formatLabel}</option>)}</select><small>Quality variants are shown only after selecting this HLS playlist.</small></label>}
                  {hiddenOnlineSourceCount > 0 && <button type="button" className="online-load-more" onClick={() => setOnlineShowAllSources(true)}>Load more sources <span>{hiddenOnlineSourceCount}</span></button>}
                </div> : (onlineMedia.availableFormats.length > 1 || onlineMedia.sourceKind.toLowerCase().includes("hls")) && <label className="online-quality-select"><span>{onlineMedia.sourceKind.toLowerCase().includes("hls") ? "HLS audio stream" : "Audio quality"}</span><select value={onlineMedia.formatId} onChange={(event) => selectOnlineFormat(event.target.value)} disabled={onlineExtracting || transcriptionBusyBySlot.online}>{onlineMedia.availableFormats.map((format) => <option value={format.formatId} key={format.formatId}>{format.recommended ? `Best audio quality (recommended) · ${format.formatLabel}` : format.formatLabel}</option>)}</select><small>Choose the playlist stream to download. Best audio quality prefers the highest available bitrate and sample rate. Audio-only streams avoid downloading video.</small></label>}
                <div className="online-format"><small>SELECTED FORMAT</small>{onlineFormatSourceLabel && <span className="online-selected-source">{onlineFormatSourceLabel}</span>}<strong>{onlineMedia.formatLabel}</strong><span>{onlineMedia.audioOnly ? "Audio-only stream · no video download" : "Audio and video stream · video will be temporary"}</span></div>
                <p className="online-quality-note">The selected stream is saved in its original audio format without re-encoding.</p>
                {onlineMedia.hasVideo && <label className="keep-video-option"><input type="checkbox" checked={onlineKeepVideo} onChange={(event) => setOnlineKeepVideo(event.target.checked)} disabled={onlineExtracting || transcriptionBusyBySlot.online} /><span><strong>Keep downloaded video in app storage</strong><small>Off by default. The temporary video is removed after audio extraction.</small></span></label>}
                <button className="primary-button transcribe-button" onClick={() => void extractOnlineAudio()} disabled={onlineExtracting || transcriptionBusyBySlot.online}>{onlineExtracting ? <Loader2 className="spin" size={18} /> : <AudioLines size={18} />}{onlineCancelRequested ? "Canceling…" : onlineExtracting ? "Extracting audio…" : "Extract audio"}{!onlineExtracting && <ArrowRight size={18} />}</button>
                {onlineExtracting && <button className="online-cancel-button" onClick={() => void cancelOnlineExtraction()} disabled={onlineCancelRequested}>{onlineCancelRequested ? "Cancel requested" : "Cancel extraction"}</button>}
              </div>}
              {webPageInfo && <div className="page-media-card card">
                <div className="section-label"><AudioLines size={15} /><span>Loaded sources · {webPageInfo.pageTitle}</span></div>
                {webPageInfo.media.map((item, index) => <div className="page-media-item" key={`${item.url}-${index}`}><div><strong>{item.label}</strong><small>{item.contentType || `${item.kind === "audio" ? "Audio" : "Video"} source`}{item.durationSeconds ? ` · ${formatDuration(item.durationSeconds)}` : ""}</small><code>{item.url}</code></div><button onClick={() => inspectPageSource(item.url)} disabled={onlineInspecting || onlineExtracting || transcriptionBusyBySlot.online}>Inspect source</button></div>)}
                {webPageInfo.networkMedia.map((item, index) => <div className="page-media-item" key={`${item.url}-${index}`}><div><strong>{item.label}</strong><small>Found in network response · {item.contentType || `${item.kind} media`}</small><code>{item.url}</code></div><button onClick={() => inspectPageSource(item.url)} disabled={onlineInspecting || onlineExtracting || transcriptionBusyBySlot.online}>Inspect response</button></div>)}
                {webPageInfo.masterPlaylists.map((url) => <div className="page-media-item" key={url}><div><strong>HLS master playlist</strong><small>Discovered from the browser network response</small><code>{url}</code></div><button onClick={() => inspectPageSource(url)} disabled={onlineInspecting || onlineExtracting || transcriptionBusyBySlot.online}>Inspect HLS</button></div>)}
                {webPageInfo.media.length === 0 && webPageInfo.networkMedia.length === 0 && webPageInfo.masterPlaylists.length === 0 && <p className="validation-note">No currently loaded source was found.</p>}
              </div>}
              {audio && audioOwner === "online" && <div className="file-card card">
                <div className="file-card-top"><div className="file-icon"><FileAudio size={24} /></div><div className="file-title"><strong>{audio.name}</strong><span>AUDIO · {formatBytes(audio.sizeBytes)} · {formatDuration(audio.durationSeconds)} · saved in app storage</span></div></div>
                <div className="media-preview"><div className="section-label"><AudioLines size={15} /><span>Preview · seek and check duration</span></div><audio className="media-preview-audio" controls preload="metadata" src={convertFileSrc(audio.path)} aria-label={`Preview ${audio.name}`} /></div>
              </div>}
              <div className="online-input-card card">
                <div className="section-label"><Globe size={15} /><span>Paste a URL</span></div>
                <p>Paste a YouTube link, HLS playlist, direct media URL, or webpage. Minefia detects the source type and checks for audio automatically. Only public or otherwise authorized sources are supported.</p>
                <div className="online-url-row"><input type="url" value={onlineUrl} onChange={(event) => setOnlineUrl(event.target.value)} placeholder="https://…" disabled={onlineInspecting || webPageInspecting || onlineExtracting || transcriptionBusyBySlot.online} onKeyDown={(event) => { if (event.key === "Enter") void inspectOnline(); }} /><button onClick={() => void inspectOnline()} disabled={!onlineUrl.trim() || onlineInspecting || webPageInspecting || onlineExtracting || transcriptionBusyBySlot.online}>{onlineInspecting || webPageInspecting ? <Loader2 className="spin" size={15} /> : <AudioLines size={15} />}{onlineInspecting || webPageInspecting ? "Checking…" : "Inspect"}</button>{(onlineInspecting || webPageInspecting) && <button className="online-cancel-button" onClick={() => void cancelOnlineExtraction()} disabled={onlineCancelRequested}>{onlineCancelRequested ? "Cancel requested" : "Cancel inspection"}</button>}</div>
                <small className="online-privacy-note">HLS playlists and direct media are inspected directly. For webpages, Minefia checks loaded media and network requests in its private browser. DRM, login walls, and access controls are not bypassed.</small>
              </div>

              <div className="model-card card">
                <div className="section-label"><Zap size={15} /><span>Transcription model</span></div>
                <div className="model-options">
                  {availableModels.map((item) => (
                    <button key={item.id} className={`model-option ${model === item.id ? "selected" : ""}`} onClick={() => setModel(item.id)} disabled={busy || modelsLoading}>
                      <span className="model-radio">{model === item.id && <i />}</span>
                      <span className="model-copy"><strong>{item.name}<em>{item.tag}</em></strong><small>{item.description}</small><b>{transcriptionRateLabel(item)}</b></span>
                    </button>
                  ))}
                  {modelsLoading && <p className="validation-note">Loading models available to this API key…</p>}
                  {!modelsLoading && selectedKey && availableModels.length === 0 && <p className="form-error">This API key returned no supported transcription models.</p>}
                  {!modelsLoading && !selectedKey && <p className="validation-note">Add or select an API key to load its available models.</p>}
                </div>
              </div>

              {audio && audioOwner === "online" && <div className="transcription-prompt-card card">
                <label htmlFor="online-transcription-context-prompt"><div className="section-label"><FileText size={15} /><span>Transcription prompt context</span></div></label>
                <p>Optional context sent with each audio part. YouTube title and description are prefilled when available; edit them or add your own terms.</p>
                <textarea id="online-transcription-context-prompt" value={transcriptionContextPrompt} onChange={(event) => updateContextPrompt("online", event.target.value)} placeholder="Names, terminology, or context to help recognize the audio…" disabled={busy} rows={5} />
                <small>This is context for transcription, not an instruction to translate or summarize. The actual prompt sent is shown below after transcription starts.</small>
              </div>}

              {webPageInfo && <div className="online-process-note card"><strong>Private browser profile</strong><p>Site cookies and login state are isolated to Minefia Audio. Direct media and discovered master playlists continue through the existing Inspect, confirmation, extraction, and playback flow.</p></div>}
              {audio && audioOwner === "online" && selectedModel && <div className="cost-card card"><div className="cost-icon"><CircleDollarSign size={19} /></div><div className="cost-main"><span>{selectedModel.pricingLabel ? "Transcription pricing" : "Estimated transcription cost"}</span><strong>{selectedModel.pricingLabel ? "Varies by usage" : <>{formatUsd(estimateTranscriptionCost(model, audio.durationSeconds))} <small>USD</small></>}</strong><p>{selectedModel.name} · {formatDuration(audio.durationSeconds)}</p></div><div className="cost-rate"><small>RATE</small>{transcriptionRateLabel(selectedModel)}</div><p className="estimate-note">{selectedModel.pricingLabel ? "Token-based pricing; exact cost depends on audio and transcript token usage." : "Duration-based estimate. Actual billing may vary."}</p></div>}
              {audio && audioOwner === "online" && <button className="primary-button transcribe-button" onClick={transcribe} disabled={busy || !selectedModel || !selectedAudioTrack}><AudioLines size={19} />{busy ? "Transcribing…" : "Start transcription"}{!busy && <ArrowRight size={18} />}</button>}
            </> : <>
              <div className="capture-source-controls">
                <label className={captureSource !== "microphone" ? "selected" : ""}><input type="checkbox" checked={captureSource !== "microphone"} onChange={() => toggleCaptureInput("system")} disabled={systemRecording || liveRecording} /><MonitorSpeaker size={15} /> System Audio</label>
                <label className={captureSource !== "system" ? "selected" : ""}><input type="checkbox" checked={captureSource !== "system"} onChange={() => toggleCaptureInput("microphone")} disabled={systemRecording || liveRecording} /><AudioLines size={15} /> Microphone</label>
              </div>
              <p className="capture-storage-note">Select either source or both. When both are selected, Minefia mixes them into one recording and transcript.</p>
              <div className="capture-mode-tabs">
                <button className={captureMode === "standard" ? "selected" : ""} onClick={() => setCaptureMode("standard")} disabled={systemRecording || liveRecording}>Standard Mode</button>
                <button className={captureMode === "live" ? "selected" : ""} onClick={() => setCaptureMode("live")} disabled={systemRecording || liveRecording}>Live Mode</button>
              </div>
              <p className="capture-storage-note">New microphone and system-audio recordings are saved in Minefia Audio’s app storage. Shared Compact is {compactEnabled ? `on · mono AAC at the selected ${captureQuality} quality` : "off · uncompressed mono WAV at the selected sample rate"}.</p>
              {captureMode === "live" && <p className="capture-storage-note">Speaker labels are not available in Live Mode. Choose Standard Mode and GPT-4o Transcribe Diarize to identify speakers.</p>}

              {(captureSource === "microphone" || captureSource === "both") && <div className="mic-device-card card">
                <label><span>Input device</span><select value={selectedInputDevice} onChange={(event) => setSelectedInputDevice(event.target.value)} disabled={systemRecording || liveRecording || inputDevices.length === 0}>
                  {inputDevices.length === 0 ? <option value="">No microphone detected</option> : inputDevices.map((device) => <option key={device.id} value={device.id}>{device.name} · {device.supportedSampleRates.length ? device.supportedSampleRates.map((rate) => `${rate / 1000} kHz`).join("/") : `default ${device.defaultSampleRate ? `${device.defaultSampleRate / 1000} kHz` : "rate"}`} · {device.supportedChannels.length ? device.supportedChannels.map((channels) => `${channels} ch`).join("/") : `${device.defaultChannels ?? "?"} ch`}</option>)}
                </select></label>
                <div className="device-state"><i className={inputDevices.some((device) => device.id === selectedInputDevice) ? "connected" : ""} />{inputDevices.some((device) => device.id === selectedInputDevice) ? "Device available" : "Device disconnected"}<span className={`compact-state ${compactEnabled ? "enabled" : "disabled"}`}><i />Capture Compact {compactEnabled ? "on" : "off"}</span><button onClick={() => void refreshInputDevices()} disabled={systemRecording || liveRecording || inputDevicesLoading} aria-label="Refresh input devices" aria-busy={inputDevicesLoading}><RefreshCw className={inputDevicesLoading ? "spin" : undefined} size={13} /></button></div>
                {(() => { const device = inputDevices.find((item) => item.id === selectedInputDevice); return device ? <small>Available formats: {device.supportedSampleRates.length ? device.supportedSampleRates.map((rate) => `${rate / 1000} kHz`).join(", ") : `device default ${device.defaultSampleRate ? `${device.defaultSampleRate / 1000} kHz` : "rate"}`}; {device.supportedChannels.length ? device.supportedChannels.map((channels) => `${channels} channel${channels === 1 ? "" : "s"}`).join(", ") : `default ${device.defaultChannels ?? "unknown"} channels`}. PCM mode uses only a supported preset rate; Compact AAC may resample.</small> : null; })()}
                <small>{compactEnabled ? `Microphone Compact is on in Settings: microphone audio uses mono AAC at the ${captureQuality} preset.` : `Microphone Compact is off in Settings: microphone audio is saved as uncompressed mono WAV at the selected ${captureQuality} sample rate.`}</small>
                <div className="input-level-meter" aria-label={`Input level ${Math.round(inputLevel * 100)} percent`}><i style={{ width: `${Math.round(inputLevel * 100)}%` }} /></div>
                <small>Recording begins only after you press Start.</small>
              </div>}

              <div className={`system-audio-card card ${(systemRecording || liveRecording) ? "recording" : systemStopping ? "stopping" : ""}`}>
                <div className="system-audio-heading">
                  <span className="system-audio-icon">{captureSource === "microphone" ? <AudioLines size={22} /> : <MonitorSpeaker size={22} />}</span>
                  <div><p>{captureSource === "both" ? "System Audio + Microphone" : captureSource === "system" ? "System Audio" : "Microphone"}</p><h3>{captureMode === "standard" ? "Standard Mode" : "Live Mode"}</h3></div>
                  <span className="standard-badge"><i />{systemRecording || liveRecording ? "RECORDING" : systemStopping ? "FINISHING" : "READY"}</span>
                </div>
                {captureMode === "standard" ? <>
                  <div className="system-metrics">
                  <div><small>ELAPSED</small><strong>{formatDuration(systemElapsed)}</strong></div>
                  <div><small>CURRENT CHUNK</small><strong>{formatDuration(systemChunkElapsed)}</strong></div>
                  <div><small>COMPLETED</small><strong>{systemCompletedChunks}</strong></div>
                  <div><small>RUNNING</small><strong>{systemActiveRequests}/{parallelRequests}</strong></div>
                  <div><small>QUEUED</small><strong>{systemQueuedChunks}</strong></div>
                  </div>
                  <div className={`silence-status ${systemWaitingForSilence ? "waiting" : ""}`}>
                  <Radio size={16} />
                  <span><strong>{systemWaitingForSilence ? "Waiting for silence" : "Building the current chunk"}</strong><small>{systemWaitingForSilence ? `Force split in ${formatDuration(systemForceRemaining)}` : "Silence splitting begins after 1:30"}</small></span>
                  </div>
                </> : <div className="live-session-status"><span className="pulse-dot" />{liveRecording ? liveMessage || "Streaming audio to the transcription service" : "Low-latency streaming transcription"}<small>{formatDuration(liveElapsed)} elapsed</small></div>}
                {systemWarning && <p className="system-warning"><CircleAlert size={14} />{systemWarning}</p>}
              </div>

              {captureMode === "standard" && <div className="model-card card">
                <div className="section-label"><Zap size={15} /><span>Standard Mode model</span></div>
                <div className="model-options">
                  {availableModels.map((item) => (
                    <button key={item.id} className={`model-option ${systemModel === item.id ? "selected" : ""}`} onClick={() => setSystemModel(item.id)} disabled={systemRecording || liveRecording || modelsLoading}>
                      <span className="model-radio">{systemModel === item.id && <i />}</span>
                      <span className="model-copy"><strong>{item.name}<em>{item.id === "gpt-transcribe" ? "Default" : item.tag}</em></strong><small>{item.description}</small><b>{item.pricingLabel ?? `$${item.pricePerMinute}/min · ${formatUsd(item.pricePerMinute * 60)}/hour`}</b></span>
                    </button>
                  ))}
                </div>
              </div>}

              {captureMode === "standard" && selectedSystemModel && <div className="cost-card card">
                <div className="cost-icon"><CircleDollarSign size={19} /></div>
                <div className="cost-main">
                  <span>{systemRecording ? "Estimated cost so far" : "Estimated Standard Mode cost"}</span>
                  <strong>{selectedSystemModel.pricingLabel ? "Varies by usage" : systemRecording ? formatUsd(systemEstimatedCost) : `${formatUsd(selectedSystemModel.pricePerMinute)} / min`} {!selectedSystemModel.pricingLabel && <small>USD</small>}</strong>
                  <p>{selectedSystemModel.name} · {selectedSystemModel.pricingLabel ? "Audio and transcript tokens determine the cost" : `${formatUsd(selectedSystemModel.pricePerMinute * 60)} per hour estimate`}</p>
                </div>
                <div className="cost-rate"><small>CONCURRENCY</small>2 requests max</div>
                <p className="estimate-note">{selectedSystemModel.pricingLabel ? "Token-based pricing; exact cost depends on usage." : "Duration-based planning estimate. Actual API billing may vary."}</p>
              </div>}

              {captureMode === "live" && <div className="cost-card card">
                <div className="cost-icon"><CircleDollarSign size={19} /></div>
                <div className="cost-main"><span>{liveElapsed > 0 ? "Estimated Live Mode cost so far" : "Estimated Live Mode cost"}</span><strong>{liveElapsed > 0 ? formatUsd(liveEstimatedCost) : "$1.02"} <small>USD {liveElapsed > 0 ? "total" : "/ hour"}</small></strong><p>gpt-live-transcribe · about $0.017 per minute</p></div>
                <div className="cost-rate"><small>MODEL</small>{liveModelAvailable ? "Available" : "Unavailable"}</div>
                <p className="estimate-note">Duration-based estimate. Live transcription is priced by audio usage; actual billing may vary.</p>
              </div>}

              <button className={`primary-button transcribe-button ${(systemRecording || liveRecording) ? "stop-recording" : ""}`} onClick={systemRecording || liveRecording ? stopSystemAudio : startSystemAudio} disabled={(captureMode === "standard" ? !selectedSystemModel : !liveModelAvailable) || (captureSource !== "system" && !selectedInputDevice) || (microphoneRateConstraint && availableCaptureQualities.length === 0) || systemStopping || liveStopping}>
                {systemStopping || liveStopping ? <Loader2 className="spin" size={19} /> : systemRecording || liveRecording ? <span className="stop-square" /> : <Radio size={19} />}
                {systemStopping ? "Finishing queued audio…" : liveStopping ? "Finalizing transcript…" : systemRecording || liveRecording ? "Stop recording" : "Start recording"}
                {!systemRecording && !liveRecording && !systemStopping && !liveStopping && <ArrowRight size={18} />}
              </button>
            </>}
          </div>

          <section className="transcript-card card">
            <div className="transcript-header">
              <div><p className="eyebrow">{sourceMode === "system" ? `${captureSource === "both" ? "SYSTEM AUDIO + MICROPHONE" : captureSource.toUpperCase()} · ${captureMode.toUpperCase()} MODE` : sourceMode === "online" ? "ONLINE MEDIA · STREAMING TRANSCRIPT" : "LOCAL FILE · STREAMING TRANSCRIPT"}</p><h2>Transcript</h2></div>
              <div className="transcript-header-actions">
                {completed && transcriptSessionId.current && transcriptRef.current.trim() && <button className="summary-start-button" onClick={() => void enhanceTranscript(transcriptRef.current)} disabled={enhancementRunning || busy || !availableTranslationModelIds.includes("gpt-6-luna")}>{enhancementRunning ? <><Loader2 className="spin" size={14} /> Enhancing…</> : <><Sparkles size={14} /> Enhance transcript</>}</button>}
                <button className="download-button" onClick={openDownloadDialog} disabled={!completed}><Download size={16} /> Download</button>
              </div>
            </div>

            {(transcript.trim() || systemRecording || liveRecording || summaryText.trim()) && <section className="question-panel card">
              <div className="section-label"><MessageCircle size={15} /><span>Ask about this transcript and summary</span></div>
              <p>Answers can reference the available transcript and its summary.</p>
              {questionMessages.length > 0 && <div className="question-history" aria-live="polite">{questionMessages.map((message, index) => <article className={`question-message ${message.role}`} key={index}><small>{message.role === "user" ? "You" : "Answer"}</small><MarkdownSummary text={message.text} /></article>)}{questionDraft && <article className="question-message assistant"><small>Answering…</small><MarkdownSummary text={questionDraft} /></article>}</div>}
              <form className="question-form" onSubmit={(event) => { event.preventDefault(); void askAboutTranscript(); }}>
                <input value={question} onChange={(event) => setQuestion(event.target.value)} placeholder="Ask about the transcript or summary…" maxLength={1200} disabled={questionBusy || !selectedQuestionModel} />
                <button type="submit" disabled={questionBusy || !question.trim() || (!transcriptRef.current.trim() && !summaryTextRef.current.trim()) || !selectedQuestionModel}>{questionBusy ? <Loader2 className="spin" size={15} /> : <ArrowRight size={16} />} Ask</button>
              </form>
              <small className="model-settings-hint">Question model: {selectedQuestionModel?.name ?? questionModel} · Change in Settings</small>
              {!selectedQuestionModel && <small className="translation-unavailable">Add a key with access to a supported text model to ask questions.</small>}
            </section>}

            {(busy || onlineExtracting || systemRecording || liveRecording || systemStopping || liveStopping || progress > 0) && (
              <div className="progress-panel">
                <div className="progress-copy"><span>{progressFinished ? <Check size={15} /> : <Loader2 className={(busy || onlineExtracting || systemRecording || liveRecording || systemStopping || liveStopping || onlineInspecting || webPageInspecting) ? "spin" : ""} size={15} />}{phase}</span><strong>{onlineDownloadLabel ?? (liveRecording ? formatDuration(liveElapsed) : `${Math.round(progress)}%`)}</strong></div>
                <div className={`progress-track ${onlineDownloadIndeterminate ? "indeterminate" : ""}`} style={progressStyle}><i /></div>
              </div>
            )}
            {technicalDetails && backgroundWorkActive && <section className="technical-activity-panel" aria-live="polite">
              <div className="technical-activity-heading"><strong>Technical details · {technicalTaskNamesBySlot[sourceMode]}</strong></div>
              {technicalLogsBySlot[sourceMode].length === 0 ? <p>Waiting for activity in this tab…</p> : <ol>{technicalLogsBySlot[sourceMode].map((entry, index) => <li key={`${entry.timestamp}-${index}`}><time>{new Date(entry.timestamp).toLocaleTimeString()}</time><b>{entry.category}</b><span>{entry.message}</span></li>)}</ol>}
            </section>}
            {busy && <button className="cancel-processing-button" onClick={() => void cancelTranscription()} disabled={transcriptionCancelRequested}>{transcriptionCancelRequested ? <><Loader2 className="spin" size={13} /> Canceling…</> : "Cancel processing"}</button>}

            {Object.keys(transcriptionPrompts).length > 0 && <div className="transcript-sections prompt-inspector">
              <details>
                <summary><span>Prompts actually sent to the transcription API</span><small>{Object.keys(transcriptionPrompts).length} parts</small></summary>
                {Object.entries(transcriptionPrompts).sort(([left], [right]) => Number(left) - Number(right)).map(([chunk, prompt]) => <div className="prompt-inspector-part" key={chunk}>
                  <strong>{Number(chunk) > 0 ? `Part ${chunk}` : "Prompt"}</strong>
                  <pre>{prompt ?? "No prompt field was sent for this part (the selected model does not use text prompts)."}</pre>
                </div>)}
              </details>
            </div>}

            {(sourceMode === "system" || completed) && (completed || busy || onlineExtracting || systemRecording || liveRecording || summaryText || notesText || Boolean(audio && audioOwner === sourceMode)) && (
              <details className="summary-panel" open>
                <summary className="summary-panel-heading">
                  <div><p className="eyebrow">MARKDOWN · STREAMING SUMMARY</p><h3>{systemRecording || liveRecording ? "Running bullet notes" : "Transcript summary"}</h3></div>
                  {summaryMode && <span className={`summary-status-pill ${summaryStatus}`}>{summaryStatus === "failed" ? <CircleAlert size={13} /> : summaryStatus === "ended" ? <Check size={13} /> : <Loader2 className="spin" size={13} />} {summaryStatus === "ended" ? "Summary ended" : summaryStatus === "failed" ? "Summary failed" : "Summary started"}</span>}
                </summary>
                <div className="summary-panel-content">
                {!summaryMode && <button className="summary-start-button" onClick={() => {
                    setSummaryMode(true);
                    summaryModeRef.current = true;
                    setSummaryStatus("started");
                    if (completed && transcriptRef.current) void generateFinalSummary(transcriptRef.current);
                  }} disabled={!selectedTranslationModel || summaryGenerating}>{summaryGenerating ? "Starting…" : "Start summary"}</button>}
                <p className="model-settings-hint">Summary model: {selectedTranslationModel?.name ?? translationModel} · Change in Settings</p>
                <div className="translation-cost cost-card summary-estimate">
                  <div className="cost-icon"><CircleDollarSign size={17} /></div>
                  <div className="cost-main">
                    <span>Estimated summary cost · {selectedTranslationModel?.name ?? translationModel}</span>
                    <strong>{formatUsd(summaryEstimate.total)} <small>USD</small></strong>
                    <p>Approx. {summaryEstimate.inputTokens.toLocaleString()} input + {summaryEstimate.outputTokens.toLocaleString()} output tokens</p>
                  </div>
                  {selectedTranslationModel && <div className="cost-rate"><small>RATE / 1M</small>${selectedTranslationModel.inputPricePerMillion} in · ${selectedTranslationModel.outputPricePerMillion} out</div>}
                  <p className="estimate-note">Character-based planning estimate; actual input and output usage may vary.</p>
                </div>
                {!selectedTranslationModel && <p className="translation-unavailable">Add a key with access to GPT-6 Luna or GPT-6.1 Sol to enable summaries.</p>}
                {summaryMode && completed && !summaryText && !summaryGenerating && <button className="summary-generate-button" onClick={() => void generateFinalSummary(transcriptRef.current)} disabled={!transcript.trim() || !selectedTranslationModel}>Generate summary from full transcript</button>}
                {(summaryText || summaryDraft) ? <MarkdownSummary text={summaryGenerating ? (summaryDraft || summaryText) : (summaryText || summaryDraft)} /> : <p className="summary-empty">Start summary to create running bullet notes during recording and a final summary when it ends.</p>}
                {summaryText && <div className="summary-translation-tools">
                  <div className="section-label"><Languages size={14} /><span>Translate summary</span></div>
                  <p className="model-settings-hint">Translation model: {selectedSummaryTranslationModel?.name ?? summaryTranslationModel} · Change in Settings</p>
                  <div className="translation-cost cost-card">
                    <div className="cost-icon"><CircleDollarSign size={17} /></div>
                    <div className="cost-main">
                      <span>Estimated summary translation cost{selectedSummaryTranslationModel ? ` · ${selectedSummaryTranslationModel.name}` : ""}</span>
                      <strong>{formatUsd(summaryTranslationEstimate.total)} <small>USD</small></strong>
                      <p>Approx. {summaryTranslationEstimate.inputTokens.toLocaleString()} input + {summaryTranslationEstimate.outputTokens.toLocaleString()} output tokens</p>
                    </div>
                    {selectedSummaryTranslationModel && <div className="cost-rate"><small>RATE / 1M</small>${selectedSummaryTranslationModel.inputPricePerMillion} in · ${selectedSummaryTranslationModel.outputPricePerMillion} out</div>}
                    <p className="estimate-note">Character-based planning estimate; actual usage may vary.</p>
                  </div>
                  <div className="translation-actions">
                    <select value={targetLanguage} onChange={(event) => setTargetLanguage(event.target.value)} disabled={summaryTranslating} aria-label="Summary target language">
                      {TRANSLATION_LANGUAGES.map((language) => <option key={language}>{language}</option>)}<option value="Other">Other language…</option>
                    </select>
                    {!summaryTranslations[`${requestedTargetLanguage} · ${selectedSummaryTranslationModel?.name ?? ""}`] && <button onClick={() => void translateSummary()} disabled={summaryTranslating || translating || !selectedSummaryTranslationModel || (targetLanguage === "Other" && !customLanguage.trim())}>{summaryTranslating ? <><Loader2 className="spin" size={13} /> Translating…</> : "Translate summary"}</button>}
                    <button className={summaryTranslationLive ? "selected" : ""} onClick={() => {
                      const enabled = !summaryTranslationLive;
                      setSummaryTranslationLive(enabled);
                      summaryTranslationLiveRef.current = enabled;
                      if (enabled && summaryText.trim() && selectedSummaryTranslationModel && requestedTargetLanguage) void translateSummary(true);
                    }} disabled={!selectedSummaryTranslationModel || (targetLanguage === "Other" && !customLanguage.trim())}>{summaryTranslationLive ? "Stop live translation" : "Translate as summary updates"}</button>
                  </div>
                  {targetLanguage === "Other" && <input className="custom-language" value={customLanguage} onChange={(event) => setCustomLanguage(event.target.value)} placeholder="Enter a language" maxLength={60} disabled={summaryTranslating} />}
                  {summaryTranslations[`${requestedTargetLanguage} · ${selectedSummaryTranslationModel?.name ?? ""}`] && <div className="summary-translated"><strong>{requestedTargetLanguage}</strong><MarkdownSummary text={summaryTranslations[`${requestedTargetLanguage} · ${selectedSummaryTranslationModel?.name ?? ""}`]} /></div>}
                </div>}
                {summaryGenerating && <small className="summary-status"><Loader2 className="spin" size={12} /> Updating Markdown…</small>}
                </div>
              </details>
            )}

            {(sourceMode === "system" || completed) && (completed || systemRecording || liveRecording || notesText || Object.keys(notesTranslations).length > 0 || Boolean(audio && audioOwner === sourceMode)) && <details className="summary-panel notes-panel" open>
              <summary className="summary-panel-heading"><div><p className="eyebrow">MARKDOWN · INCREMENTAL NOTES</p><h3>{notesStyle === "study" ? "Cornell study notes" : "Meeting minutes"}</h3></div>{notesMode ? <span className="summary-status-pill started"><Loader2 className="spin" size={13} /> Notes active</span> : notesText && completed ? <span className="summary-status-pill ended"><Check size={13} /> Notes ended</span> : null}</summary>
              <div className="summary-panel-content">
                {!notesMode && <>
                  <div className="notes-style-options" role="group" aria-label="Notes format">
                    <button className={notesStyle === "meeting" ? "selected" : ""} onClick={() => chooseNotesStyle("meeting")}>Meeting minutes</button>
                    <button className={notesStyle === "study" ? "selected" : ""} onClick={() => chooseNotesStyle("study")}>Cornell study notes</button>
                  </div>
                  <button className="summary-start-button" onClick={startNotes} disabled={!selectedTranslationModel}>Start {notesStyle === "study" ? "study notes" : "meeting minutes"}</button>
                </>}
                <p className="model-settings-hint">Notes model: {selectedTranslationModel?.name ?? translationModel} · Change in Settings</p>
                {notesText ? <MarkdownSummary text={notesText} /> : <p className="summary-empty">{notesStyle === "study" ? "Cornell-style notes preserve lecture detail and add key questions, a summary, and review questions." : "Meeting minutes keep discussion context, decisions, open questions, and action items."} Start notes after transcription is complete, or during recording.</p>}
                {notesText && <div className="summary-translation-tools notes-translation-tools">
                  <div className="section-label"><Languages size={14} /><span>Translate notes</span></div>
                  <p className="model-settings-hint">Translation model: {selectedTranscriptTranslationModel?.name ?? transcriptTranslationModel} · Change in Settings</p>
                  <div className="translation-actions">
                    <select value={targetLanguage} onChange={(event) => setTargetLanguage(event.target.value)} disabled={notesTranslating} aria-label="Notes target language">
                      {TRANSLATION_LANGUAGES.map((language) => <option key={language}>{language}</option>)}<option value="Other">Other language…</option>
                    </select>
                    <button onClick={() => void translateNotes()} disabled={notesTranslating || !selectedTranscriptTranslationModel || (targetLanguage === "Other" && !customLanguage.trim())}>{notesTranslating ? <><Loader2 className="spin" size={13} /> Translating…</> : notesTranslations[requestedTargetLanguage] ? "Translate again" : "Translate notes"}</button>
                  </div>
                  {targetLanguage === "Other" && <input className="custom-language" value={customLanguage} onChange={(event) => setCustomLanguage(event.target.value)} placeholder="Enter a language" maxLength={60} disabled={notesTranslating} />}
                  {notesTranslations[requestedTargetLanguage] && <div className="summary-translated"><strong>{requestedTargetLanguage}</strong><MarkdownSummary text={notesTranslations[requestedTargetLanguage]} /></div>}
                </div>}
              </div>
            </details>}

            {completed && (
              <details className="translation-panel translation-fold" open>
                <summary className="translation-option-heading">
                  <div className="translation-title"><Languages size={16} /><span><strong>Translate transcript</strong><small>OPTIONAL · STREAM = TRUE</small></span></div>
                </summary>
                <div className="translation-panel-content">
                {!selectedTranscriptTranslationModel && <p className="translation-unavailable">This API key returned neither GPT-6 Luna nor GPT-6.1 Sol, so translation is unavailable.</p>}
                {selectedTranscriptTranslationModel && <>
                  <p className="model-settings-hint">Translation model: {selectedTranscriptTranslationModel.name} · Change in Settings</p>
                  <div className="translation-actions">
                    <select value={targetLanguage} onChange={(event) => setTargetLanguage(event.target.value)} disabled={translating || summaryTranslating} aria-label="Target language">
                      {TRANSLATION_LANGUAGES.map((language) => <option key={language}>{language}</option>)}
                      <option value="Other">Other language…</option>
                    </select>
                    {!translations[targetTranslationKey] && <button onClick={translateTranscript} disabled={translating || summaryTranslating || (targetLanguage === "Other" && !customLanguage.trim())}>
                      {translating && <Loader2 className="spin" size={13} />}
                      {translating ? "Translating…" : "Translate"}
                    </button>}
                  </div>
                  {targetLanguage === "Other" && <input className="custom-language" value={customLanguage} onChange={(event) => setCustomLanguage(event.target.value)} placeholder="Enter a language, e.g. Swedish" maxLength={60} disabled={translating} />}
                  <div className="translation-cost cost-card">
                    <div className="cost-icon"><CircleDollarSign size={17} /></div>
                    <div className="cost-main">
                      <span>Estimated translation cost</span>
                      <strong>{formatUsd(translationEstimate.total)} <small>USD</small></strong>
                      <p>Approx. {translationEstimate.inputTokens.toLocaleString()} input + {translationEstimate.outputTokens.toLocaleString()} output tokens</p>
                    </div>
                    <div className="cost-rate"><small>RATE / 1M</small>${selectedTranscriptTranslationModel.inputPricePerMillion} in · ${selectedTranscriptTranslationModel.outputPricePerMillion} out</div>
                    <p className="estimate-note">Character-based planning estimate. Language, output length, and reasoning tokens can change actual billing.</p>
                  </div>
                </>}
                {transcriptVersions.length > 1 && <div className="transcript-version-picker"><small>Transcript version</small><div className="version-tabs">
                    {transcriptVersions.map((version) => (
                      <button key={version.language} className={activeLanguage === version.language ? "selected" : ""} onClick={() => selectTranscriptVersion(version.language)}>{version.language}</button>
                    ))}
                  </div></div>}
                </div>
              </details>
            )}

            <details className="transcript-fold" open>
              <summary>Transcript · {activeLanguage}</summary>
              <div className={`transcript-body ${displayedText ? "has-text" : ""}`}>
              {displayedText ? ((displayedTextStreaming || displayedSections.length <= 1) ? <p>{displayedText}<span className={displayedTextStreaming ? "cursor" : ""} /></p> : (
                <div className="transcript-sections">
                  {displayedSections.map((section, index) => (
                    <TranscriptSection key={`${activeLanguage}-${index}`} section={section} index={index} />
                  ))}
                </div>
              )) : (
                <div className="empty-state"><span><AudioLines size={25} /></span><strong>Your transcript will appear here</strong><p>Watch the text arrive live while your audio is processed.</p></div>
              )}
              </div>
            </details>
            <footer><span><span className="secure-dot" /> API keys protected by Keychain</span>{displayedText && <span>{activeLanguage} · {displayedText.length.toLocaleString()} characters</span>}</footer>
          </section>
          </section>
        </>}

        {error && <div className="error-toast"><CircleAlert size={18} /><span>{error}</span><button onClick={() => setError("")}><X size={16} /></button></div>}
      </main>

      <SettingsModal open={settingsOpen} keys={apiKeys} adminKeys={adminApiKeys} onClose={() => setSettingsOpen(false)} onChanged={refreshSettingsKeys} parallelRequests={parallelRequests} onParallelRequestsChange={setParallelRequests} captureQuality={captureQuality} onCaptureQualityChange={setCaptureQuality} availableCaptureQualities={availableCaptureQualities} captureQualityRestrictionNote={captureQualityRestrictionNote} compactEnabled={compactEnabled} onCompactEnabledChange={setCompactPreference} compactChangeDeferred={compactChangeDeferred} summaryModelLocked={summaryModelLocked} technicalErrors={technicalErrors} onTechnicalErrorsChange={setTechnicalErrors} technicalDetails={technicalDetails} onTechnicalDetailsChange={setTechnicalDetails} headlessBrowser={headlessBrowser} onHeadlessBrowserChange={setHeadlessBrowser} translationModels={availableTranslationModels} summaryModel={translationModel} onSummaryModelChange={setTranslationModel} transcriptTranslationModel={transcriptTranslationModel} onTranscriptTranslationModelChange={setTranscriptTranslationModel} summaryTranslationModel={summaryTranslationModel} onSummaryTranslationModelChange={setSummaryTranslationModel} questionModel={questionModel} onQuestionModelChange={setQuestionModel} />
      <LibraryModal
        open={libraryOpen}
        files={libraryFiles}
        transcripts={savedTranscripts}
        deletionDisabled={transcriptionBusyBySlot.file || transcriptionBusyBySlot.online || systemRecording || liveRecording || systemStopping || liveStopping}
        onClose={() => setLibraryOpen(false)}
        onOpenFile={(file) => {
          if (isLocalTaskBusy()) { setError("Local file is busy with another task. Wait for it to finish before opening a Library item."); return; }
          setLibraryOpen(false); switchSourceMode("file");
          void inspectAudio(file.path).then((ready) => { if (ready) updateContextPrompt("file", file.contextPrompt ?? ""); });
        }}
        onOpenTranscript={(item) => {
          if (isLocalTaskBusy()) { setError("Local file is busy with another task. Wait for it to finish before opening a Library transcript."); return; }
          setLibraryOpen(false); switchSourceMode("file"); openSavedTranscript(item);
        }}
        onRevealPath={(path) => { void invoke("reveal_library_path", { path }).catch((cause) => setError(errorMessage(cause))); }}
        onDeleteFile={(file) => setPendingDelete({ kind: "media", id: file.path, name: file.name, isReference: file.isReference })}
        onDeleteTranscript={(item) => setPendingDelete({ kind: "transcript", id: item.id, name: item.name })}
      />
      <UsageModal open={usageOpen} costs={projectCosts} loading={usageLoading} error={usageError} onRefresh={() => void refreshUsage()} onClose={() => setUsageOpen(false)} />
      <DownloadModal open={downloadOpen} versions={transcriptVersions} selectedLanguage={downloadLanguage} onLanguageChange={setDownloadLanguage} onClose={() => setDownloadOpen(false)} onConfirm={downloadTranscript} />
      <ConfirmationModal
        open={conversionOpen}
        title="Audio conversion is required"
        description="This audio format needs re-encoding before transcription."
        details="Conversion can take extra time and may reduce audio quality. Do you want to continue?"
        confirmLabel="Convert"
        onCancel={() => { setConversionOpen(false); setPendingConversion(null); setError("[CONVERSION_CANCELLED] Audio conversion was canceled by the user."); }}
        onConfirm={() => {
          setConversionOpen(false);
          const pending = pendingConversion;
          setPendingConversion(null);
          if (pending === "online") void extractOnlineAudio(true);
          else void runTranscription(true);
        }}
      />
      <ConfirmationModal
        open={largeDownloadOpen}
        title="Confirm media download"
        description="The selected stream may use a substantial amount of network data, especially for long recordings or HLS playlists."
        details="Minefia downloads only the selected stream, extracts audio into app storage, and removes temporary media when finished. You can cancel while it runs."
        confirmLabel="Download selected audio"
        icon="download"
        onCancel={() => setLargeDownloadOpen(false)}
        onConfirm={() => { setLargeDownloadOpen(false); void extractOnlineAudio(onlineConversionApproved, true); }}
      />
      <ConfirmationModal
        open={pendingDelete !== null}
        title={pendingDelete?.isReference ? "Delete original file?" : "Delete saved item?"}
        description={pendingDelete?.name ?? "This item"}
        details={pendingDelete?.isReference ? "This permanently deletes the original file from your computer and removes its Library entry. This cannot be undone." : "This permanently removes the item from Minefia Audio storage. This cannot be undone."}
        confirmLabel={pendingDelete?.isReference ? "Delete original" : "Delete"}
        onCancel={() => setPendingDelete(null)}
        onConfirm={() => void deleteLibraryItem()}
      />
    </div>
  );
}
