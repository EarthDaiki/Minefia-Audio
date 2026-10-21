import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Check, Eye, EyeOff, Loader2, Plus, Settings, Trash2, X } from "lucide-react";
import type { ApiKeySummary, TranslationModel } from "../types";
import { errorMessage } from "../lib/errors";

type Props = {
  open: boolean;
  keys: ApiKeySummary[];
  onClose: () => void;
  onChanged: () => Promise<void>;
  parallelRequests: number;
  onParallelRequestsChange: (value: number) => void;
  technicalErrors: boolean;
  onTechnicalErrorsChange: (value: boolean) => void;
  technicalDetails: boolean;
  onTechnicalDetailsChange: (value: boolean) => void;
  headlessBrowser: boolean;
  onHeadlessBrowserChange: (value: boolean) => void;
  translationModels: TranslationModel[];
  summaryModel: string;
  onSummaryModelChange: (value: string) => void;
  transcriptTranslationModel: string;
  onTranscriptTranslationModelChange: (value: string) => void;
  summaryTranslationModel: string;
  onSummaryTranslationModelChange: (value: string) => void;
  questionModel: string;
  onQuestionModelChange: (value: string) => void;
};

export function SettingsModal({ open, keys, onClose, onChanged, parallelRequests, onParallelRequestsChange, technicalErrors, onTechnicalErrorsChange, technicalDetails, onTechnicalDetailsChange, headlessBrowser, onHeadlessBrowserChange, translationModels, summaryModel, onSummaryModelChange, transcriptTranslationModel, onTranscriptTranslationModelChange, summaryTranslationModel, onSummaryTranslationModelChange, questionModel, onQuestionModelChange }: Props) {
  const [label, setLabel] = useState("");
  const [key, setKey] = useState("");
  const [show, setShow] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    if (open) {
      setLabel("");
      setKey("");
      setError("");
      setShow(false);
    }
  }, [open]);

  if (!open) return null;

  async function saveKey() {
    if (!label.trim() || !key.trim()) {
      setError("Enter a name and API key.");
      return;
    }
    setSaving(true);
    setError("");
    try {
      await invoke("save_api_key", {
        label: label.trim(),
        apiKey: key.trim(),
      });
      setLabel("");
      setKey("");
      await onChanged();
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setSaving(false);
    }
  }

  async function selectKey(id: string) {
    try {
      await invoke("select_api_key", { id });
      await onChanged();
    } catch (cause) {
      setError(errorMessage(cause));
    }
  }

  async function deleteKey(id: string) {
    try {
      await invoke("delete_api_key", { id });
      await onChanged();
    } catch (cause) {
      setError(errorMessage(cause));
    }
  }

  return (
    <div className="modal-backdrop" onMouseDown={onClose}>
      <section className="modal keys-modal" onMouseDown={(event) => event.stopPropagation()} aria-modal="true" role="dialog" aria-labelledby="settings-title">
        <header className="settings-header">
          <button className="icon-button modal-close" onClick={onClose} aria-label="Close settings"><X size={18} /></button>
          <div className="modal-icon"><Settings size={22} /></div>
          <p className="eyebrow">MINEFIA AUDIO</p>
          <h2 id="settings-title">Settings</h2>
          <p className="modal-copy">Manage models, API access, and app behavior.</p>
        </header>
        <div className="settings-content">
        <section className="settings-section">
          <h3>Text models</h3>
          <p className="validation-note">Choose a model independently for each task. Audio transcription models are selected on the main screen.</p>
          {[{
            label: "Summary", value: summaryModel, change: onSummaryModelChange,
          }, {
            label: "Transcript translation", value: transcriptTranslationModel, change: onTranscriptTranslationModelChange,
          }, {
            label: "Summary translation", value: summaryTranslationModel, change: onSummaryTranslationModelChange,
          }, {
            label: "Transcript Q&A", value: questionModel, change: onQuestionModelChange,
          }].map((field) => {
            const model = translationModels.find((item) => item.id === field.value);
            return <label className="settings-model-row" key={field.label}>
              <span><strong>{field.label}</strong><small>{model?.description ?? "No supported model available for this API key."}</small></span>
              <select value={field.value} onChange={(event) => field.change(event.target.value)} disabled={translationModels.length === 0} aria-label={`${field.label} model`}>
                {translationModels.map((item) => <option value={item.id} key={item.id}>{item.name}</option>)}
              </select>
            </label>;
          })}
        </section>

        <section className="settings-section">
          <h3>API access</h3>
          <p className="validation-note">API keys are kept in your system Keychain. Choose the key Minefia Audio should use.</p>

        {keys.length > 0 && (
          <div className="key-list">
            {keys.map((item) => (
              <div className={`saved-key ${item.isSelected ? "selected" : ""}`} key={item.id}>
                <button className="key-select" onClick={() => selectKey(item.id)}>
                  <span className="radio-mark">{item.isSelected && <Check size={12} />}</span>
                  <span><strong>{item.label}</strong><small>{item.masked}</small></span>
                  {item.isSelected && <em>Active</em>}
                </button>
                <button className="delete-key" onClick={() => deleteKey(item.id)} aria-label={`Delete ${item.label}`}><Trash2 size={15} /></button>
              </div>
            ))}
          </div>
        )}

        </section>

        <section className="settings-section">
          <h3>Processing</h3>
          <label className="field-label" htmlFor="parallel-requests">Maximum concurrent transcription requests</label>
          <select className="text-field" id="parallel-requests" value={parallelRequests} onChange={(event) => onParallelRequestsChange(Number(event.target.value))}>
            {[1, 2, 3, 4].map((value) => <option key={value} value={value}>{value}{value === 2 ? " · default" : ""}</option>)}
          </select>
          <p className="validation-note">Applies to file uploads and Standard Mode. Live Mode streams through one realtime session.</p>
        </section>

        <section className="settings-section">
          <h3>App behavior</h3>
        <div className="settings-error-option">
          <div>
            <strong>Technical error details</strong>
            <p className="validation-note">Show raw provider and network errors instead of simplified messages. Useful for troubleshooting.</p>
          </div>
          <button className={`toggle ${technicalErrors ? "on" : ""}`} role="switch" aria-checked={technicalErrors} aria-label="Technical error details" onClick={() => onTechnicalErrorsChange(!technicalErrors)}><i /></button>
        </div>

        <div className="settings-error-option">
          <div>
            <strong>Technical details</strong>
            <p className="validation-note">Show the current task’s live log during downloads and other background work. Each task is saved in its own named log file in app storage.</p>
          </div>
          <button className={`toggle ${technicalDetails ? "on" : ""}`} role="switch" aria-checked={technicalDetails} aria-label="Technical details and download log" onClick={() => onTechnicalDetailsChange(!technicalDetails)}><i /></button>
        </div>
        <div className="settings-error-option">
          <div>
            <strong>Headless browser</strong>
            <p className="validation-note">Run page inspection in the background. If the page redirects to a login URL, Minefia Audio opens it visibly so you can sign in.</p>
          </div>
          <button className={`toggle ${headlessBrowser ? "on" : ""}`} role="switch" aria-checked={headlessBrowser} aria-label="Headless browser" onClick={() => onHeadlessBrowserChange(!headlessBrowser)}><i /></button>
        </div>

        </section>

        <section className="settings-section">
          <h3>Add API key</h3>
          <p className="validation-note">The key is checked with OpenAI before it is stored.</p>

        <label className="field-label" htmlFor="key-label">Key name</label>
        <input className="text-field" id="key-label" value={label} onChange={(event) => setLabel(event.target.value)} placeholder="Personal, Work, Client…" />
        <label className="field-label second-label" htmlFor="api-key">API key</label>
        <div className="secret-field">
          <input id="api-key" type={show ? "text" : "password"} value={key} onChange={(event) => setKey(event.target.value)} placeholder="sk-..." autoComplete="off" onKeyDown={(event) => event.key === "Enter" && saveKey()} />
          <button onClick={() => setShow((value) => !value)} aria-label="Toggle key visibility">{show ? <EyeOff size={18} /> : <Eye size={18} />}</button>
        </div>
        <p className="validation-note">The key will be validated with <strong>GET /v1/models</strong> before it is saved.</p>
        {error && <p className="form-error">{error}</p>}
        <button className="primary-button modal-submit" onClick={saveKey} disabled={saving}>
          {saving ? <Loader2 className="spin" size={17} /> : <Plus size={17} />}
          {saving ? "Validating key…" : "Validate & save key"}
        </button>
        <p className="service-note">Keychain service: <strong>Minefia-Audio</strong></p>
        </section>
        </div>
      </section>
    </div>
  );
}
