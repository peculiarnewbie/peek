// The canvas showing the remote screen. One element for the whole page, made
// here so the socket, input handling and components can all reach it.

export const cv = document.createElement('canvas');
cv.id = 'screen';
cv.tabIndex = 0;
cv.width = 16;
cv.height = 9;

export const ctx = cv.getContext('2d', { alpha: false })!;

/** Keyboard input goes to the canvas; return focus there after UI clicks. */
export const focusScreen = () => cv.focus();

/** Fit the canvas into the window, keeping its aspect ratio. */
export function layout() {
  const k = Math.min(innerWidth / cv.width, innerHeight / cv.height);
  cv.style.width = Math.floor(cv.width * k) + 'px';
  cv.style.height = Math.floor(cv.height * k) + 'px';
}

export function resizeScreen(w: number, h: number) {
  if (cv.width === w && cv.height === h) return;
  cv.width = w;
  cv.height = h;
  layout();
}
