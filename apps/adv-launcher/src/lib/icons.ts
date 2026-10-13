/**
 * Inline icon paths on a 16×16 grid, drawn with `currentColor`. Stroke icons
 * use the stroke width given by <Icon>; entries starting with "fill:" are filled.
 */
export const icons = {
  grid: '<rect x="2" y="2" width="5" height="5" rx="1"/><rect x="9" y="2" width="5" height="5" rx="1"/><rect x="2" y="9" width="5" height="5" rx="1"/><rect x="9" y="9" width="5" height="5" rx="1"/>',
  clock: '<circle cx="8" cy="8" r="6"/><path d="M8 4.8V8l2.2 1.4"/>',
  star: '<path d="M8 2.2l1.7 3.6 3.9.5-2.9 2.7.7 3.9L8 11l-3.4 1.9.7-3.9L2.4 6.3l3.9-.5z"/>',
  unstarted: '<circle cx="8" cy="8" r="6" stroke-dasharray="2.2 2.2"/>',
  chart: '<path d="M3 13.5V9M6.5 13.5V4M10 13.5V7M13.5 13.5V2.5"/>',
  folder: '<path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h3l1.5 1.5h4.5A1.5 1.5 0 0 1 14 6v5.5a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 11.5z"/>',
  "folder-plus": '<path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h3l1.5 1.5h4.5A1.5 1.5 0 0 1 14 6v5.5a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 11.5z"/><path d="M8 7v4M6 9h4"/>',
  "folder-question": '<path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h3l1.5 1.5h4.5A1.5 1.5 0 0 1 14 6v5.5a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 11.5z"/><path d="M6.6 7.4a1.4 1.4 0 1 1 1.9 1.3c-.3.1-.5.4-.5.7M8 11h.01"/>',
  disk: '<rect x="2" y="4.5" width="12" height="7" rx="1.5"/><path d="M4.5 9h.01M7 9h.01" stroke-width="1.8"/>',
  sliders: '<path d="M3 4.5h6M12 4.5h1M3 11.5h1M7 11.5h6"/><circle cx="10.5" cy="4.5" r="1.5"/><circle cx="5.5" cy="11.5" r="1.5"/>',
  search: '<circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5L14 14"/>',
  "chevron-down": '<path d="M4 6l4 4 4-4"/>',
  "chevron-left": '<path d="M10 3.5L5.5 8l4.5 4.5"/>',
  "chevron-right": '<path d="M6 3.5l4.5 4.5L6 12.5"/>',
  plus: '<path d="M8 3v10M3 8h10"/>',
  close: '<path d="M4 4l8 8M12 4l-8 8"/>',
  check: '<path d="M3.5 8.5l3 3 6-7"/>',
  play: 'fill:<path d="M4.5 2.8v10.4L13 8z"/>',
  pause: 'fill:<rect x="4" y="3" width="2.6" height="10" rx=".8"/><rect x="9.4" y="3" width="2.6" height="10" rx=".8"/>',
  more: 'fill:<circle cx="3.5" cy="8" r="1.3"/><circle cx="8" cy="8" r="1.3"/><circle cx="12.5" cy="8" r="1.3"/>',
  unlink: '<path d="M6.5 9.5l-2 2a2.1 2.1 0 0 1-3-3l2-2M9.5 6.5l2-2a2.1 2.1 0 0 1 3 3l-2 2M2.5 2.5l11 11"/>',
  eye: '<path d="M1.5 8s2.4-4.5 6.5-4.5S14.5 8 14.5 8 12.1 12.5 8 12.5 1.5 8 1.5 8z"/><circle cx="8" cy="8" r="2"/>',
  "eye-off": '<path d="M6.3 3.7A6.8 6.8 0 0 1 8 3.5c4.1 0 6.5 4.5 6.5 4.5a11 11 0 0 1-1.8 2.3M4.2 4.9A11.3 11.3 0 0 0 1.5 8s2.4 4.5 6.5 4.5a6.6 6.6 0 0 0 3.1-.8M6.6 6.6a2 2 0 0 0 2.8 2.8M2 2l12 12"/>',
  file: '<path d="M4 1.8h5L12.5 5.3v8.4a.8.8 0 0 1-.8.8H4.3a.8.8 0 0 1-.8-.8V2.6a.8.8 0 0 1 .5-.8z"/><path d="M9 1.8v3.5h3.5"/>',
  info: '<circle cx="8" cy="8" r="6"/><path d="M8 7.2v4M8 4.9h.01"/>',
  warning: '<path d="M8 2.2L14.3 13H1.7z"/><path d="M8 6.5v3M8 11.2h.01"/>',
  keyboard: '<rect x="1.5" y="4" width="13" height="8" rx="1.5"/><path d="M4 6.5h.01M6.5 6.5h.01M9 6.5h.01M11.5 6.5h.01M5 9.5h6"/>',
  exit: '<path d="M6 2.5H3.5a1 1 0 0 0-1 1v9a1 1 0 0 0 1 1H6M10.5 5l3 3-3 3M13.5 8H6"/>',
} as const;

export type IconName = keyof typeof icons;
