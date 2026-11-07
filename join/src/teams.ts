// Entrée dans une réunion Microsoft Teams (web), en invité ou avec le compte connecté dans le profil.
// L'interface de Teams change souvent : chaque étape est « au mieux », et si l'une échoue on peut
// cliquer soi-même dans la fenêtre — l'audio du bot est déjà branché.
import type { Locator, Page } from "playwright";

const CONTINUE_IN_BROWSER = /continue on this browser|continuer sur ce navigateur|join on the web|rejoindre sur le web/i;
const JOIN_NOW = /^\s*(join now|rejoindre maintenant|join|rejoindre)\s*$/i;
const LEAVE = /^(leave|quitter)/i;
const NAME_FIELD = /name|nom/i;

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function clickFirst(locators: Locator[], timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    for (const loc of locators) {
      try {
        const el = loc.first();
        if (await el.isVisible()) {
          await el.click();
          return true;
        }
      } catch {
        /* l'interface bouge encore */
      }
    }
    await sleep(500);
  }
  return false;
}

export async function joinTeams(page: Page, url: string, displayName: string, log: (m: string) => void): Promise<boolean> {
  log("ouverture du lien de réunion");
  await page.goto(url, { waitUntil: "domcontentloaded" });

  // 1. « Comment voulez-vous rejoindre ? » -> dans le navigateur
  if (
    await clickFirst(
      [page.locator('[data-tid="joinOnWeb"]'), page.getByRole("button", { name: CONTINUE_IN_BROWSER }), page.getByText(CONTINUE_IN_BROWSER)],
      20_000,
    )
  ) {
    log("« Continuer sur ce navigateur »");
  }

  // 2. Écran de préparation : son nom (invité), caméra coupée
  let nameBox: Locator | undefined;
  for (const loc of [page.locator('[data-tid="prejoin-display-name-input"]'), page.getByPlaceholder(NAME_FIELD)]) {
    try {
      await loc.first().waitFor({ state: "visible", timeout: 15_000 });
      nameBox = loc.first();
      break;
    } catch {
      /* champ suivant */
    }
  }
  if (nameBox) {
    await nameBox.fill(displayName);
    log(`nom saisi : ${displayName}`);
  } else {
    log("pas de champ nom (profil connecté à un compte ?)");
  }
  try {
    const camera = page.locator('[data-tid="toggle-video"]').first();
    if ((await camera.isVisible()) && (await camera.getAttribute("aria-checked")) === "true") {
      await camera.click();
      log("caméra coupée");
    }
  } catch {
    /* pas de bouton caméra */
  }

  // 3. Rejoindre
  if (!(await clickFirst([page.locator('[data-tid="prejoin-join-button"]'), page.getByRole("button", { name: JOIN_NOW })], 30_000))) {
    log("⚠️  bouton « Rejoindre maintenant » introuvable : cliquez-le dans la fenêtre du bot");
    return false;
  }
  log("demande envoyée — l'organisateur doit peut-être l'admettre depuis la salle d'attente");

  // 4. Dans l'appel quand les contrôles d'appel apparaissent (même masqués)
  await waitInCallFor(page, 10 * 60_000);
  if (page.isClosed()) return false; // arrêt demandé pendant l'attente
  if (await callControlsPresent(page)) {
    log("✅ le bot est dans la réunion");
    return true;
  }
  log("⚠️  toujours pas admis après 10 min");
  return false;
}

/** Attend que le bot soit dans l'appel (entré automatiquement ou à la main via VNC), sans limite. */
export async function waitInCall(page: Page): Promise<void> {
  while (!page.isClosed()) {
    if (await callControlsPresent(page)) return;
    await sleep(2_000);
  }
}

const ALONE =/seule? personne (ici|dans (la|cette) réunion)|personne d'autre n'est là|only (one|person) (here|in this meeting)|no one else is here/i;

/** Appel coupé par Teams, qu'on peut reprendre (« Rejoindre l'appel »). */
const DROPPED = /appel a été coupé|appel a été interrompu|call (was|has been) dropped|we lost (the|your) connection|call dropped/i;
const REJOIN = /^\s*(rejoindre l'appel|rejoin( call)?|rejoindre)\s*$/i;
const MAX_REJOINS = 3;

async function waitInCallFor(page: Page, ms: number): Promise<void> {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline && !page.isClosed() && !(await callControlsPresent(page))) await sleep(2_000);
}

/** Ce que Teams affiche quand on n'est plus dans l'appel. */
const ENDED = /vous avez quitté|la réunion (est terminée|a pris fin|s'est terminée)|vous avez été (retiré|supprimé|exclu)|you('ve| have) (left|been removed)|(the )?meeting (has )?ended|call ended|rejoindre à nouveau|^\s*rejoin\s*$/i;
/** Sans signe de fin explicite, l'absence des contrôles d'appel ne suffit qu'au-delà de ce délai. */
const NO_CONTROLS_MS = 90_000;

/** Les contrôles d'appel sont-ils dans la page ? Teams masque sa barre d'outils quand la souris ne
 * bouge pas : on regarde la présence du bouton « Quitter » dans la page, pas sa visibilité. */
