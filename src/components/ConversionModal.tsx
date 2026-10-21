import { AlertTriangle } from "lucide-react";

type ConversionModalProps = {
  open: boolean;
  onCancel: () => void;
  onConfirm: () => void;
};

export function ConversionModal({ open, onCancel, onConfirm }: ConversionModalProps) {
  if (!open) return null;

  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={(event) => {
      if (event.target === event.currentTarget) onCancel();
    }}>
      <section className="modal conversion-modal" role="dialog" aria-modal="true" aria-labelledby="conversion-title">
        <div className="modal-icon conversion-warning"><AlertTriangle size={21} /></div>
        <h2 id="conversion-title">音声の変換が必要です</h2>
        <div className="conversion-copy">
          <p>この音声形式は、文字起こしを行う前に再エンコードが必要です。</p>
          <p>再エンコードには追加の処理時間がかかり、音質が低下する可能性があります。</p>
          <p>続行しますか？</p>
        </div>
        <div className="modal-actions">
          <button className="secondary-button" onClick={onCancel}>Cancel</button>
          <button className="primary-button" onClick={onConfirm}>Convert</button>
        </div>
      </section>
    </div>
  );
}
