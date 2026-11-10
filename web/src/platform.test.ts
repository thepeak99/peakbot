// Table test for isAndroid() (design-attach-menu §2): the attach-menu gate is
// the allow-list "UA contains Android" — app WebView AND Android browsers,
// regardless of pointer type. Linux alone is NOT Android, and iPad's
// desktop-reporting UA is NOT Android; unknown/empty UAs take the plain path.
//
// RED until web/src/platform.ts exists (the import below is the contract).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { isAndroid } from "./platform";

// Realistic full UA strings, one per platform in the design's table.
const ANDROID_CHROME_UA =
  "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Mobile Safari/537.36";
const ANDROID_WEBVIEW_UA =
  "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/126.0.0.0 Mobile Safari/537.36; wv";
const ANDROID_FIREFOX_UA =
  "Mozilla/5.0 (Android 14; Mobile; rv:140.0) Gecko/140.0 Firefox/140.0";
const IPHONE_UA =
  "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1";
const IPAD_AS_MAC_UA =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";
const WINDOWS_UA =
  "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
const LINUX_DESKTOP_UA =
  "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
// Pinned literally: the jsdom default UA (this file runs under the `node`
// environment, where navigator.userAgent is Node's own string).
const JSDOM_UA =
  "Mozilla/5.0 (linux) AppleWebKit/537.36 (KHTML, like Gecko) jsdom/29.1.1";

describe("isAndroid()", () => {
  let uaSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    uaSpy = vi.spyOn(navigator, "userAgent", "get");
  });

  afterEach(() => {
    uaSpy.mockRestore();
  });

  it.each([
    ["Chrome on Android", ANDROID_CHROME_UA],
    ["Android WebView (; wv)", ANDROID_WEBVIEW_UA],
    ["Firefox on Android", ANDROID_FIREFOX_UA],
  ])("%s → true", (_platform, ua) => {
    uaSpy.mockReturnValue(ua);
    expect(isAndroid()).toBe(true);
  });

  it.each([
    ["iPhone Safari", IPHONE_UA],
    ["iPad reporting as Macintosh", IPAD_AS_MAC_UA],
    ["Windows Chrome", WINDOWS_UA],
    ["Linux desktop (Linux alone is not Android)", LINUX_DESKTOP_UA],
    ["jsdom default UA", JSDOM_UA],
    ["empty string", ""],
  ])("%s → false", (_platform, ua) => {
    uaSpy.mockReturnValue(ua);
    expect(isAndroid()).toBe(false);
  });
});
