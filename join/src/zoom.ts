// Entrée dans une réunion Zoom par le client web (« Rejoindre depuis votre navigateur »), sans
// application. Au mieux, comme les autres plateformes : si l'interface a changé, on finit à la main
// dans la fenêtre du bot (VNC).
//
// Certaines réunions exigent un compte Zoom ou un CAPTCHA pour le client web : il faut alors le
// faire une fois dans la fenêtre du bot.
import type { Page } from "playwright";

import { clickFirst, Refused, sleep, visibleText, where, type Driver } from "./util.js";

const JOIN = /^\s*(join|rejoindre|participer)\s*$/i;
const JOIN_AUDIO = /join audio by computer|rejoindre l'audio (par|depuis l')ordinateur|join with computer audio|audio de l'ordinateur/i;
const LEAVE = /^\s*(leave|quitter)\s*$/i;
const LEAVE_CONFIRM = /^\s*(leave meeting|quitter la réunion)\s*$/i;
const CAMERA_OFF = /^\s*(stop video|arrêter la vidéo|désactiver la vidéo)\s*$/i;
const DISMISS = /^\s*(accept( cookies)?|accepter( les cookies)?|got it|ok|j'ai compris)\s*$/i;
const ENDED = /meeting has been ended|réunion a été terminée|you have been removed|vous avez été (exclu|retiré)|this meeting has ended|la réunion est terminée/i;
const REFUSED = /invalid meeting id|numéro de réunion non valide|meeting (passcode|password) (is )?(wrong|incorrect)|code secret (est )?incorrect|sign in to join|connectez-vous pour rejoindre/i;
const NO_CONTROLS_MS = 90_000;

/** Lien d'invitation (`…zoom.us/j/123?pwd=…`) -> client web (`…zoom.us/wc/join/123?pwd=…`). */
export function webClientUrl(link: string): string {
  const u = new URL(link);
  const id = u.pathname.match(/\/(?:j|s|w|wc\/join|wc)\/(\d+)/)?.[1];
  if (!id) return link;
  const out = new URL(`https://${u.hostname}/wc/join/${id}`);
  const pwd = u.searchParams.get("pwd");
  if (pwd) out.searchParams.set("pwd", pwd);
  return out.toString();
}

async function inCall(page: Page): Promise<boolean> {
  return (await page.getByRole("button", { name: LEAVE, includeHidden: true }).count().catch(() => 0)) > 0;
}

const refused = (page: Page) => visibleText(page, REFUSED);

async function joinZoom(page: Page, url: string, displayName: string, log: (m: string) => void): Promise<boolean> {
  const web = webClientUrl(url);
  log("ouverture de la réunion Zoom dans le client web");
  await page.goto(web, { waitUntil: "domcontentloaded" });
  await clickFirst([page.getByRole("button", { name: DISMISS })], 5_000); // bandeau cookies

  // 1. Nom
  const nameBox = page.locator("#input-for-name").or(page.getByRole("textbox", { name: /your name|votre nom|nom/i })).first();
  try {
    await nameBox.waitFor({ state: "visible", timeout: 20_000 });
    await nameBox.fill(displayName);
    log(`nom saisi : ${displayName}`);
  } catch {
    const why = await refused(page);
    if (why) throw new Refused(`Zoom refuse l'entrée : « ${why} »`);
    log("pas de champ nom (compte Zoom connecté, ou CAPTCHA à faire dans la fenêtre du bot)");
  }
  await cameraOff(page, log, "avant d'entrer");

  // 2. Rejoindre
  if (!(await clickFirst([page.locator(".preview-join-button"), page.getByRole("button", { name: JOIN })], 30_000))) {
    log("⚠️  bouton « Rejoindre » introuvable : cliquez-le dans la fenêtre du bot");
    return false;
  }
  log("demande envoyée — l'hôte doit peut-être l'admettre depuis la salle d'attente");

  // 3. Dans la réunion quand « Quitter » apparaît ; puis brancher l'audio de l'ordinateur.
  const deadline = Date.now() + 10 * 60_000;
  while (Date.now() < deadline && !page.isClosed()) {
    if (await inCall(page)) {
      log("✅ le bot est dans la réunion");
      if (await clickFirst([page.getByRole("button", { name: JOIN_AUDIO })], 10_000)) log("audio de l'ordinateur branché");
      return true;
    }
    const why = await refused(page);
    if (why) throw new Refused(`entrée refusée : « ${why} »`);
    await sleep(2_000);
  }
  if (!page.isClosed()) log("⚠️  toujours pas admis après 10 min");
  return false;
}

async function cameraOff(page: Page, log: (m: string) => void, when: string): Promise<boolean> {
  const button = page.getByRole("button", { name: CAMERA_OFF, includeHidden: true }).first();
  if ((await button.count().catch(() => 0)) === 0) return false;
  await page.mouse.move(640, 700).catch(() => {});
  await button.click({ force: true, timeout: 3_000 }).catch(() => {});
  log(`caméra coupée (${when})`);
  return true;
}

async function waitInCall(page: Page): Promise<void> {
  while (!page.isClosed() && !(await inCall(page))) await sleep(2_000);
}

async function waitForEnd(page: Page, log: (m: string) => void): Promise<void> {
  let goneSince = 0;
  let endedSeen = 0;
  for (;;) {
    await sleep(5_000);
    if (page.isClosed()) {
      log("départ : la fenêtre de la réunion a été fermée");
      return;
    }
    const ended = page.getByText(ENDED).first();
    if (await ended.isVisible().catch(() => false)) {
      if (++endedSeen >= 2) {
        const text = ((await ended.textContent().catch(() => "")) ?? "").trim().slice(0, 120);
        log(`départ : Zoom affiche « ${text} » (${await where(page)})`);
        return;
      }
    } else endedSeen = 0;

    if (await inCall(page)) {
      if (goneSince) log("contrôles d'appel revenus : il est toujours dans la réunion");
      goneSince = 0;
    } else if (!goneSince) {
      goneSince = Date.now();
      log(`contrôles d'appel introuvables (${await where(page)}) : il part si ça dure ${NO_CONTROLS_MS / 1000} s`);
    } else if (Date.now() - goneSince >= NO_CONTROLS_MS) {
      log(`départ : plus de contrôles d'appel depuis ${NO_CONTROLS_MS / 1000} s (${await where(page)})`);
      return;
    }
  }
}

/** « Quitter » puis « Quitter la réunion » (Zoom demande confirmation), vérifié. */
async function leaveZoom(page: Page, log: (m: string) => void): Promise<void> {
  if (page.isClosed()) return;
  if (await inCall(page)) {
    for (const force of [false, true]) {
      try {
        await page.mouse.move(640, 700).catch(() => {});
        await sleep(300);
        await page.getByRole("button", { name: LEAVE, includeHidden: true }).first().click({ force, timeout: 3_000 });
        await clickFirst([page.getByRole("button", { name: LEAVE_CONFIRM })], 3_000);
      } catch {
        continue;
      }
      const until = Date.now() + 6_000;
      while (Date.now() < until && !page.isClosed() && (await inCall(page))) await sleep(300);
      if (page.isClosed() || !(await inCall(page))) {
        log(`raccroché${force ? " (clic forcé)" : ""}`);
        await sleep(1_500);
        return;
      }
    }
    log("⚠️ impossible de raccrocher proprement : fermeture de la page");
  } else {
    log("pas encore dans la réunion : fermeture de la page (retire la demande d'entrée)");
  }
  await page.close({ runBeforeUnload: true }).catch(() => {});
  await sleep(2_000);
}

export const zoom: Driver = { join: joinZoom, waitInCall, cameraOff, waitForEnd, leave: leaveZoom };
