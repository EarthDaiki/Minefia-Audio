import { ArrowRight, CircleAlert, KeyRound, Loader2, RefreshCw, ShieldCheck } from "lucide-react";

type Props = {
  state: "loading" | "missing" | "error";
  keyLabel?: string;
  error?: string;
  onOpenSettings: () => void;
  onRetry: () => void;
};

export function ApiAccessGate({ state, keyLabel, error, onOpenSettings, onRetry }: Props) {
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
      <section className="access-card">
        <span className="access-icon"><KeyRound size={29} /></span>
        <p className="eyebrow">SECURE SETUP</p>
        <h1>Add your OpenAI API key</h1>
        <p className="access-copy">Connect an API key before adding audio. Minefia Audio validates it with the Models API and stores it in your system Keychain.</p>
        <button className="primary-button access-primary" onClick={onOpenSettings}>
          <KeyRound size={18} /> Add API key <ArrowRight size={18} />
        </button>
        <div className="access-assurance"><ShieldCheck size={16} /><span><strong>Stored securely</strong>Your key stays in the macOS Keychain service Minefia-Audio.</span></div>
      </section>
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
}
