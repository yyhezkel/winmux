import { createSignal } from "solid-js";

// Phase GG: tiny global store for the in-app Markdown viewer. A module
// signal (rather than prop-threading) lets the deeply-nested
// FileManagerPane open the viewer while a single <MarkdownViewer> mounted
// at the App root renders it.

export interface MarkdownDoc {
  /** Shown in the window header — usually the file name. */
  title: string;
  /** Raw markdown source to render. */
  source: string;
}

const [doc, setDoc] = createSignal<MarkdownDoc | null>(null);

/** Reactive accessor for the open doc (null = viewer closed). */
export const markdownDoc = doc;

export function openMarkdown(title: string, source: string): void {
  setDoc({ title, source });
}

export function closeMarkdown(): void {
  setDoc(null);
}

/** True for file names winmux renders as markdown (vs opening in the OS). */
export function isMarkdownFile(name: string): boolean {
  return /\.(md|markdown|mdown|mkd|mkdn)$/i.test(name);
}
