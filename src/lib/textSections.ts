const DEFAULT_SECTION_CHARS = 3_500;
const MIN_BREAK_RATIO = 0.65;

export function splitReadableSections(text: string, maxChars = DEFAULT_SECTION_CHARS): string[] {
  const characters = Array.from(text.trim());
  if (characters.length <= maxChars) return text.trim() ? [text.trim()] : [];

  const sections: string[] = [];
  let start = 0;
  while (start < characters.length) {
    const idealEnd = Math.min(start + maxChars, characters.length);
    if (idealEnd === characters.length) {
      sections.push(characters.slice(start).join("").trim());
      break;
    }

    const earliestBreak = start + Math.floor(maxChars * MIN_BREAK_RATIO);
    let end = idealEnd;
    for (let index = idealEnd; index >= earliestBreak; index -= 1) {
      if (isReadableBreak(characters[index - 1], characters[index])) {
        end = index;
        break;
      }
    }

    const section = characters.slice(start, end).join("").trim();
    if (section) sections.push(section);
    start = end;
    while (characters[start]?.trim() === "") start += 1;
  }
  return sections;
}

function isReadableBreak(current?: string, next?: string): boolean {
  if (!current) return false;
  if (current === "\n") return true;
  if (["。", "！", "？"].includes(current)) return true;
  return [".", "!", "?"].includes(current) && (!next || /\s/.test(next));
}
