import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { CircleAlert, Eye, EyeOff, KeyRound, Loader2, RefreshCw, ShieldCheck, FolderPlus, List, Check } from "lucide-react";
import { errorMessage } from "../lib/errors";
import type { AdminApiKeySummary, CreatedProjectKey, OpenAIProjectSummary } from "../types";
import { rememberOpenAIKeyId } from "../lib/keyMetadata";

type Props = {
  state: "loading" | "missing" | "error";
  keyLabel?: string;
  error?: string;
  adminKeys: AdminApiKeySummary[];
  onOpenSettings: () => void;
  onRetry: () => Promise<void>;
};

export function ApiAccessGate({ state, keyLabel, error, adminKeys, onOpenSettings, onRetry }: Props) {
  const [step, setStep] = useState<"admin" | "project">(adminKeys.length ? "project" : "admin");
  const [choice, setChoice] = useState<"create" | "select" | "existing">("create");
  const [itemName, setItemName] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [adminName, setAdminName] = useState("");
  const [adminKey, setAdminKey] = useState("");
  const [projectName, setProjectName] = useState("Minefia Audio");
  const [serviceAccountName, setServiceAccountName] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [showAdminKey, setShowAdminKey] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState("");
  const [projects, setProjects] = useState<OpenAIProjectSummary[]>([]);
  const [selectedProject, setSelectedProject] = useState<OpenAIProjectSummary | null>(null);
  const [projectsLoading, setProjectsLoading] = useState(false);

  useEffect(() => {
    if (adminKeys.length > 0) setStep("project");
  }, [adminKeys.length]);

  useEffect(() => {
    if (step !== "project" || choice !== "select" || adminKeys.length === 0) return;
    let active = true;
    setProjectsLoading(true);
    setSaveError("");
    invoke<OpenAIProjectSummary[]>("list_openai_projects")
      .then((items) => { if (active) setProjects(items); })
      .catch((cause) => { if (active) setSaveError(errorMessage(cause)); })
      .finally(() => { if (active) setProjectsLoading(false); });
    return () => { active = false; };
  }, [step, choice, adminKeys.length]);

  if (state === "loading") {
    return (
      <section className="access-card access-loading" aria-live="polite">
        <span className="access-icon"><Loader2 className="spin" size={28} /></span>
        <h1>Checking your setup…</h1>
        <p>Loading the API key and its available transcription models.</p>
      </section>
    );
  }

  if (state === "missing") {
    return (
      <div className="modal-backdrop initial-key-backdrop">
        <section className="modal initial-key-modal" role="dialog" aria-modal="true" aria-labelledby="initial-key-title">
          <div className="modal-icon"><KeyRound size={22} /></div>
          <p className="eyebrow">SECURE SETUP</p>
          <h2 id="initial-key-title">Set up API access</h2>
          <p className="modal-copy">A Project API key is needed to use Minefia Audio. Admin access is optional and only enables Usage costs and creating a dedicated app project.</p>

          {step === "admin" ? <>
            <h3 className="settings-subheading">Optional: add an Admin API key first</h3>
            <p className="validation-note">With an Admin key, we recommend creating a Project just for Minefia Audio. If you already have a dedicated Project, you can enter its Project API key instead.</p>
            <label className="field-label" htmlFor="initial-admin-name">Keychain item name</label>
            <input className="text-field" id="initial-admin-name" value={adminName} onChange={(event) => setAdminName(event.target.value)} placeholder="Organization admin" />
            <label className="field-label second-label" htmlFor="initial-admin-key">Admin API key</label>
            <div className="secret-field">
              <input id="initial-admin-key" type={showAdminKey ? "text" : "password"} value={adminKey} onChange={(event) => setAdminKey(event.target.value)} placeholder="sk-admin-…" autoComplete="off" onKeyDown={(event) => event.key === "Enter" && void saveAdmin()} />
              <button onClick={() => setShowAdminKey((value) => !value)} aria-label="Toggle Admin key visibility">{showAdminKey ? <EyeOff size={18} /> : <Eye size={18} />}</button>
            </div>
            <p className="validation-note">Validated with <strong>GET /v1/organization/admin_api_keys</strong>. It is stored in Keychain service <strong>Minefia-Audio</strong>.</p>
            <button className="primary-button modal-submit initial-key-submit" onClick={() => void saveAdmin()} disabled={saving || !adminName.trim() || !adminKey.trim()}>
              {saving ? <Loader2 className="spin" size={17} /> : <KeyRound size={17} />}{saving ? "Validating Admin key…" : "Save Admin key and continue"}
            </button>
            <button className="access-secondary initial-skip-admin" onClick={() => { setSaveError(""); setStep("project"); }}>Continue without Admin key</button>
          </> : <>
            {adminKeys.length > 0 && <div className="setup-choice-tabs">
              <button className={choice === "create" ? "selected" : ""} onClick={() => { setChoice("create"); setSaveError(""); }}><FolderPlus size={15} /> Create dedicated Project <small>Recommended</small></button>
              <button className={choice === "select" ? "selected" : ""} onClick={() => { setChoice("select"); setSelectedProject(null); setSaveError(""); }}><List size={15} /> Select a Project</button>
              <button className={choice === "existing" ? "selected" : ""} onClick={() => { setChoice("existing"); setSaveError(""); }}>Use existing Project key</button>
            </div>}
            {choice === "create" && adminKeys.length > 0 ? <>
              <p className="validation-note">This creates one Minefia Audio Project and a Service Account key. OpenAI Projects can be archived but cannot be deleted. The generated Project key is stored in Keychain using the Service Account name as its item name.</p>
              <label className="field-label" htmlFor="initial-project-name">Project name</label>
              <input className="text-field" id="initial-project-name" value={projectName} onChange={(event) => setProjectName(event.target.value)} placeholder="Minefia Audio" />
              <label className="field-label second-label" htmlFor="initial-service-account-name">Service Account name / Keychain item name</label>
              <input className="text-field" id="initial-service-account-name" value={serviceAccountName} onChange={(event) => setServiceAccountName(event.target.value)} placeholder="Minefia Audio" onKeyDown={(event) => event.key === "Enter" && void createProjectKey()} />
              <button className="primary-button modal-submit initial-key-submit" onClick={() => void createProjectKey()} disabled={saving || !projectName.trim() || !serviceAccountName.trim()}>
                {saving ? <Loader2 className="spin" size={17} /> : <FolderPlus size={17} />}{saving ? "Creating secure Project key…" : "Create Project and add key"}
              </button>
            </> : choice === "select" && adminKeys.length > 0 ? <>
              <p className="validation-note">Select an active Project, then create a Service Account for Minefia Audio. The new API key will be stored in Keychain; only its non-secret ID is kept in local storage.</p>
              {projectsLoading ? <div className="project-list-loading"><Loader2 className="spin" size={17} /> Loading organization Projects…</div> : <div className="setup-project-list">
                {projects.map((project) => <button key={project.id} className={`setup-project-option ${selectedProject?.id === project.id ? "selected" : ""}`} onClick={() => setSelectedProject(project)}>
                  <span className="radio-mark">{selectedProject?.id === project.id && <Check size={12} />}</span><span><strong>{project.name}</strong><small>{project.id}</small></span>
                </button>)}
                {projects.length === 0 && !saveError && <p className="validation-note">No active Projects were returned for this Admin key.</p>}
              </div>}
              {selectedProject && <>
                <label className="field-label second-label" htmlFor="selected-service-account-name">Service Account name / Keychain item name</label>
                <input className="text-field" id="selected-service-account-name" value={serviceAccountName} onChange={(event) => setServiceAccountName(event.target.value)} placeholder="Minefia Audio" onKeyDown={(event) => event.key === "Enter" && void createProjectKey()} />
                <button className="primary-button modal-submit initial-key-submit" onClick={() => void createProjectKey()} disabled={saving || projectsLoading || !serviceAccountName.trim()}>
                  {saving ? <Loader2 className="spin" size={17} /> : <KeyRound size={17} />}{saving ? "Creating Service Account…" : "Create Service Account and add key"}
                </button>
              </>}
            </> : <>
              {!adminKeys.length && <p className="validation-note">Enter a Project API key from a dedicated Minefia Audio Project. A dedicated Project is recommended so the app's usage and key access stay separate.</p>}
              {adminKeys.length > 0 && <p className="validation-note">If you already created a Project just for Minefia Audio, enter its Project API key. Otherwise, use the recommended Project creation option.</p>}
              <label className="field-label" htmlFor="initial-key-item-name">Keychain item name</label>
              <input className="text-field" id="initial-key-item-name" value={itemName} onChange={(event) => setItemName(event.target.value)} placeholder="Minefia Audio" />
              <label className="field-label second-label" htmlFor="initial-project-api-key">Project API key</label>
              <div className="secret-field">
                <input id="initial-project-api-key" type={showKey ? "text" : "password"} value={apiKey} onChange={(event) => setApiKey(event.target.value)} placeholder="sk-…" autoComplete="off" onKeyDown={(event) => event.key === "Enter" && void saveProjectKey()} />
                <button onClick={() => setShowKey((value) => !value)} aria-label="Toggle key visibility">{showKey ? <EyeOff size={18} /> : <Eye size={18} />}</button>
              </div>
              {adminKeys.length > 0 && <p className="validation-note">This should be a key from a Project dedicated to Minefia Audio.</p>}
              {adminKeys.length > 0 && <p className="validation-note">Entering a key manually does not enable Usage. Usage requires a Project selected or created here and its Service Account key.</p>}
              <button className="primary-button modal-submit initial-key-submit" onClick={() => void saveProjectKey()} disabled={saving || !itemName.trim() || !apiKey.trim()}>
                {saving ? <Loader2 className="spin" size={17} /> : <KeyRound size={17} />}{saving ? "Validating Project key…" : "Add Project API key"}
              </button>
            </>}
            {adminKeys.length === 0 && <button className="access-secondary initial-skip-admin" onClick={() => setStep("admin")}>Add an optional Admin key instead</button>}
            <p className="validation-note">Project keys are validated with <strong>GET /v1/models</strong> and saved to the system Keychain.</p>
          </>}
          {saveError && <p className="form-error">{saveError}</p>}
          <div className="access-assurance"><ShieldCheck size={16} /><span><strong>Stored securely</strong>The API key stays in your system Keychain.</span></div>
        </section>
      </div>
    );
  }

  return (
    <section className="access-card access-error" aria-live="polite">
      <span className="access-icon"><CircleAlert size={29} /></span>
      <p className="eyebrow">MODEL ACCESS REQUIRED</p>
      <h1>We couldn't load transcription models</h1>
      <p className="access-copy">{keyLabel ? <><strong>{keyLabel}</strong> is still saved, but it cannot currently provide a supported transcription model.</> : "Select a working API key to continue."}</p>
      {error && <div className="access-error-detail">{error}</div>}
      <div className="access-actions">
        <button className="primary-button access-primary" onClick={onOpenSettings}>
          <KeyRound size={18} /> Add another API key
        </button>
        <button className="access-secondary" onClick={onRetry}>
          <RefreshCw size={16} /> Try again
        </button>
      </div>
      <p className="access-preserved">Your existing API key has not been deleted.</p>
    </section>
  );

  async function saveProjectKey() {
    if (!itemName.trim() || !apiKey.trim()) return;
    setSaving(true);
    setSaveError("");
    try {
      await invoke("save_api_key", { label: itemName.trim(), apiKey: apiKey.trim() });
      await onRetry();
    } catch (cause) {
      setSaveError(errorMessage(cause));
    } finally {
      setSaving(false);
    }
  }

  async function saveAdmin() {
    if (!adminName.trim() || !adminKey.trim()) return;
    setSaving(true); setSaveError("");
    try {
      await invoke("save_admin_api_key", { label: adminName.trim(), apiKey: adminKey.trim() });
      setAdminKey("");
      await onRetry();
      setStep("project");
    } catch (cause) { setSaveError(errorMessage(cause)); }
    finally { setSaving(false); }
  }

  async function createProjectKey() {
    if (!serviceAccountName.trim()) return;
    if (choice === "select" && !selectedProject) return;
    if (choice === "create" && !projectName.trim()) return;
    setSaving(true); setSaveError("");
    try {
      if (choice === "select" && selectedProject) {
        await invoke("select_minefia_project", { projectId: selectedProject.id, projectName: selectedProject.name });
      }
      const created = await invoke<CreatedProjectKey>("create_minefia_project_key", { projectName: projectName.trim(), serviceAccountName: serviceAccountName.trim() });
      rememberOpenAIKeyId(created.key.openaiKeyId);
      await onRetry();
    } catch (cause) { setSaveError(errorMessage(cause)); }
    finally { setSaving(false); }
  }
}
