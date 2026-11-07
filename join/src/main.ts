#!/usr/bin/env node
// fdm-join : fait entrer le bot dans une réunion. Ne touche ni à l'audio ni à l'IA (rôle du binaire
// Rust `fdm`, qui crée les périphériques PipeWire) : ouvre un navigateur branché dessus et clique.
import { resolve } from "node:path";
import { parseArgs } from "node:util";

import { RTC_LOG, launchBotBrowser } from "./browser.js";
import { AI_MIC, MEETING_SINK, devicesPresent, routeBrowserStreams } from "./devices.js";
import { platformOf } from "./platform.js";
import { joinTeams, leaveTeams, waitForEnd, waitInCall } from "./teams.js";

const log = (m: string) => console.log(`${new Date().toLocaleTimeString("fr-FR")} [fdm-join] ${m}`);

const { values, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    name: { type: "string", default: process.env.FDM_AGENT_NAME ?? "Nova" },
    headless: { type: "boolean", default: false },
    profile: { type: "string", default: process.env.FDM_BROWSER_PROFILE ?? ".fdm-browser-profile" },
    browser: { type: "string", default: process.env.FDM_BROWSER_PATH },
    help: { type: "boolean", short: "h", default: false },
  },
});

const url = positionals[0];
if (values.help || !url) {
  console.log(`Usage : fdm-join <lien de réunion> [--name Nova] [--headless] [--profile DIR] [--browser CHEMIN]`);
  process.exit(values.help ? 0 : 2);
}
if (!devicesPresent()) {
  console.error(`Périphériques « ${MEETING_SINK} » / « ${AI_MIC} » absents : lancez d'abord \`fdm\`.`);
  process.exit(1);
}
const platform = platformOf(url);
if (platform !== "teams") {
  console.error(`Plateforme non gérée pour l'instant : ${platform} (seul Teams l'est).`);
  process.exit(1);
}

const profileDir = resolve(values.profile!);
const context = await launchBotBrowser({ profileDir, headless: values.headless!, executablePath: values.browser, displayName: values.name });
const page = context.pages()[0] ?? (await context.newPage());
const routing = setInterval(() => routeBrowserStreams(profileDir, log), 3000);
// État des connexions WebRTC de Teams (voir watchConnections) : utile quand un appel est coupé.
page.on("console", (msg) => {
  const text = msg.text();
  if (text.startsWith(RTC_LOG)) log(text.slice(RTC_LOG.length).trim());
});

let stopping = false;
async function stop(code = 0): Promise<never> {
  if (!stopping) {
    stopping = true;
    clearInterval(routing);
    log("le bot quitte la réunion");
    await leaveTeams(page, log).catch(() => {});
    await context.close().catch(() => {});
  }
  process.exit(code);
}
process.on("SIGINT", () => void stop());
process.on("SIGTERM", () => void stop());
context.on("close", () => void stop()); // fenêtre fermée à la main

try {
  await joinTeams(page, url, values.name!, log);
} catch (e) {
  // On garde la fenêtre : on peut finir à la main (l'audio du bot est déjà branché).
  log(`⚠️  entrée automatique impossible (${(e as Error).message.split("\n")[0]}) — finissez dans la fenêtre du bot`);
}

// Une fois dans l'appel (automatiquement ou à la main), on surveille la fin de la réunion pour
// ne pas laisser tourner la session gpt-live : le service voit le navigateur s'arrêter et la ferme.
await waitInCall(page);
await waitForEnd(page, log);
await stop(0);
