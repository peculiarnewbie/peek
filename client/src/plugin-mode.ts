// A separate build for the opt-in screen plugin. The normal client keeps
// its existing transport and UI. No machine token is read in this mode.
export const SCREEN_PLUGIN = import.meta.env.MODE === 'screen-plugin';
