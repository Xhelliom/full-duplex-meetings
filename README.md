# full-duplex-meetings

Un **participant IA** dans vos réunions Microsoft Teams, avec **OpenAI GPT-Live** (`gpt-live-1`), le modèle vocal
full duplex. Il rejoint la réunion avec sa propre identité, écoute en permanence, et prend part à la conversation
naturellement, comme un collègue :

- il répond quand on s'adresse à lui, avec ou sans son nom, et suit l'échange tant qu'il dure ;
- il se tait quand les autres se parlent entre eux ou qu'on se tourne vers quelqu'un d'autre ;
- il intervient de lui-même quand la discussion touche son domaine (`FDM_EXPERTISE`, `FDM_PROACTIVITY`) ;
- il parle aussi peu ou aussi longtemps que nécessaire, et s'arrête dès qu'on lui coupe la parole.

Aucune règle côté client : c'est le modèle qui gère le rythme de la conversation. Le code ne fait que le transport
audio, et lui donne ses consignes (rôle, quand parler, acquiescements, interruptions, délégation), structurées
comme le recommande le guide de prompting GPT-Live.

## Architecture

```
┌─ conteneur du bot (ou votre PC) ─────────────────────────────────────────────────────────────┐
│  PipeWire                                                                                    │
│   fdm_meeting : haut-parleur du bot ──► fdm (Rust) ──► gpt-live-1 (flux continu, 24 kHz)     │
│   fdm_ai_mic  : micro du bot        ◄── fdm (Rust) ◄── sa voix (temps réel, 100 ms)          │
│        ▲  ▼                                                                                  │
│  Chromium du bot (fdm-join, TypeScript + Playwright) : rejoint Teams sous le nom « Nova »    │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
```

- **`core/`, le cœur en Rust (binaire `fdm`).** Il crée les deux nœuds PipeWire, qui disparaissent à l'arrêt,
  envoie le son de la réunion en continu (des zéros quand personne ne parle : gpt-live cale son rythme dessus) et
  joue sa voix avec une petite réserve anti-gigue. Si la session se ferme, il en rouvre une avec l'historique de
  la réunion.
- **`join/`, l'entrée dans la réunion (TypeScript, Playwright).** Il ouvre un Chromium branché sur ces nœuds et
  clique sur « Rejoindre ». `fdm --join <lien>` le lance lui-même.

## Le plus simple : un conteneur

Le bot a sa propre machine (PipeWire, écran virtuel, navigateur) : rien de partagé avec votre poste ni votre
session Teams.

```bash
cp .env.example .env && $EDITOR .env          # OPENAI_API_KEY, nom, domaines…
docker compose build
FDM_JOIN="https://teams.microsoft.com/l/meetup-join/..." docker compose run --rm bot
```

Ensuite, admettez « Nova » depuis la salle d'attente de Teams. Pour voir l'écran du bot (et cliquer si l'interface
de Teams a changé), ouvrez un client VNC sur `localhost:5900`. `Ctrl+C` ou `docker stop` : il quitte la réunion
et ferme proprement sa session.

**Identité :** par défaut, il rejoint en **invité** sous son nom. Pour un vrai compte (sans salle d'attente),
créez un compte Microsoft pour le bot et connectez-le une fois dans son navigateur, via VNC. Le profil est gardé
dans le volume `bot-profile`.

## Sans conteneur (Arch)

```bash
sudo pacman -S --needed rust clang pipewire pipewire-pulse wireplumber libpulse nodejs npm
cargo install --path core                              # binaire fdm
(cd join && npm ci && npm run build && npm install -g .) && npx playwright install chromium
fdm --echo-test --join "<lien>"                        # vérifier le son sans IA : il répète ce qu'il entend
fdm --join "<lien>"
```

Sans pilotage du navigateur, lancez `fdm`, puis choisissez *FDM Réunion* comme haut-parleur et *FDM Micro IA* comme
micro dans un client Teams **dédié au bot**. Pas dans le vôtre, sinon il parle à votre place.

Toutes les options : `fdm --help` (chacune a son équivalent `FDM_*` dans `.env`).

## Tests

```bash
cargo test --manifest-path core/Cargo.toml
(cd join && npm test)
```

Vérifié aussi de bout en bout, avec un faux serveur `gpt-live-1` :
- dans PipeWire sur le poste : il entend la réunion, sa voix arrive dans `fdm_ai_mic`, le flux montant reste
  continu, et un vrai Chromium joue dans `fdm_meeting` et écoute `fdm_ai_mic` ;
- dans le conteneur : tout démarre, l'aller-retour audio fonctionne, et `docker stop` est propre.

## Notes

- `gpt-live-1.1` est sorti dans ChatGPT, mais **pas encore dans l'API**. Ce sera `FDM_MODEL=gpt-live-1.1`.
- Coût : environ 0,05 $ par minute de session, plus le modèle de délégation.
- **Pistes :**
  - savoir qui parle : les sous-titres en direct de Teams affichent le nom de chaque orateur, à transmettre au
    modèle en pensée silencieuse ;
  - Google Meet (`join/src/platform.ts` le reconnaît déjà) ;
  - délégation `client` avec nos propres outils (notes, compte-rendu).
