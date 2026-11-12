// Petits outils partagés par les pilotes de plateforme (Teams, Meet).
import type { Locator, Page } from "playwright";

export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Clique le premier élément visible parmi `locators`, en réessayant jusqu'à `timeoutMs`. */
export async function clickFirst(locators: Locator[], timeoutMs: number): Promise<boolean> {
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

/** Où en est la page, pour les journaux : titre et adresse sans paramètres. */
export async function where(page: Page): Promise<string> {
  const u = new URL(page.url());
  return `page « ${await page.title().catch(() => "?")} », ${u.host}${u.pathname}`;
}

/** Ce qu'un pilote de plateforme sait faire ; main.ts l'enchaîne. */
export interface Driver {
  /** Ouvre le lien, saisit le nom, coupe la caméra, demande à entrer ; true une fois dans l'appel. */
  join(page: Page, url: string, displayName: string, log: (m: string) => void): Promise<boolean>;
  /** Rend la main dès qu'il est dans l'appel (entré seul ou à la main via VNC). */
  waitInCall(page: Page): Promise<void>;
  cameraOff(page: Page, log: (m: string) => void, when: string): Promise<boolean>;
  /** Rend la main quand la réunion est finie pour lui (raison journalisée). */
  waitForEnd(page: Page, log: (m: string) => void): Promise<void>;
  /** Raccroche vraiment (ou annule la demande d'entrée). */
  leave(page: Page, log: (m: string) => void): Promise<void>;
}

/** La plateforme refuse l'entrée (navigateur refusé, demande rejetée, lien invalide) : inutile
 * d'attendre, le bot abandonne la réunion (et la session gpt-live, facturée, avec). */
export class Refused extends Error {}

/** Texte visible correspondant à `pattern`, ou null. */
export async function visibleText(page: Page, pattern: RegExp): Promise<string | null> {
  const el = page.getByText(pattern).first();
  if (!(await el.isVisible().catch(() => false))) return null;
  return ((await el.textContent().catch(() => "")) ?? "").trim().slice(0, 160);
}
