// Inline SVG, so the page stays one request with no icon font, no sprite
// sheet, and nothing for the CSP to allow. Each icon draws in `currentColor`
// and sizes off `em`, so it follows the text it sits next to.

const svg = (body: string, size = '1em') =>
  `<svg viewBox="0 0 24 24" width="${size}" height="${size}" fill="currentColor" aria-hidden="true" focusable="false">${body}</svg>`;

/** Four squares: the Windows logo, which really is this simple. */
export const windows = svg(
  '<rect x="3" y="3.6" width="8" height="8" rx=".6"/>' +
    '<rect x="13" y="3.6" width="8" height="8" rx=".6"/>' +
    '<rect x="3" y="12.8" width="8" height="8" rx=".6"/>' +
    '<rect x="13" y="12.8" width="8" height="8" rx=".6"/>',
);

/** The Apple silhouette. */
export const apple = svg(
  '<path d="M17.1 12.7c0-2.6 2.1-3.9 2.2-4-1.2-1.8-3.1-2-3.8-2-1.6-.2-3.1 1-3.9 1-.8 0-2-1-3.3-1-1.7 0-3.3 1-4.2 2.5-1.8 3.1-.5 7.8 1.3 10.3.9 1.3 1.9 2.7 3.3 2.6 1.3-.1 1.8-.8 3.4-.8s2 .8 3.4.8c1.4 0 2.3-1.3 3.2-2.5.7-1 1-2 1.1-2.1-.1 0-2.7-1.1-2.7-4.8z"/>' +
    '<path d="M14.6 4.6c.7-.9 1.2-2.1 1.1-3.3-1 0-2.3.7-3.1 1.6-.7.8-1.2 2-1.1 3.2 1.2.1 2.3-.6 3.1-1.5z"/>',
);

/** Tux, flattened to a silhouette: head, body, flippers, feet. */
export const linux = svg(
  '<path d="M12 1.6c-2.3 0-4 1.8-4 4.1v1.6c0 .7-.3 1.3-.7 1.9-1.4 1.8-2.3 3.6-2.8 5.3-.3 1-.6 1.7-1.1 2.3-.5.6-.6 1.4-.2 2 .4.6 1.1.9 1.8.7l1.6-.4c.3 1.4 1.5 2.5 3 2.8 1.6.3 3.2.3 4.8 0 1.5-.3 2.7-1.4 3-2.8l1.6.4c.7.2 1.4-.1 1.8-.7.4-.6.3-1.4-.2-2-.5-.6-.8-1.3-1.1-2.3-.5-1.7-1.4-3.5-2.8-5.3-.4-.6-.7-1.2-.7-1.9V5.7c0-2.3-1.7-4.1-4-4.1z"/>' +
    '<circle cx="10.3" cy="5.9" r="1.25" fill="var(--bg)"/>' +
    '<circle cx="13.7" cy="5.9" r="1.25" fill="var(--bg)"/>' +
    '<circle cx="10.3" cy="6.1" r=".5"/>' +
    '<circle cx="13.7" cy="6.1" r=".5"/>' +
    '<path d="M12 7.1c.8 0 1.5.5 1.5 1s-.7 1-1.5 1-1.5-.5-1.5-1 .7-1 1.5-1z" fill="#f2a83b"/>',
);

/** The GitHub mark (Octicons). */
export const github = svg(
  '<path d="M12 .5A11.5 11.5 0 0 0 .5 12c0 5.08 3.29 9.39 7.86 10.91.58.1.79-.25.79-.56 0-.27-.01-1.18-.02-2.14-3.2.7-3.87-1.37-3.87-1.37-.53-1.33-1.28-1.69-1.28-1.69-1.05-.71.08-.7.08-.7 1.16.08 1.77 1.19 1.77 1.19 1.03 1.76 2.7 1.25 3.36.96.1-.75.4-1.26.73-1.55-2.55-.29-5.24-1.28-5.24-5.69 0-1.26.45-2.29 1.19-3.09-.12-.29-.52-1.46.11-3.05 0 0 .97-.31 3.18 1.18a11 11 0 0 1 5.79 0c2.2-1.49 3.17-1.18 3.17-1.18.63 1.59.23 2.76.12 3.05.74.8 1.18 1.83 1.18 3.09 0 4.42-2.69 5.39-5.25 5.68.41.36.78 1.06.78 2.14 0 1.55-.01 2.79-.01 3.17 0 .31.21.67.8.56A11.5 11.5 0 0 0 23.5 12 11.5 11.5 0 0 0 12 .5z"/>',
);
