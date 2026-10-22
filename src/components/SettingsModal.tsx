import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Check, Eye, EyeOff, Loader2, Plus, Settings, Trash2, X, FolderPlus, List } from "lucide-react";
import type { AdminApiKeySummary, ApiKeySummary, CreatedProjectKey, MinefiaProject, OpenAIProjectSummary, TranslationModel } from "../types";
import { errorMessage } from "../lib/errors";
import { rememberOpenAIKeyId } from "../lib/keyMetadata";

type Props = {
  open: boolean;
  keys: ApiKeySummary[];
  adminKeys: AdminApiKeySummary[];
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

export function SettingsModal({ open, keys, adminKeys, onClose, onChanged, parallelRequests, onParallelRequestsChange, technicalErrors, onTechnicalErrorsChange, technicalDetails, onTechnicalDetailsChange, headlessBrowser, onHeadlessBrowserChange, translationModels, summaryModel, onSummaryModelChange, transcriptTranslationModel, onTranscriptTranslationModelChange, summaryTranslationModel, onSummaryTranslationModelChange, questionModel, onQuestionModelChange }: Props) {
  const [tab, setTab] = useState<"keys" | "models" | "general">("keys");
  const [label, setLabel] = useState("");
  const [key, setKey] = useState("");
  const [adminLabel, setAdminLabel] = useState("");
  const [adminKey, setAdminKey] = useState("");
  const [show, setShow] = useState(false);
  const [showAdmin, setShowAdmin] = useState(false);
  const [saving, setSaving] = useState(false);
  const [savingAdmin, setSavingAdmin] = useState(false);
  const [error, setError] = useState("");
  const [adminError, setAdminError] = useState("");
  const [projectValidationVisible, setProjectValidationVisible] = useState(false);
  const [adminValidationVisible, setAdminValidationVisible] = useState(false);
  const [appProject, setAppProject] = useState<MinefiaProject>({ projectId: null, projectName: null, createdByApp: false, serviceAccountId: null, serviceAccountName: null, apiKeyId: null });
  const [appProjectName, setAppProjectName] = useState("Minefia Audio");
  const [serviceAccountName, setServiceAccountName] = useState("");
  const [creatingAppProject, setCreatingAppProject] = useState(false);
  const [projectMode, setProjectMode] = useState<"create" | "select">("create");
  const [openAiProjects, setOpenAiProjects] = useState<OpenAIProjectSummary[]>([]);
  const [selectedOpenAiProject, setSelectedOpenAiProject] = useState<OpenAIProjectSummary | null>(null);
  const [loadingProjects, setLoadingProjects] = useState(false);

  useEffect(() => {
    if (open) {
      setLabel(""); setKey(""); setAdminLabel(""); setAdminKey("");
      setError(""); setAdminError(""); setShow(false); setShowAdmin(false);
      setProjectValidationVisible(false); setAdminValidationVisible(false);
      void invoke<MinefiaProject>("get_minefia_project").then(setAppProject).catch(() => setAppProject({ projectId: null, projectName: null, createdByApp: false, serviceAccountId: null, serviceAccountName: null, apiKeyId: null }));
    }
  }, [open]);

  useEffect(() => {
    if (!open || projectMode !== "select" || !adminKeys.length) return;
    let active = true;
    setLoadingProjects(true);
    setAdminError("");
    invoke<OpenAIProjectSummary[]>("list_openai_projects")
      .then((items) => { if (active) setOpenAiProjects(items); })
      .catch((cause) => { if (active) setAdminError(errorMessage(cause)); })
      .finally(() => { if (active) setLoadingProjects(false); });
    return () => { active = false; };
  }, [open, projectMode, adminKeys.length]);

  if (!open) return null;

  async function saveKey() {
    if (!label.trim() || !key.trim()) {
      setProjectValidationVisible(true);
      setError(projectMissingMessage());
      return;
    }
    setSaving(true); setError("");
    try {
      await invoke("save_api_key", { label: label.trim(), apiKey: key.trim() });
      setLabel(""); setKey(""); await onChanged();
    } catch (cause) { setError(errorMessage(cause)); }
    finally { setSaving(false); }
  }

  async function saveAdminKey() {
    if (!adminLabel.trim() || !adminKey.trim()) {
      setAdminValidationVisible(true);
      setAdminError(adminMissingMessage());
      return;
    }
    setSavingAdmin(true); setAdminError("");
    try {
      await invoke("save_admin_api_key", { label: adminLabel.trim(), apiKey: adminKey.trim() });
      setAdminLabel(""); setAdminKey(""); await onChanged();
    } catch (cause) { setAdminError(errorMessage(cause)); }
    finally { setSavingAdmin(false); }
  }

  async function createAppProject() {
    if (!serviceAccountName.trim()) return;
    if (!appProject.projectId && projectMode === "create" && !appProjectName.trim()) return;
    if (!appProject.projectId && projectMode === "select" && !selectedOpenAiProject) return;
    setCreatingAppProject(true); setAdminError("");
    try {
      if (!appProject.projectId && projectMode === "select" && selectedOpenAiProject) {
        await invoke("select_minefia_project", { projectId: selectedOpenAiProject.id, projectName: selectedOpenAiProject.name });
      }
      const result = await invoke<CreatedProjectKey>("create_minefia_project_key", { projectName: appProjectName.trim(), serviceAccountName: serviceAccountName.trim() });
      rememberOpenAIKeyId(result.key.openaiKeyId);
      setAppProject(result.project);
      await onChanged();
    } catch (cause) { setAdminError(errorMessage(cause)); }
    finally { setCreatingAppProject(false); }
  }

  function projectMissingMessage() {
    if (!label.trim() && !key.trim()) return "Enter a Keychain item name and Project API key.";
    if (!label.trim()) return "Enter a Keychain item name.";
    return "Enter a Project API key.";
  }

  function adminMissingMessage() {
    if (!adminLabel.trim() && !adminKey.trim()) return "Enter a Keychain item name and Admin API key.";
    if (!adminLabel.trim()) return "Enter a Keychain item name.";
    return "Enter an Admin API key.";
  }

  async function selectKey(id: string, admin = false) {
    try { await invoke(admin ? "select_admin_api_key" : "select_api_key", { id }); await onChanged(); }
    catch (cause) { (admin ? setAdminError : setError)(errorMessage(cause)); }
  }

  async function deleteKey(id: string, admin = false) {
    try { await invoke(admin ? "delete_admin_api_key" : "delete_api_key", { id }); await onChanged(); }
    catch (cause) { (admin ? setAdminError : setError)(errorMessage(cause)); }
  }

  const modelFields = [
    { label: "Summary", value: summaryModel, change: onSummaryModelChange },
    { label: "Transcript translation", value: transcriptTranslationModel, change: onTranscriptTranslationModelChange },
    { label: "Summary translation", value: summaryTranslationModel, change: onSummaryTranslationModelChange },
    { label: "Transcript Q&A", value: questionModel, change: onQuestionModelChange },
  ];

  return (
    <div className="modal-backdrop" onMouseDown={onClose}>
      <section className="modal keys-modal" onMouseDown={(event) => event.stopPropagation()} aria-modal="true" role="dialog" aria-labelledby="settings-title">
        <header className="settings-header">
          <button className="icon-button modal-close" onClick={onClose} aria-label="Close settings"><X size={18} /></button>
          <div className="modal-icon"><Settings size={22} /></div>
          <p className="eyebrow">MINEFIA AUDIO</p>
          <h2 id="settings-title">Settings</h2>
          <p className="modal-copy">Manage API access, models, and app behavior.</p>
        </header>
        <nav className="settings-tabs" aria-label="Settings sections">
          <button className={tab === "keys" ? "selected" : ""} onClick={() => setTab("keys")}>API keys</button>
          <button className={tab === "models" ? "selected" : ""} onClick={() => setTab("models")}>Models</button>
          <button className={tab === "general" ? "selected" : ""} onClick={() => setTab("general")}>General</button>
        </nav>
        <div className="settings-content">
          {tab === "models" && <section className="settings-section">
            <h3>Text models</h3>
            <p className="validation-note">Choose a model independently for each task. Audio transcription models are selected on the main screen.</p>
            {modelFields.map((field) => {
              const model = translationModels.find((item) => item.id === field.value);
              return <label className="settings-model-row" key={field.label}>
                <span><strong>{field.label}</strong><small>{model?.description ?? "No supported model available for this API key."}</small></span>
                <select value={field.value} onChange={(event) => field.change(event.target.value)} disabled={translationModels.length === 0} aria-label={`${field.label} model`}>
                  {translationModels.map((item) => <option value={item.id} key={item.id}>{item.name}</option>)}
                </select>
              </label>;
            })}
          </section>}

          {tab === "keys" && <>
            <section className="settings-section">
              <h3>Project API keys</h3>
              <p className="validation-note">Used for transcription and model requests. These keys are separate from Admin keys and stored in the system Keychain.</p>
              <div className="key-list">
                {keys.map((item) => <div className={`saved-key ${item.isSelected ? "selected" : ""}`} key={item.id}>
                  <button className="key-select" onClick={() => void selectKey(item.id)}>
                    <span className="radio-mark">{item.isSelected && <Check size={12} />}</span>
                    <span><strong>{item.label}</strong><small>{item.masked}</small></span>
                    {item.isSelected && <em>Default for app</em>}
                  </button>
                  <button className="delete-key" onClick={() => void deleteKey(item.id)} aria-label={`Delete ${item.label}`}><Trash2 size={15} /></button>
                </div>)}
              </div>
              <h3 className="settings-subheading">Add Project API key</h3>
              <label className="field-label" htmlFor="key-label">Keychain item name</label>
              <input className="text-field" id="key-label" value={label} onChange={(event) => { setLabel(event.target.value); setError(""); }} placeholder="Personal, Work, Client…" />
              <label className="field-label second-label" htmlFor="api-key">Project API key</label>
              <div className="secret-field">
                <input id="api-key" type={show ? "text" : "password"} value={key} onChange={(event) => { setKey(event.target.value); setError(""); }} placeholder="sk-…" autoComplete="off" onKeyDown={(event) => event.key === "Enter" && void saveKey()} />
                <button onClick={() => setShow((value) => !value)} aria-label="Toggle key visibility">{show ? <EyeOff size={18} /> : <Eye size={18} />}</button>
              </div>
              <p className="validation-note">Validated with <strong>GET /v1/models</strong> before saving. Keychain service: <strong>Minefia-Audio</strong>.</p>
              {(error || (projectValidationVisible && (!label.trim() || !key.trim()) ? projectMissingMessage() : "")) && <p className="form-error">{error || projectMissingMessage()}</p>}
              <button className="primary-button modal-submit" onClick={() => void saveKey()} disabled={saving || !label.trim() || !key.trim()}>{saving ? <Loader2 className="spin" size={17} /> : <Plus size={17} />}{saving ? "Validating key…" : "Validate & save Project key"}</button>
            </section>

            <section className="settings-section admin-key-section">
              <h3>Admin API keys · Usage costs</h3>
              <p className="validation-note">Used only for the organization Costs API. Add an Admin API key with Usage read access; a Project key cannot replace it. Stored separately in the system Keychain service <strong>Minefia-Audio</strong>.</p>
              <div className="key-list">
                {adminKeys.map((item) => <div className={`saved-key ${item.isSelected ? "selected" : ""}`} key={item.id}>
                  <button className="key-select" onClick={() => void selectKey(item.id, true)}>
                    <span className="radio-mark">{item.isSelected && <Check size={12} />}</span>
                    <span><strong>{item.label}</strong><small>{item.masked}</small></span>
                    {item.isSelected && <em>Default for Usage</em>}
                  </button>
                  <button className="delete-key" onClick={() => void deleteKey(item.id, true)} aria-label={`Delete Admin key ${item.label}`}><Trash2 size={15} /></button>
                </div>)}
              </div>
              <h3 className="settings-subheading">Add Admin API key</h3>
              <label className="field-label" htmlFor="admin-key-label">Keychain item name</label>
              <input className="text-field" id="admin-key-label" value={adminLabel} onChange={(event) => { setAdminLabel(event.target.value); setAdminError(""); }} placeholder="Usage admin, Finance…" />
              <label className="field-label second-label" htmlFor="admin-api-key">Admin API key</label>
              <div className="secret-field">
                <input id="admin-api-key" type={showAdmin ? "text" : "password"} value={adminKey} onChange={(event) => { setAdminKey(event.target.value); setAdminError(""); }} placeholder="sk-admin-…" autoComplete="off" onKeyDown={(event) => event.key === "Enter" && void saveAdminKey()} />
                <button onClick={() => setShowAdmin((value) => !value)} aria-label="Toggle Admin key visibility">{showAdmin ? <EyeOff size={18} /> : <Eye size={18} />}</button>
              </div>
              <p className="validation-note">Checked with <strong>GET /v1/organization/admin_api_keys?limit=1</strong> before saving. A failed access check is not saved. Costs are fetched only when you refresh Usage.</p>
              {(adminError || (adminValidationVisible && (!adminLabel.trim() || !adminKey.trim()) ? adminMissingMessage() : "")) && <p className="form-error">{adminError || adminMissingMessage()}</p>}
              <button className="primary-button modal-submit" onClick={() => void saveAdminKey()} disabled={savingAdmin || !adminLabel.trim() || !adminKey.trim()}>{savingAdmin ? <Loader2 className="spin" size={17} /> : <Plus size={17} />}{savingAdmin ? "Checking Costs API access…" : "Validate & save Admin key"}</button>

            {adminKeys.length > 0 && <>
              <h3 className="settings-subheading">Minefia Audio dedicated Project</h3>
              {appProject.projectId ? <div className="app-project-status">
                <strong>{appProject.projectName || "Minefia Audio"}</strong>
                <span>{appProject.createdByApp ? "Project created" : "Existing Project selected"} · {appProject.projectId}</span>
                {appProject.serviceAccountName && <small>Service Account: {appProject.serviceAccountName} · its API key is stored in Keychain.</small>}
                {!appProject.serviceAccountId && <p className="validation-note">The Project exists. Enter a Service Account name below to finish creating its Project API key.</p>}
              </div> : <>
                <p className="validation-note">Recommended: create one Project dedicated to this app. You can also select an existing Project. OpenAI Projects cannot be deleted.</p>
                <div className="setup-choice-tabs">
                  <button className={projectMode === "create" ? "selected" : ""} onClick={() => setProjectMode("create")}><FolderPlus size={15} /> Create new Project <small>Recommended</small></button>
                  <button className={projectMode === "select" ? "selected" : ""} onClick={() => { setSelectedOpenAiProject(null); setProjectMode("select"); }}><List size={15} /> Select existing Project</button>
                </div>
                {projectMode === "select" ? <>
                  {loadingProjects ? <div className="project-list-loading"><Loader2 className="spin" size={17} /> Loading organization Projects…</div> : <div className="setup-project-list">
                    {openAiProjects.map((project) => <button key={project.id} className={`setup-project-option ${selectedOpenAiProject?.id === project.id ? "selected" : ""}`} onClick={() => setSelectedOpenAiProject(project)}>
                      <span className="radio-mark">{selectedOpenAiProject?.id === project.id && <Check size={12} />}</span><span><strong>{project.name}</strong><small>{project.id}</small></span>
                    </button>)}
                    {openAiProjects.length === 0 && !adminError && <p className="validation-note">No active Projects were returned for this Admin key.</p>}
                  </div>}
                </> : <><label className="field-label" htmlFor="app-project-name">Project name</label><input className="text-field" id="app-project-name" value={appProjectName} onChange={(event) => setAppProjectName(event.target.value)} placeholder="Minefia Audio" /></>}
              </>}
              {!appProject.serviceAccountId && <>
                {appProject.projectId && <p className="validation-note">Create a Service Account in this Project to enable Usage and securely register its Project API key.</p>}
                <label className="field-label second-label" htmlFor="app-service-account-name">Service Account name / Keychain item name</label>
                <input className="text-field" id="app-service-account-name" value={serviceAccountName} onChange={(event) => setServiceAccountName(event.target.value)} placeholder="Minefia Audio" />
                <button className="primary-button modal-submit" onClick={() => void createAppProject()} disabled={creatingAppProject || loadingProjects || !serviceAccountName.trim() || (!appProject.projectId && projectMode === "create" && !appProjectName.trim()) || (!appProject.projectId && projectMode === "select" && !selectedOpenAiProject)}>{creatingAppProject ? <Loader2 className="spin" size={17} /> : <Plus size={17} />}{creatingAppProject ? "Creating Project key…" : appProject.projectId ? "Create Service Account key" : projectMode === "select" ? "Create Service Account and add key" : "Create Project and Service Account key"}</button>
              </>}
            </>}
            </section>
          </>}

          {tab === "general" && <>
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
              <div className="settings-error-option"><div><strong>Technical error details</strong><p className="validation-note">Show raw provider and network errors instead of simplified messages.</p></div><button className={`toggle ${technicalErrors ? "on" : ""}`} role="switch" aria-checked={technicalErrors} aria-label="Technical error details" onClick={() => onTechnicalErrorsChange(!technicalErrors)}><i /></button></div>
              <div className="settings-error-option"><div><strong>Technical details</strong><p className="validation-note">Show the current task’s live log during downloads and other background work.</p></div><button className={`toggle ${technicalDetails ? "on" : ""}`} role="switch" aria-checked={technicalDetails} aria-label="Technical details" onClick={() => onTechnicalDetailsChange(!technicalDetails)}><i /></button></div>
              <div className="settings-error-option"><div><strong>Headless browser</strong><p className="validation-note">Run page inspection in the background. Login pages open visibly so you can sign in.</p></div><button className={`toggle ${headlessBrowser ? "on" : ""}`} role="switch" aria-checked={headlessBrowser} aria-label="Headless browser" onClick={() => onHeadlessBrowserChange(!headlessBrowser)}><i /></button></div>
            </section>
          </>}
        </div>
      </section>
    </div>
  );
}
