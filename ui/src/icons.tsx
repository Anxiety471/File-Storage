// Small inline icon set (24px grid, stroke-based) so the UI needs no icon dependency.
const paths: Record<string, string> = {
  drive: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
  folder: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
  file: "M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8zM14 3v5h5M9 13h6M9 17h6",
  star: "M12 3.5l2.6 5.3 5.9.9-4.3 4.1 1 5.8-5.2-2.7-5.2 2.7 1-5.8-4.3-4.1 5.9-.9z",
  trash: "M4 7h16M10 11v6M14 11v6M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12M9 7V4h6v3",
  restore: "M3 12a9 9 0 1 0 3-6.7L3 8M3 3v5h5",
  plus: "M12 5v14M5 12h14",
  search: "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM20 20l-4-4",
  close: "M6 6l12 12M18 6L6 18",
  back: "M15 5l-7 7 7 7",
  chevron: "M9 6l6 6-6 6",
  history: "M3 12a9 9 0 1 0 3-6.7L3 8M3 3v5h5M12 7v5l3 2",
  edit: "M4 20h4L19 9l-4-4L4 16zM14 6l4 4",
  more: "M12 6h.01M12 12h.01M12 18h.01",
  eye: "M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12zM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z",
  check: "M5 12l5 5L20 7",
  download: "M12 4v11M7 10l5 5 5-5M5 20h14",
  upload: "M12 20V9M7 14l5-5 5 5M5 4h14",
  code: "M9 8l-5 4 5 4M15 8l5 4-5 4M13 5l-2 14",
  sheet: "M4 5a1 1 0 0 1 1-1h14a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1zM4 10h16M4 15h16M10 4v16",
  image: "M4 5a1 1 0 0 1 1-1h14a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1zM4 16l5-5 4 4 3-3 4 4M15 8.5h.01",
  play: "M4 5a1 1 0 0 1 1-1h14a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1zM10 9v6l5-3z",
  archive: "M4 5h16v4H4zM5 9v10a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V9M10 13h4",
  pdf: "M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8zM14 3v5h5M8.5 16v-4h1.5a1 1 0 0 1 0 2H8.5M13 12v4h1a2 2 0 0 0 0-4z",
  clock: "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 7v5l3 2",
  move: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2zM10 13h6M13 10l3 3-3 3",
  copy: "M9 9h10v12H9zM5 15V3h10",
  diff: "M6 3v12M6 15a3 3 0 1 0 0 6 3 3 0 0 0 0-6zM18 21V9M18 9a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM6 9h6M15 15h6",
  key: "M15 7a4 4 0 1 1-3.9 4.9L4 19v2h3v-2h2v-2h2l1.1-1.1A4 4 0 0 1 15 7zM16 10h.01",
  sort: "M7 4v16M4 17l3 3 3-3M17 20V4M14 7l3-3 3 3",
  refresh: "M20 12a8 8 0 1 1-2.3-5.7L20 8M20 3v5h-5",
  slides: "M4 5a1 1 0 0 1 1-1h14a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1zM8 9h8v6H8z",
};

export function Icon({ name, size = 20, filled = false }: { name: keyof typeof paths; size?: number; filled?: boolean }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill={filled ? "currentColor" : "none"}
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d={paths[name]} />
    </svg>
  );
}
