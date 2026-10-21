import { AlertTriangle, Download } from "lucide-react";

type Props = {
  open: boolean;
  title: string;
  description: string;
  details: string;
  confirmLabel: string;
  icon?: "warning" | "download";
  onCancel: () => void;
  onConfirm: () => void;
};

export function ConfirmationModal({ open, title, description, details, confirmLabel, icon = "warning", onCancel, onConfirm }: Props) {
  if (!open) return null;
  return <div className="modal-backdrop" role="presentation" onMouseDown={(event) => {
    if (event.target === event.currentTarget) onCancel();
  }}>
    <section className="modal conversion-modal" role="dialog" aria-modal="true" aria-labelledby="confirmation-title">
      <div className="modal-icon conversion-warning">{icon === "download" ? <Download size={21} /> : <AlertTriangle size={21} />}</div>
      <p className="eyebrow">USER CONFIRMATION</p>
      <h2 id="confirmation-title">{title}</h2>
      <div className="conversion-copy"><p>{description}</p><p>{details}</p></div>
      <div className="modal-actions"><button className="secondary-button" onClick={onCancel}>Cancel</button><button className="primary-button" onClick={onConfirm}>{confirmLabel}</button></div>
    </section>
  </div>;
}
