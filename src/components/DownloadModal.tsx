import { Download, FileText, X } from "lucide-react";
import type { TranscriptVersion } from "../types";

type Props = {
  open: boolean;
  versions: TranscriptVersion[];
  selectedLanguage: string;
  onLanguageChange: (language: string) => void;
  onClose: () => void;
  onConfirm: () => void;
};

export function DownloadModal({ open, versions, selectedLanguage, onLanguageChange, onClose, onConfirm }: Props) {
  if (!open) return null;
  const selected = versions.find((version) => version.language === selectedLanguage) ?? versions[0];

  return (
    <div className="modal-backdrop" onMouseDown={onClose}>
      <section className="modal download-modal" onMouseDown={(event) => event.stopPropagation()} aria-modal="true" role="dialog">
        <button className="icon-button modal-close" onClick={onClose} aria-label="Close"><X size={18} /></button>
        <div className="modal-icon"><FileText size={22} /></div>
        <p className="eyebrow">DOWNLOAD TRANSCRIPT</p>
        <h2>Choose the text language</h2>
        <p className="modal-copy">Confirm which completed transcript should be written to the download file.</p>
        <div className="download-language-list">
          {versions.map((version) => (
            <button key={version.language} className={selected?.language === version.language ? "selected" : ""} onClick={() => onLanguageChange(version.language)}>
              <span>{version.language}</span>
              <small>{version.text.length.toLocaleString()} characters</small>
            </button>
          ))}
        </div>
        <div className="download-confirm-copy">Download the <strong>{selected?.language}</strong> transcript?</div>
        <button className="primary-button modal-submit" onClick={onConfirm} disabled={!selected}>
          <Download size={17} /> Download .txt
        </button>
      </section>
    </div>
  );
}
