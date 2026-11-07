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

## Démarrer (conteneur)

Le bot a sa propre machine : PipeWire, écran virtuel, navigateur et `fdm`. Rien n'est partagé avec votre poste
ni avec votre session Teams.

```bash
docker compose up -d --build
```

Puis ouvrez **http://localhost:8080**. Tout se fait dans l'interface :

| Section | Ce qu'on y règle |
|---|---|
| **Rejoindre une réunion** | Collez le lien et cliquez sur *Rejoindre* ; *Quitter* le fait sortir. État en direct. |
| **Identité et comportement** | Nom affiché, voix, langue, rôle et domaines de compétence, prise de parole spontanée (réservé / normal / actif), **consignes de comportement**, contexte des réunions. En avancé : un **prompt système complet**, et l'aperçu du prompt réellement envoyé. |
| **Clé API OpenAI** | Stockée sur la machine du bot (fichier `0600`), jamais renvoyée à la page. Modèles en avancé. |
| **Outils** | Recherche web, et **serveurs MCP** (voir plus bas). |
| **Journal** | Ce qui se dit, ce qu'il répond, ses appels d'outils, ses erreurs, en direct. |

Les réglages sont appliqués **à la prochaine réunion** : le prompt, la voix et les outils d'une session GPT-Live
sont fixés à son ouverture. Admettez le bot depuis la salle d'attente de Teams. Pour voir son écran (et cliquer si
l'interface de Teams a changé), ouvrez un client VNC sur `localhost:5900`.

**Identité :** par défaut, il rejoint en **invité** sous son nom. Pour un vrai compte (sans salle d'attente),
créez-lui un compte Microsoft et connectez-le une fois dans son navigateur via VNC. Le profil est gardé dans le
volume `bot-profile`.

**Sécurité :** l'interface n'écoute que sur `127.0.0.1` (voir `docker-compose.yml`). Pour l'exposer ailleurs,
définissez `FDM_UI_TOKEN` et ouvrez `http://hôte:8080/?token=<jeton>`.

## Outils : recherche web et MCP

Quand gpt-live délègue une réflexion, le modèle de réflexion (`gpt-5.6-luna` par défaut) peut utiliser :

- **la recherche web** (outil natif d'OpenAI) ;
- **des serveurs MCP**, branchés de deux façons :
  - *Local — commande* : le bot lance la commande (stdio), par ex. `npx -y @modelcontextprotocol/server-filesystem
    /home/pwuser/docs`. Node et `npx` sont dans l'image.
  - *Local — URL* : un serveur MCP HTTP joignable depuis le bot (machine, réseau privé), avec en-têtes
    (`Authorization: Bearer …`).
  - *OpenAI — URL publique* : le serveur est passé tel quel à OpenAI, qui l'appelle directement.

Chaque outil local devient une fonction `serveur__outil` (filtrable avec « outils autorisés »). Les appels
s'exécutent pendant que la voix continue la conversation, puis le résultat est rendu au modèle **comme des données,
jamais comme des consignes**. Un serveur en panne n'empêche pas la réunion : il apparaît en erreur.

## Architecture

```
┌─ conteneur du bot ───────────────────────────────────────────────────────────────────────────┐
│  fdm (Rust) --serve ── interface web :8080 (réglages, lien de réunion, journal)               │
│   ├─ PipeWire : fdm_meeting (il entend) / fdm_ai_mic (il parle)                               │
│   ├─ session gpt-live-1 : flux audio continu ⇄ OpenAI ; délégation → modèle de réflexion      │
│   │                       └─ outils : recherche web, MCP (stdio / HTTP / natif OpenAI)        │
│   └─ fdm-join (TypeScript + Playwright) : Chromium du bot branché sur fdm_meeting / fdm_ai_mic│
└──────────────────────────────────────────────────────────────────────────────────────────────┘
```

- **`core/`, le cœur en Rust (binaire `fdm`).** Audio PipeWire natif, GPT-Live, outils MCP (`rmcp`), interface
  web (`axum`). Envoie le son de la réunion en continu (des zéros quand personne ne parle : gpt-live cale son
  rythme dessus). Si la session se ferme, il en rouvre une avec l'historique de la réunion.
- **`join/`, l'entrée dans la réunion (TypeScript).** Ouvre un Chromium branché sur les nœuds du bot et clique
  sur « Rejoindre ».

## Sans conteneur (Arch)

```bash
sudo pacman -S --needed rust clang pipewire pipewire-pulse wireplumber libpulse nodejs npm
cargo install --path core
(cd join && npm ci && npm run build && npm install -g .) && npx playwright install chromium
fdm --serve                                   # interface sur http://127.0.0.1:8080
fdm --echo-test --join "<lien>"               # ou : vérifier le son sans IA (il répète ce qu'il entend)
```

## Tests

```bash
cargo test --manifest-path core/Cargo.toml
(cd join && npm test)
# avec de vrais serveurs MCP :
FDM_TEST_MCP="python serveur.py" FDM_TEST_MCP_URL="http://127.0.0.1:8931/mcp" cargo test --manifest-path core/Cargo.toml
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
  - confirmation orale avant qu'un outil MCP n'**agisse** (envoyer, modifier), comme dans loquivox ;
  - chargement des outils à la demande (`find_tools`) pour les serveurs qui en exposent des dizaines.
