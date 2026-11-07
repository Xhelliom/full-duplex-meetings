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

  // 4. Dans l'appel quand le bouton « Quitter » apparaît
  try {
    await page.getByRole("button", { name: LEAVE }).first().waitFor({ state: "visible", timeout: 10 * 60_000 });
    log("✅ le bot est dans la réunion");
    return true;
  } catch {
    log("⚠️  toujours pas admis après 10 min");
    return false;
  }
}

/** Attend que le bot soit dans l'appel (entré automatiquement ou à la main via VNC), sans limite. */
export async function waitInCall(page: Page): Promise<void> {
  while (!page.isClosed()) {
    if (await page.getByRole("button", { name: LEAVE }).first().isVisible().catch(() => false)) return;
    await sleep(2_000);
  }
}

const ALONE =/seule? personne (ici|dans (la|cette) réunion)|personne d'autre n'est là|only (one|person) (here|in this meeting)|no one else is here/i;

/** Rend la main quand la réunion est finie pour le bot : il n'y est plus (fin de la réunion,
 * retiré par l'organisateur, réseau perdu…), ou il y est seul depuis `aloneMs`. */
export async function waitForEnd(page: Page, log: (m: string) => void, aloneMs = 5 * 60_000): Promise<void> {
  let goneSince = 0;
  let aloneSince = 0;
  for (;;) {
    await sleep(5_000);
    if (page.isClosed()) return;
    const now = Date.now();
    const inCall = await page.getByRole("button", { name: LEAVE }).first().isVisible().catch(() => false);
    if (inCall) goneSince = 0;
    else if (!goneSince) goneSince = now;
    else if (now - goneSince >= 15_000) {
      log("la réunion est terminée pour le bot (il n'y est plus)");
      return;
    }
    const alone = inCall && (await page.getByText(ALONE).first().isVisible().catch(() => false));
    if (!alone) aloneSince = 0;
    else if (!aloneSince) aloneSince = now;
    else if (now - aloneSince >= aloneMs) {
      log(`seul dans la réunion depuis ${Math.round(aloneMs / 60_000)} min : il s'en va`);
      return;
    }
  }
}

export async function leaveTeams(page: Page): Promise<void> {
  await clickFirst([page.getByRole("button", { name: LEAVE })], 2_000);
}
