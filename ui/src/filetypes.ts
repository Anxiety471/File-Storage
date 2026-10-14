// How the UI shows each kind of file: which viewer opens it, which icon and colour it gets,
// and which highlight.js language applies to source code.

export type Viewer =
  | "code" // editable text with syntax highlighting
  | "markdown" // editable, with a rendered preview
  | "html" // editable, with a sandboxed preview
  | "csv" // editable, with a table preview
  | "pdf"
  | "image"
  | "docx"
  | "sheet"
  | "audio"
  | "video"
  | "binary"; // download only

export type Badge = "doc" | "pdf" | "sheet" | "slides" | "image" | "code" | "text" | "media" | "archive" | "other";

export interface FileType {
  viewer: Viewer;
  badge: Badge;
  /** highlight.js language id, when the file is source code */
  lang?: string;
  /** Short human label, e.g. "Word document" */
  label: string;
}

// extension -> highlight.js language
const CODE: Record<string, string> = {
  rs: "rust", py: "python", pyi: "python", js: "javascript", mjs: "javascript", cjs: "javascript", jsx: "javascript",
  ts: "typescript", tsx: "typescript", mts: "typescript", go: "go", java: "java", kt: "kotlin", kts: "kotlin",
  swift: "swift", c: "c", h: "c", cc: "cpp", cpp: "cpp", cxx: "cpp", hpp: "cpp", hh: "cpp", cs: "csharp",
  rb: "ruby", php: "php", pl: "perl", lua: "lua", r: "r", scala: "scala", dart: "dart", ex: "elixir", exs: "elixir",
  erl: "erlang", hs: "haskell", clj: "clojure", fs: "fsharp", ml: "ocaml", zig: "zig", nim: "nim", jl: "julia",
  sh: "bash", bash: "bash", zsh: "bash", fish: "bash", ps1: "powershell", bat: "dos", cmd: "dos",
  sql: "sql", graphql: "graphql", gql: "graphql", proto: "protobuf",
  css: "css", scss: "scss", sass: "scss", less: "less", vue: "xml", svelte: "xml", xml: "xml", svg: "xml", xsl: "xml",
  json: "json", jsonc: "json", json5: "json", yaml: "yaml", yml: "yaml", toml: "ini", ini: "ini", cfg: "ini", conf: "ini",
  env: "bash", properties: "properties", gradle: "groovy", groovy: "groovy", tf: "hcl", hcl: "hcl",
  diff: "diff", patch: "diff", makefile: "makefile", mk: "makefile", dockerfile: "dockerfile", cmake: "cmake",
  tex: "latex", vim: "vim", asm: "x86asm", s: "x86asm", sol: "solidity", m: "objectivec", mm: "objectivec",
};

const TEXT = new Set(["txt", "log", "text", "rst", "adoc", "gitignore", "dockerignore", "editorconfig", "license", "readme"]);

export function extOf(name: string): string {
  const lower = name.toLowerCase();
  const dot = lower.lastIndexOf(".");
  // Dockerfile, Makefile and dotfiles like .gitignore use their whole name
  if (dot <= 0) return lower.replace(/^\./, "");
  return lower.slice(dot + 1);
}

export function fileType(name: string, mime?: string | null): FileType {
  const ext = extOf(name);
  switch (ext) {
    case "md":
    case "markdown":
    case "mdx":
      return { viewer: "markdown", badge: "text", lang: "markdown", label: "Markdown" };
    case "html":
    case "htm":
      return { viewer: "html", badge: "code", lang: "xml", label: "HTML" };
    case "csv":
    case "tsv":
      return { viewer: "csv", badge: "sheet", label: ext.toUpperCase() };
    case "pdf":
      return { viewer: "pdf", badge: "pdf", label: "PDF" };
    case "docx":
      return { viewer: "docx", badge: "doc", label: "Word document" };
    case "doc":
    case "odt":
    case "rtf":
      return { viewer: "binary", badge: "doc", label: "Document" };
    case "xlsx":
    case "xlsm":
    case "xls":
    case "ods":
      return { viewer: "sheet", badge: "sheet", label: ext === "xls" ? "Excel 97-2003 workbook" : "Spreadsheet" };
    case "ppt":
    case "pptx":
    case "odp":
      return { viewer: "binary", badge: "slides", label: "Presentation" };
    case "png":
    case "jpg":
    case "jpeg":
    case "gif":
    case "webp":
    case "bmp":
    case "ico":
    case "avif":
      return { viewer: "image", badge: "image", label: "Image" };
    case "svg":
      return { viewer: "image", badge: "image", lang: "xml", label: "SVG image" };
    case "mp3":
    case "wav":
    case "ogg":
    case "m4a":
    case "flac":
      return { viewer: "audio", badge: "media", label: "Audio" };
    case "mp4":
    case "webm":
    case "mov":
      return { viewer: "video", badge: "media", label: "Video" };
    case "zip":
    case "gz":
    case "tgz":
    case "tar":
    case "7z":
    case "rar":
      return { viewer: "binary", badge: "archive", label: "Archive" };
  }
  if (CODE[ext]) return { viewer: "code", badge: "code", lang: CODE[ext], label: "Source code" };
  if (TEXT.has(ext)) return { viewer: "code", badge: "text", label: "Text" };
  if (mime && (mime.startsWith("text/") || /json|xml|yaml|toml/.test(mime))) return { viewer: "code", badge: "text", label: "Text" };
  if (mime === undefined || mime === null) return { viewer: "code", badge: "text", label: "Text" };
  return { viewer: "binary", badge: "other", label: "File" };
}

/** Text viewers edit the file in place; the others take a new upload for a new revision. */
export const isTextViewer = (v: Viewer) => v === "code" || v === "markdown" || v === "html" || v === "csv";