async function callControlsPresent(page: Page): Promise<boolean> {
  for (const loc of [page.getByRole("button", { name: LEAVE, includeHidden: true }), page.locator('[data-tid="hangup-main-btn"], #hangup-button')]) {
    if ((await loc.count().catch(() => 0)) > 0) return true;
  }
  return false;
}

/** Rend la main quand la réunion est finie pour le bot : Teams l'affiche (fin de la réunion, retiré par
 * l'organisateur…), ses contrôles d'appel ont disparu depuis longtemps, ou il est seul depuis `aloneMs`.
 * Chaque départ est journalisé avec sa raison, pour pouvoir corriger une fausse détection. */
export async function waitForEnd(page: Page, log: (m: string) => void, aloneMs = 5 * 60_000): Promise<void> {
  let goneSince = 0;
  let endedSeen = 0;
  let aloneSince = 0;
  let rejoins = 0;
  const where = async () => {
    const u = new URL(page.url());
    return `page « ${await page.title().catch(() => "?")} », ${u.host}${u.pathname}`;
  };
  for (;;) {
    await sleep(5_000);
    if (page.isClosed()) {
      log("départ : la fenêtre de la réunion a été fermée");
      return;
    }
    const now = Date.now();

    // 0. Appel coupé par Teams (« Oh oh… votre appel a été coupé ») : on rejoint, quelques fois.
    if (await page.getByText(DROPPED).first().isVisible().catch(() => false)) {
      if (rejoins >= MAX_REJOINS) {
        log(`départ : Teams a coupé l'appel ${rejoins + 1} fois (${await where()})`);
        return;
      }
      rejoins++;
      log(`⚠️  Teams a coupé l'appel : il rejoint (essai ${rejoins}/${MAX_REJOINS})`);
      if (await clickFirst([page.getByRole("button", { name: REJOIN })], 5_000)) {
        await clickFirst([page.locator('[data-tid="prejoin-join-button"]'), page.getByRole("button", { name: JOIN_NOW })], 20_000);
        goneSince = 0;
        await waitInCallFor(page, 60_000);
        log((await callControlsPresent(page)) ? "de retour dans l'appel" : "pas encore de retour dans l'appel");
      } else log("bouton « Rejoindre l'appel » introuvable");
      continue;
    }

    // 1. Teams dit explicitement que c'est fini (vu deux fois de suite, pour éviter un message fugace).
    const ended = page.getByText(ENDED).first();
    if (await ended.isVisible().catch(() => false)) {
      if (++endedSeen >= 2) {
        const text = ((await ended.textContent().catch(() => "")) ?? "").trim().slice(0, 120);
        log(`départ : Teams affiche « ${text} » (${await where()})`);
        return;
      }
    } else endedSeen = 0;

    // 2. Plus aucun contrôle d'appel dans la page depuis longtemps.
    const inCall = await callControlsPresent(page);
    if (inCall) {
      if (goneSince) log("contrôles d'appel revenus : il est toujours dans la réunion");
      goneSince = 0;
    } else if (!goneSince) {
      goneSince = now;
      log(`contrôles d'appel introuvables (${await where()}) : il part si ça dure ${NO_CONTROLS_MS / 1000} s`);
    } else if (now - goneSince >= NO_CONTROLS_MS) {
      log(`départ : plus de contrôles d'appel depuis ${NO_CONTROLS_MS / 1000} s (${await where()})`);
      return;
    }

    // 3. Seul dans la réunion.
    const alone = inCall && (await page.getByText(ALONE).first().isVisible().catch(() => false));
    if (!alone) aloneSince = 0;
    else if (!aloneSince) aloneSince = now;
    else if (now - aloneSince >= aloneMs) {
      log(`départ : seul dans la réunion depuis ${Math.round(aloneMs / 60_000)} min`);
      return;
    }
  }
}

/** Raccroche vraiment : sans ça, fermer le navigateur laisse un participant « fantôme » dans la
 * réunion jusqu'à expiration côté Teams. La barre d'outils est souvent masquée : on bouge la souris
 * pour la faire revenir, et on clique le bouton même s'il est encore caché. */
export async function leaveTeams(page: Page, log: (m: string) => void): Promise<void> {
  if (page.isClosed()) return;
  await page.mouse.move(600, 380).catch(() => {});
  await page.mouse.move(640, 420).catch(() => {});
  for (const loc of [page.locator('[data-tid="hangup-main-btn"], #hangup-button'), page.getByRole("button", { name: LEAVE, includeHidden: true })]) {
    const button = loc.first();
    if ((await button.count().catch(() => 0)) === 0) continue;
    try {
      await button.click({ force: true, timeout: 3_000 });
      await sleep(1_500); // le temps que Teams prévienne les autres participants
      log("raccroché");
      return;
    } catch {
      /* bouton suivant */
    }
  }
  // Encore dans la salle d'attente : on annule la demande, sinon l'organisateur la voit toujours.
  const cancel = page.getByRole("button", { name: /^\s*(annuler|cancel)\s*$/i, includeHidden: true }).first();
  if ((await cancel.count().catch(() => 0)) > 0) {
    try {
      await cancel.click({ force: true, timeout: 3_000 });
      await sleep(1_500);
      log("demande d'entrée annulée (il était en salle d'attente)");
      return;
    } catch {
      /* rien à faire de plus */
    }
  }
  log("pas de bouton pour raccrocher ni annuler (déjà sorti de l'appel ?)");
}
