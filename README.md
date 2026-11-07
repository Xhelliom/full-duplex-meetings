# full-duplex-meetings

Un **agent vocal IA** (API OpenAI Realtime, `gpt-realtime`) qui **rejoint une réunion Microsoft Teams comme un
participant à part entière** : il entend tout le monde, suit la conversation, et prend la parole quand on
l'interpelle. Full duplex : il écoute en permanence, même pendant qu'il parle, et se tait si on lui coupe la parole.

Cible : **Linux (Arch) + PipeWire** (via `pipewire-pulse`), Wayland ou X11.

## Comment ça marche

```
               ┌──────────── navigateur Chromium dédié au bot (Playwright) ────────────┐
               │  Teams web, connecté comme invité « Assistant IA »                     │
               │    haut-parleurs ─► fdm_meeting (sink virtuel)                         │
               │    micro         ◄─ fdm_ai_mic  (source virtuelle)                     │
               └────────────────────────────────────────────────────────────────────────┘
 fdm_meeting.monitor ─► parec ─► [fdm] ─► WebSocket OpenAI Realtime ─► [fdm] ─► pacat ─► fdm_ai_voice ─► fdm_ai_mic
```

- Les périphériques virtuels sont créés au démarrage et supprimés à l'arrêt (`pactl load-module …`).
- Le navigateur du bot est lancé avec `PULSE_SINK`/`PULSE_SOURCE` ; un filet de sécurité redéplace ses flux
  audio toutes les 3 s si besoin. **Votre propre Teams et votre son ne sont pas touchés** : vous pouvez être dans la
  même réunion depuis votre client habituel.
- Teams n'envoie jamais à un participant sa propre voix → pas d'écho de l'IA sur elle-même.

### Plusieurs personnes en face
L'IA reçoit un seul flux mixé. Pour qu'elle se comporte bien en réunion :
- **Mode `name` (défaut)** : toute la réunion est envoyée au modèle (il garde le contexte), mais il **ne répond que
  quand on l'appelle** par son nom ou un alias (« Nova, tu peux résumer ? »), détecté sur la transcription. Ensuite il
  reste « en conversation » `FDM_FOLLOWUP_SECONDS` secondes (on peut enchaîner sans répéter son nom).
- **Mode `always`** : répond à chaque tour de parole (tête-à-tête).
- Les instructions système lui expliquent qu'il y a plusieurs interlocuteurs, de les distinguer par la voix et les
  prénoms, de s'adresser aux gens par leur prénom, et de rester bref (oral).
- Interruption (*barge-in*) : dès que quelqu'un parle, la voix de l'IA est coupée instantanément et le modèle est
  informé de ce qui a réellement été entendu (`conversation.item.truncate`).

## Installation (Arch)

```bash
sudo pacman -S --needed pipewire pipewire-pulse libpulse python
git clone <ce dépôt> && cd full-duplex-meetings
python -m venv .venv && source .venv/bin/activate
pip install -e .
python -m playwright install chromium     # ou FDM_BROWSER_PATH=/usr/bin/chromium
cp .env.example .env && $EDITOR .env      # OPENAI_API_KEY, nom de l'agent…
fdm doctor
```

## Utilisation

### 1. Le bot rejoint la réunion tout seul (nouvelle personne dans la réunion)
```bash
fdm join "https://teams.microsoft.com/l/meetup-join/..." --name "Nova" --context "Réunion projet X, ordre du jour : …"
```
Une fenêtre Chromium s'ouvre : « Continuer sur ce navigateur » → saisit le nom → coupe la caméra → « Rejoindre
maintenant ». L'organisateur l'admet depuis la salle d'attente, et c'est parti. Si l'interface Teams a changé et
qu'une étape bloque, **cliquez simplement dans la fenêtre** : le pont audio tourne déjà.
Fermer la fenêtre ou `Ctrl+C` arrête tout et nettoie les périphériques.

Astuce : le profil du navigateur est conservé (`.fdm-browser-profile/`). Vous pouvez vous y connecter une fois avec un
compte Microsoft dédié au bot (ex. `assistant@votreboite.com`) pour éviter la salle d'attente des invités.

### 2. Vous branchez vous-même un client Teams
```bash
fdm listen
```
Puis dans ce client Teams (ex. `teams-for-linux` sur un 2ᵉ compte) : *Paramètres → Périphériques* → haut-parleur
**FDM-Reunion**, micro **FDM-Micro-IA**. ⚠️ Si vous le faites dans *votre* Teams, l'IA parlera **à votre place**.

### Options utiles
| Option | Effet |
|---|---|
| `--name` | Nom affiché et nom auquel elle réagit |
| `--reply-mode name\|always` | Voir ci-dessus |
| `--voice marin` | Voix OpenAI (`marin`, `cedar`, `alloy`, …) |
| `--language fr` | Langue de transcription |
| `--context "…"` | Rôle / ordre du jour / infos |
| `--no-barge-in` | Ne se tait pas quand on parle par-dessus |
| `--monitor` | Vous entendez aussi ce que le bot entend et dit (debug) |
| `-v` | Logs détaillés |

`fdm audio-teardown` supprime des périphériques laissés par une session plantée.

## Tests
```bash
pip install -e '.[dev]' && pytest
```

## Pistes suivantes
- **Qui parle ?** Lire les sous-titres en direct de Teams (DOM de la page du bot) qui affichent le nom de chaque
  orateur, et les injecter au modèle (`[Alice] …`) pour une attribution fiable.
- Outils (function calling) : prise de notes, compte-rendu en fin de réunion, recherche dans des docs.
- Auto-join depuis le calendrier (Microsoft Graph) ; Google Meet / Zoom (même principe audio).
