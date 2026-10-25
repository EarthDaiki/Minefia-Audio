import { useMemo, useState } from "react";
import { AudioLines, FileText, FolderOpen, Search, Sparkles, Trash2, X } from "lucide-react";
import { formatBytes } from "../lib/format";
import type { LibraryFile, SavedTranscript } from "../types";

type Props = {
  open: boolean;
  files: LibraryFile[];
  transcripts: SavedTranscript[];
  deletionDisabled: boolean;
  onClose: () => void;
  onOpenFile: (file: LibraryFile) => void;
  onOpenTranscript: (transcript: SavedTranscript) => void;
  onRevealPath: (path: string) => void;
  onDeleteFile: (file: LibraryFile) => void;
  onDeleteTranscript: (transcript: SavedTranscript) => void;
};

export function LibraryModal({
  open,
  files,
  transcripts,
  deletionDisabled,
  onClose,
  onOpenFile,
  onOpenTranscript,
  onRevealPath,
  onDeleteFile,
  onDeleteTranscript,
}: Props) {
  const [tab, setTab] = useState<"media" | "transcripts" | "enhanced" | "summaries" | "notes" | "translations" | "summaryTranslations" | "notesTranslations">("media");
  const [query, setQuery] = useState("");
  const filteredFiles = useMemo(() => files.filter((file) => file.name.toLowerCase().includes(query.toLowerCase())), [files, query]);
  const filteredTranscripts = useMemo(() => transcripts.filter((item) => {
    const matchesQuery = item.name.toLowerCase().includes(query.toLowerCase());
    if (!matchesQuery) return false;
    if (tab === "summaries") return item.kind === "summary";
    if (tab === "notes") return item.kind === "notes" || item.kind === "study_notes";
    if (tab === "translations") return item.kind === "translation";
    if (tab === "summaryTranslations") return item.kind === "summary_translation";
    if (tab === "notesTranslations") return item.kind === "notes_translation";
    if (tab === "enhanced") return item.kind === "enhanced";
    return item.kind === "transcript";
  }), [transcripts, query, tab]);
  if (!open) return null;

  const mediaGroups = new Map<string, LibraryFile[]>();
  for (const file of filteredFiles) mediaGroups.set(file.date, [...(mediaGroups.get(file.date) ?? []), file]);
  const transcriptGroups = new Map<string, SavedTranscript[]>();
  for (const item of filteredTranscripts) transcriptGroups.set(item.date, [...(transcriptGroups.get(item.date) ?? []), item]);
  const groups = tab === "media"
    ? Array.from(mediaGroups, ([date, items]) => ({ date, items }))
    : Array.from(transcriptGroups, ([date, items]) => ({ date, items }));

  return <div className="modal-backdrop library-backdrop" role="presentation" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
    <section className="modal library-modal" role="dialog" aria-modal="true" aria-labelledby="library-title">
      <button className="icon-button modal-close" onClick={onClose} aria-label="Close library"><X size={18} /></button>
      <div className="modal-icon"><FolderOpen size={22} /></div>
      <p className="eyebrow">YOUR MINEFIA FILES</p>
      <h2 id="library-title">Library</h2>
      <p className="modal-copy">Manage saved media, transcripts, summaries, and translations. Local files are referenced in place; deleting one here deletes the original file. App-created recordings and extracted audio can also be deleted here.</p>

      <div className="library-tabs" role="tablist" aria-label="Saved files">
        <button className={tab === "media" ? "selected" : ""} onClick={() => setTab("media")}><AudioLines size={14} />Media <small>{files.length}</small></button>
        <button className={tab === "transcripts" ? "selected" : ""} onClick={() => setTab("transcripts")}><FileText size={14} />Transcripts <small>{transcripts.filter((item) => item.kind === "transcript").length}</small></button>
        <button className={tab === "enhanced" ? "selected" : ""} onClick={() => setTab("enhanced")}><Sparkles size={14} />Enhanced <small>{transcripts.filter((item) => item.kind === "enhanced").length}</small></button>
        <button className={tab === "summaries" ? "selected" : ""} onClick={() => setTab("summaries")}><FileText size={14} />Summaries <small>{transcripts.filter((item) => item.kind === "summary").length}</small></button>
        <button className={tab === "notes" ? "selected" : ""} onClick={() => setTab("notes")}><FileText size={14} />Notes <small>{transcripts.filter((item) => item.kind === "notes").length}</small></button>
        <button className={tab === "translations" ? "selected" : ""} onClick={() => setTab("translations")}><FileText size={14} />Transcript translations <small>{transcripts.filter((item) => item.kind === "translation").length}</small></button>
        <button className={tab === "summaryTranslations" ? "selected" : ""} onClick={() => setTab("summaryTranslations")}><FileText size={14} />Summary translations <small>{transcripts.filter((item) => item.kind === "summary_translation").length}</small></button>
        <button className={tab === "notesTranslations" ? "selected" : ""} onClick={() => setTab("notesTranslations")}><FileText size={14} />Note translations <small>{transcripts.filter((item) => item.kind === "notes_translation").length}</small></button>
      </div>
      <label className="library-search"><Search size={15} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder={`Search ${tab}…`} /></label>
      <div className="library-items">
        {groups.sort((a, b) => b.date.localeCompare(a.date)).map(({ date, items }) => <section className="library-date-group" key={date}>
          <h3>{date}<small>{items.length} {tab === "media" ? "files" : tab}</small></h3>
          {items.map((item) => "sizeBytes" in item
            ? <div className="library-item-entry" key={item.path}><div className="library-item-row"><button className="library-item-open" onClick={() => onOpenFile(item)}><AudioLines size={16} /><span>{item.name}<small>{item.isReference ? "Original file · stored on this device" : formatBytes(item.sizeBytes)}</small></span></button><button className={`library-delete-button${item.isReference ? " library-remove-reference" : ""}`} aria-label={`${item.isReference ? "Delete original file" : "Delete"} ${item.name}`} title={item.isReference ? "Delete original file" : "Delete saved media"} disabled={deletionDisabled} onClick={() => onDeleteFile(item)}><Trash2 size={14} />{item.isReference && <span>Delete</span>}</button></div><button className="library-path-button" title={`Open the folder containing ${item.name}`} aria-label={`Open folder containing ${item.name}`} onClick={() => onRevealPath(item.path)}><FolderOpen size={14} /><span><small>File path · click to show folder</small><code>{item.path}</code></span></button></div>
            : <div className="library-item-entry" key={item.id}><div className="library-item-row"><button className="library-item-open" onClick={() => onOpenTranscript(item)}><FileText size={16} /><span>{item.name}<small>{item.text.slice(0, 88).replace(/\s+/g, " ")}</small></span></button><button className="library-delete-button" aria-label={`Delete ${item.name}`} title="Delete transcript" disabled={deletionDisabled} onClick={() => onDeleteTranscript(item)}><Trash2 size={14} /></button></div><button className="library-path-button" title={`Open transcript folder for ${item.name}`} aria-label={`Open transcript folder for ${item.name}`} onClick={() => onRevealPath(item.path)}><FolderOpen size={14} /><span><small>Transcript folder · click to show</small><code>{item.path}</code></span></button></div>)}
        </section>)}
        {groups.length === 0 && <div className="library-empty"><FolderOpen size={22} /><strong>No saved {tab} found</strong><span>Local references, recordings, extracted audio, transcripts, summaries, and translations stay here until you delete them.</span></div>}
      </div>
      <div className="library-footer"><span>{files.length} media files</span><span>{transcripts.length} saved text items</span><small>Saved items remain until you delete them.</small></div>
    </section>
  </div>;
}
