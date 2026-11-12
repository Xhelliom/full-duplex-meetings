#!/usr/bin/env node
// fdm-join : fait entrer le bot dans une réunion. Ne touche ni à l'audio ni à l'IA (rôle du binaire
// Rust `fdm`, qui crée les périphériques PipeWire) : ouvre un navigateur branché dessus et clique.
import { resolve } from "node:path";
import { parseArgs } from "node:util";

import { RTC_LOG, launchBotBrowser } from "./browser.js";
import { AI_MIC, MEETING_SINK, devicesPresent, routeBrowserStreams } from "./devices.js";
import { platformOf } from "./platform.js";
import { meet } from "./meet.js";
import { zoom } from "./zoom.js";
import { teams } from "./teams.js";
import { Refused, type Driver } from "./util.js";

const BROWSE_PAGE = `<!doctype html><meta charset="utf-8"><title>Navigateur du bot</title>
<body style="font:16px system-ui;max-width:640px;margin:48px auto;line-height:1.6">
<h1>Navigateur du bot</h1>
<p>Connectez ici les comptes du bot : ils restent dans son profil pour les prochaines réunions.</p>
<ul>
<li><a href="https://accounts.google.com/">Compte Google</a> (Google Meet)</li>
<li><a href="https://login.microsoftonline.com/">Compte Microsoft professionnel</a> ou <a href="https://login.live.com/">personnel</a> (Teams)</li>
<li><a href="https://zoom.us/signin">Compte Zoom</a></li>
</ul>
<p>Quand c'est fait, cliquez « Fermer le navigateur » dans l'interface.</p>`;

const log = (m: string) => console.log(`${new Date().toLocaleTimeString("fr-FR")} [fdm-join] ${m}`);

const { values, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    name: { type: "string", default: process.env.FDM_AGENT_NAME ?? "Nova" },
    headless: { type: "boolean", default: false },
    browse: { type: "boolean", default: false },
    profile: { type: "string", default: process.env.FDM_BROWSER_PROFILE ?? ".fdm-browser-profile" },
    browser: { type: "string", default: process.env.FDM_BROWSER_PATH },
    help: { type: "boolean", short: "h", default: false },
  },
});

const url = positionals[0];
if (values.help || (!url && !values.browse)) {
  console.log(`Usage : fdm-join <lien de réunion> [--name Nova] [--headless] [--profile DIR] [--browser CHEMIN]
        fdm-join --browse   (navigateur du bot seul, pour y connecter ses comptes)`);
  process.exit(values.help ? 0 : 2);
}
if (!devicesPresent()) {
  console.error(`Périphériques « ${MEETING_SINK} » / « ${AI_MIC} » absents : lancez d'abord \`fdm\`.`);
  process.exit(1);
}
const drivers: Partial<Record<string, Driver>> = { teams, meet, zoom };
const platform = values.browse ? "unknown" : platformOf(url);
const driver = drivers[platform];
if (!driver && !values.browse) {
  log(`⚠️  plateforme non gérée : ${platform === "unknown" ? "lien non reconnu" : platform} (Teams, Google Meet et Zoom le sont)`);
  process.exit(1);
}

const profileDir = resolve(values.profile!);
// Un échec ici (profil verrouillé, navigateur absent…) doit se voir dans le journal de l'interface.
const context = await launchBotBrowser({ profileDir, headless: values.headless!, executablePath: values.browser, displayName: values.name }).catch((e: Error) => {
  const why = /profile appears to be in use/.test(e.message)
    ? `profil du navigateur verrouillé par un autre Chromium (${profileDir}/SingletonLock) : redémarrez le conteneur`
    : e.message.split("\n")[0];
  log(`⚠️  le navigateur du bot ne démarre pas : ${why}`);
  process.exit(1);
});
const page = context.pages()[0] ?? (await context.newPage());
const routing = setInterval(() => routeBrowserStreams(profileDir, log), 3000);
// État des connexions WebRTC de la réunion (voir watchConnections) : utile quand un appel est coupé.
page.on("console", (msg) => {
  const text = msg.text();
  if (text.startsWith(RTC_LOG)) log(text.slice(RTC_LOG.length).trim());
});

let stopping = false;
async function stop(code = 0): Promise<never> {
  if (!stopping) {
    stopping = true;
    clearInterval(routing);
    if (driver) {
      log("le bot quitte la réunion");
      await driver.leave(page, log).catch(() => {});
    } else log("navigateur du bot fermé");
    await context.close().catch(() => {});
  }
  process.exit(code);
}
process.on("SIGINT", () => void stop());
process.on("SIGTERM", () => void stop());
context.on("close", () => void stop()); // fenêtre fermée à la main

if (!driver) {
  // Navigateur seul : une page d'accueil avec les pages de connexion, puis on attend qu'on le ferme.
  await page.setContent(BROWSE_PAGE);
  log("navigateur du bot ouvert : connectez ses comptes dans « Écran du bot » (cochez « Contrôler »)");
  await new Promise(() => {});
}

try {
  await driver!.join(page, url, values.name!, log);
} catch (e) {
  if (e instanceof Refused) {
    log(`⛔ ${e.message} — le bot abandonne cette réunion`);
    await stop(3);
  }
  // On garde la fenêtre : on peut finir à la main (l'audio du bot est déjà branché).
  log(`⚠️  entrée automatique impossible (${(e as Error).message.split("\n")[0]}) — finissez dans la fenêtre du bot`);
}

// Une fois dans l'appel (automatiquement ou à la main), on surveille la fin de la réunion pour
// ne pas laisser tourner la session gpt-live : le service voit le navigateur s'arrêter et la ferme.
await driver!.waitInCall(page);
await driver!.cameraOff(page, log, "dans l'appel");
await driver!.waitForEnd(page, log);
await stop(0);
