// OS check, not shell detection (desktop.ts owns app-vs-browser). The UA is the
// only signal shared by the Android app's WebView and Android browsers.
export const isAndroid = (): boolean => /Android/.test(navigator.userAgent);
