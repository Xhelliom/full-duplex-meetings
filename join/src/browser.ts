import { chromium, type BrowserContext } from "playwright";

import { AI_MIC, MEETING_SINK } from "./devices.js";

export interface BrowserOptions {
  profileDir: string;
  headless: boolean;
  executablePath?: string;
}

/** Chromium dédié au bot, branché sur ses périphériques : il joue la réunion dans fdm_meeting et
 * prend fdm_ai_mic comme micro. Profil persistant : on peut y connecter une fois un compte du bot. */
export async function launchBotBrowser(opts: BrowserOptions): Promise<BrowserContext> {
  return chromium.launchPersistentContext(opts.profileDir, {
    headless: opts.headless,
    executablePath: opts.executablePath,
    env: { ...process.env, PULSE_SINK: MEETING_SINK, PULSE_SOURCE: AI_MIC } as Record<string, string>,
    permissions: ["microphone", "camera", "notifications"],
    args: [
      "--use-fake-ui-for-media-stream", // accepte micro/caméra sans boîte de dialogue
      "--autoplay-policy=no-user-gesture-required",
      "--disable-features=HardwareMediaKeyHandling",
    ],
    viewport: { width: 1280, height: 800 },
    locale: "fr-FR",
  });
}
