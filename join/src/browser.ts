import { chromium, type BrowserContext } from "playwright";

import { AI_MIC, MEETING_SINK } from "./devices.js";

export interface BrowserOptions {
  profileDir: string;
  headless: boolean;
  executablePath?: string;
  /** Affiché sur la caméra virtuelle. */
  displayName?: string;
}

/** Chromium dédié au bot, branché sur ses périphériques : il joue la réunion dans fdm_meeting et
 * prend fdm_ai_mic comme micro. Profil persistant : on peut y connecter une fois un compte du bot. */
export async function launchBotBrowser(opts: BrowserOptions): Promise<BrowserContext> {
  const context = await chromium.launchPersistentContext(opts.profileDir, {
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
  await context.addInitScript(virtualCamera, opts.displayName ?? "");
  await context.addInitScript(watchConnections);
  return context;
}

/** Préfixe des messages de console relayés dans le journal du bot. */
export const RTC_LOG = "[fdm-rtc]";

/** Journalise l'état des connexions WebRTC de Teams (via la console, relayée par fdm-join) : quand
 * un appel est coupé, on voit si c'est le réseau (ICE failed/disconnected) ou Teams lui-même. */
function watchConnections(): void {
  const Real = window.RTCPeerConnection;
  if (!Real) return;
  let n = 0;
  class Watched extends Real {
    constructor(...args: ConstructorParameters<typeof RTCPeerConnection>) {
      super(...args);
      const id = ++n;
      const say = (m: string) => console.log(`[fdm-rtc] connexion ${id} : ${m}`);
      this.addEventListener("connectionstatechange", () => say(`état ${this.connectionState}`));
      this.addEventListener("iceconnectionstatechange", () => {
        if (["failed", "disconnected"].includes(this.iceConnectionState)) say(`ICE ${this.iceConnectionState}`);
      });
    }
  }
  window.RTCPeerConnection = Watched;
}

/** La machine du bot n'a pas de caméra. Or Teams demande micro et caméra d'un seul appel : sans
 * caméra, tout échoue (NotFoundError) et il conclut qu'on refuse l'audio et la vidéo, ce qui bloque
 * l'entrée. On fournit donc une caméra virtuelle (une image fixe à son nom) ; le micro reste le vrai. */
function virtualCamera(name: string): void {
  const md = navigator.mediaDevices;
  if (!md?.getUserMedia) return;
  const realGetUserMedia = md.getUserMedia.bind(md);
  const realEnumerate = md.enumerateDevices.bind(md);

  const cameraTrack = (): MediaStreamTrack => {
    const canvas = document.createElement("canvas");
    canvas.width = 640;
    canvas.height = 360;
    const g = canvas.getContext("2d")!;
    const draw = () => {
      g.fillStyle = "#1b2029";
      g.fillRect(0, 0, canvas.width, canvas.height);
      g.fillStyle = "#e6e9ef";
      g.font = "600 40px system-ui, sans-serif";
      g.textAlign = "center";
      g.textBaseline = "middle";
      g.fillText(name || "IA", canvas.width / 2, canvas.height / 2);
    };
    draw();
    setInterval(draw, 1000); // quelques images suffisent, mais le flux doit rester vivant
    return canvas.captureStream(5).getVideoTracks()[0];
  };

  md.getUserMedia = async (constraints?: MediaStreamConstraints) => {
    try {
      return await realGetUserMedia(constraints);
    } catch (e) {
      if (!constraints?.video || !["NotFoundError", "OverconstrainedError"].includes((e as DOMException)?.name)) throw e;
      const stream = constraints.audio ? await realGetUserMedia({ audio: constraints.audio }) : new MediaStream();
      stream.addTrack(cameraTrack());
      return stream;
    }
  };
  md.enumerateDevices = async () => {
    const devices = await realEnumerate();
    if (devices.some((d) => d.kind === "videoinput")) return devices;
    const camera = { deviceId: "fdm-camera", groupId: "fdm", kind: "videoinput", label: "Caméra du bot", toJSON() { return this; } };
    return [...devices, camera as MediaDeviceInfo];
  };
}
