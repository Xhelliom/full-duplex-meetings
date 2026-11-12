// Entrée dans une réunion Google Meet (web), en invité ou avec le compte Google connecté dans le
// profil. Comme pour Teams, chaque étape est « au mieux » : si l'interface a changé, on finit à la
// main dans la fenêtre du bot (VNC), l'audio est déjà branché.
//
// En invité, Google refuse souvent les navigateurs automatisés (« Vous ne pouvez pas participer à cet
// appel vidéo ») : connecter une fois un compte Google dans le profil du bot règle le problème.
import type { Page } from "playwright";

import { clickFirst, Refused, sleep, visibleText, where, type Driver } from "./util.js";

const NAME_FIELD = /your name|votre nom|nom/i;
/** « Participer » (compte de l'organisation) ou « Demander à participer » (invité). */
const JOIN = /^\s*(join now|ask to join|participer( à la réunion)?|demander à participer|rejoindre( maintenant)?)\s*$/i;
const LEAVE = /^(leave call|quitter l'appel|raccrocher)/i;
const CAMERA_OFF = /^(turn off camera|désactiver la caméra)/i;
const DISMISS = /^\s*(got it|ok|j'ai compris|fermer|dismiss|close)\s*$/i;
/** Plus dans l'appel : départ, exclusion, fin, retour à l'accueil. */
const ENDED = /you left the meeting|vous avez quitté la réunion|you('ve| have) been removed|vous avez été (exclu|retiré)|the call has ended|l'appel est terminé|la réunion est terminée|return to home screen|revenir à l'écran d'accueil/i;
/** Entrée refusée ou impossible. */
const REFUSED = /can't join this (video )?call|check your meeting code|vérifiez (le|votre) code|invalid video call name|nom d'appel vidéo (non valide|incorrect)|ne pouvez pas participer|denied your request|a refusé votre demande|no one responded|personne n'a répondu/i;
const ALONE = /you're the only one here|vous êtes (le seul|la seule|seul|seule) (participant|personne|ici)/i;
const NO_CONTROLS_MS = 90_000;

async function inCall(page: Page): Promise<boolean> {
  return (await page.getByRole("button", { name: LEAVE, includeHidden: true }).count().catch(() => 0)) > 0;
}

const refused = (page: Page) => visibleText(page, REFUSED);

async function joinMeet(page: Page, url: string, displayName: string, log: (m: string) => void): Promise<boolean> {
  log("ouverture du lien Google Meet");
  await page.goto(url, { waitUntil: "domcontentloaded" });
  await clickFirst([page.getByRole("button", { name: DISMISS })], 5_000); // bulles d'aide éventuelles

  // 1. Nom (invité seulement : avec un compte Google, Meet prend celui du compte)
  const nameBox = page.getByRole("textbox", { name: NAME_FIELD }).or(page.getByPlaceholder(NAME_FIELD)).first();
  try {
    await nameBox.waitFor({ state: "visible", timeout: 15_000 });
    await nameBox.fill(displayName);
    log(`nom saisi : ${displayName}`);
  } catch {
    const why = await refused(page);
    if (why) throw new Refused(`Google Meet refuse l'entrée : « ${why} » — connectez un compte Google au bot (bouton « Ouvrir le navigateur du bot »)`);
    log("pas de champ nom (profil connecté à un compte Google ?)");
  }
  await cameraOff(page, log, "avant d'entrer");

  // 2. Participer / demander à participer
  if (!(await clickFirst([page.getByRole("button", { name: JOIN })], 30_000))) {
    log("⚠️  bouton « Participer » introuvable : cliquez-le dans la fenêtre du bot");
    return false;
  }
  log("demande envoyée — un participant doit peut-être l'admettre");

  // 3. Admis quand le bouton « Quitter l'appel » apparaît ; refus détecté en route.
  const deadline = Date.now() + 10 * 60_000;
  while (Date.now() < deadline && !page.isClosed()) {
    if (await inCall(page)) {
      log("✅ le bot est dans la réunion");
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
  await page.mouse.move(640, 700).catch(() => {}); // la barre d'outils se masque
  await button.click({ force: true, timeout: 3_000 }).catch(() => {});
  log(`caméra coupée (${when})`);
  return true;
}

async function waitInCall(page: Page): Promise<void> {
  while (!page.isClosed() && !(await inCall(page))) await sleep(2_000);
}

async function waitForEnd(page: Page, log: (m: string) => void, aloneMs = 5 * 60_000): Promise<void> {
  let goneSince = 0;
  let endedSeen = 0;
  let aloneSince = 0;
  for (;;) {
    await sleep(5_000);
    if (page.isClosed()) {
      log("départ : la fenêtre de la réunion a été fermée");
      return;
    }
    const now = Date.now();

    const ended = page.getByText(ENDED).first();
    if (await ended.isVisible().catch(() => false)) {
      if (++endedSeen >= 2) {
        const text = ((await ended.textContent().catch(() => "")) ?? "").trim().slice(0, 120);
        log(`départ : Meet affiche « ${text} » (${await where(page)})`);
        return;
      }
    } else endedSeen = 0;

    const present = await inCall(page);
    if (present) {
      if (goneSince) log("contrôles d'appel revenus : il est toujours dans la réunion");
      goneSince = 0;
    } else if (!goneSince) {
      goneSince = now;
      log(`contrôles d'appel introuvables (${await where(page)}) : il part si ça dure ${NO_CONTROLS_MS / 1000} s`);
    } else if (now - goneSince >= NO_CONTROLS_MS) {
      log(`départ : plus de contrôles d'appel depuis ${NO_CONTROLS_MS / 1000} s (${await where(page)})`);
      return;
    }

    const alone = present && (await page.getByText(ALONE).first().isVisible().catch(() => false));
    if (!alone) aloneSince = 0;
    else if (!aloneSince) aloneSince = now;
    else if (now - aloneSince >= aloneMs) {
      log(`départ : seul dans la réunion depuis ${Math.round(aloneMs / 60_000)} min`);
      return;
    }
  }
}

/** Raccroche en vérifiant chaque tentative ; en salle d'attente, fermer la page retire la demande. */
async function leaveMeet(page: Page, log: (m: string) => void): Promise<void> {
  if (page.isClosed()) return;
  if (await inCall(page)) {
    const button = page.getByRole("button", { name: LEAVE, includeHidden: true }).first();
    const attempts: [string, () => Promise<unknown>][] = [
      ["clic sur Quitter l'appel", async () => { await page.mouse.move(640, 700); await sleep(300); await button.click({ timeout: 3_000 }); }],
      ["clic forcé sur Quitter l'appel", () => button.click({ force: true, timeout: 3_000 })],
    ];
    for (const [how, act] of attempts) {
      try {
        await act();
      } catch {
        continue;
      }
      const until = Date.now() + 6_000;
      while (Date.now() < until && !page.isClosed() && (await inCall(page))) await sleep(300);
      if (page.isClosed() || !(await inCall(page))) {
        log(`raccroché (${how})`);
        await sleep(1_500);
        return;
      }
      log(`${how} : toujours dans l'appel, autre méthode`);
    }
    log("⚠️ impossible de raccrocher proprement : fermeture de la page");
  } else {
    log("pas encore dans l'appel : fermeture de la page (retire la demande d'entrée)");
  }
  await page.close({ runBeforeUnload: true }).catch(() => {});
  await sleep(2_000);
}

export const meet: Driver = { join: joinMeet, waitInCall, cameraOff, waitForEnd: (p, l) => waitForEnd(p, l), leave: leaveMeet };
