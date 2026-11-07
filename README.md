# full-duplex-meetings

Un **collègue IA dans vos réunions Microsoft Teams**. Il rejoint la réunion comme un participant à part entière,
entend tout le monde, et prend la parole **quand on l'appelle par son prénom, son surnom ou son nom**, comme on
interpelle un humain. Ensuite il reste dans l'échange : les questions suivantes n'ont pas besoin de son nom, il
parle aussi peu ou aussi longtemps qu'il le juge utile, et on lui coupe la parole si besoin.

Linux (Arch) + PipeWire. Cœur en **Rust** (audio natif PipeWire + OpenAI), entrée dans Teams en **Python** (Playwright).

## Architecture

```
┌─ fdm (Rust) ───────────────────────────────────────────────┐     ┌─ fdm-join (Python, facultatif) ─────┐
│ crée 2 nœuds PipeWire (disparaissent à l'arrêt) :          │     │ Chromium dédié au bot, branché sur  │
│   fdm_meeting  = haut-parleur du bot (il ENTEND la réunion)│◄────│ fdm_meeting / fdm_ai_mic, rejoint   │
│   fdm_ai_mic   = micro du bot (il PARLE dans la réunion)   │────►│ la réunion comme invité « Nova »    │
│ flux continu ⇄ OpenAI : gpt-live-1 (défaut) ou gpt-realtime│     └─────────────────────────────────────┘
│ savoir-vivre : quand parler, quand se taire, interruptions │
└────────────────────────────────────────────────────────────┘
```

Les deux moitiés ne se parlent pas : leur seul lien, ce sont les périphériques PipeWire. Votre propre Teams et
votre son ne sont pas touchés : vous pouvez être dans la même réunion avec votre client habituel.

### Deux moteurs

| | **Live** (`gpt-live-1`, défaut) | **Realtime** (`gpt-realtime`) |
|---|---|---|
| Duplex | Vraiment full duplex : écoute et parle en même temps, gère seul le rythme et les interruptions | Tour par tour (détection de fin de phrase) ; écoute pendant qu'il parle et se laisse couper |
| Quand parler | Une **porte** sur sa voix : fermée tant qu'on ne l'appelle pas, ouverte dès que son nom est entendu | On ne déclenche une réponse que s'il est appelé, puis à chaque phrase de l'échange ; il peut choisir de se taire (outil) |
| Réflexion | Délègue à un modèle OpenAI (recherche web incluse) | Le même modèle |
| Interruption | Gérée par le modèle | Coupure si quelqu'un parle par-dessus plus de 400 ms (les « hm » sont ignorés) |

**Fin de l'échange**, comme entre humains : on le congédie (« merci Nova », « c'est bon Nova »), ou les autres
continuent un bon moment entre eux sans s'adresser à lui (`FDM_DISENGAGE_SECONDS`, 90 s). Aucune limite de durée
sur ce qu'il dit.

## Installation (Arch)

```bash
sudo pacman -S --needed rust clang pipewire pipewire-pulse wireplumber libpulse python
git clone <ce dépôt> && cd full-duplex-meetings

cargo install --path core                 # installe le binaire `fdm` dans ~/.cargo/bin

python -m venv .venv && source .venv/bin/activate
pip install -e join && python -m playwright install chromium   # ou FDM_BROWSER_PATH=/usr/bin/chromium

cp .env.example .env && $EDITOR .env      # OPENAI_API_KEY, prénom, surnoms…
```

## Utilisation

```bash
# 1. Vérifier le son sans IA : le bot répète ce qu'il entend
fdm --echo-test --join "https://teams.microsoft.com/l/meetup-join/..."

# 2. Pour de vrai : il rejoint et participe
fdm --join "https://teams.microsoft.com/l/meetup-join/..." --context "Point projet X, avec Alice et Paul"
```

Une fenêtre Chromium s'ouvre et clique sur « Continuer sur ce navigateur », saisit son nom, coupe la caméra puis
clique sur « Rejoindre ». L'organisateur l'admet depuis la salle d'attente. Si l'interface Teams a changé, cliquez
vous-même dans la fenêtre : l'audio est déjà branché. Fermer la fenêtre arrête le navigateur, Ctrl+C arrête tout.

Sans pilotage du navigateur : lancez `fdm` seul, puis, dans **n'importe quel** client Teams dédié au bot (autre
profil de navigateur, `teams-for-linux` avec un 2ᵉ compte…), choisissez *Haut-parleur : FDM Réunion* et
*Micro : FDM Micro IA*. ⚠️ Pas dans votre propre Teams, sinon l'IA parle à votre place.

Astuce : connectez une fois le navigateur du bot (profil `.fdm-browser-profile/`) à un compte Microsoft dédié pour
éviter la salle d'attente des invités.

Toutes les options : `fdm --help` (chacune a son équivalent `FDM_*` dans `.env`).

## Tests

```bash
cargo test --manifest-path core/Cargo.toml
```

Vérifié aussi de bout en bout dans PipeWire, avec de faux serveurs Live/Realtime : son nom ouvre la parole, sa voix
arrive dans `fdm_ai_mic`, il est bloqué s'il parle sans être appelé, il se fait couper, et le flux montant reste
continu. Et avec un vrai Chromium : sa sortie arrive dans `fdm_meeting` et son micro écoute `fdm_ai_mic`.

## Pistes

- **Qui parle ?** Lire les sous-titres en direct de Teams (ils affichent le nom de chaque orateur) dans la page du
  bot, et les envoyer à `fdm` (socket local) pour les donner au modèle en pensée silencieuse.
- Délégation `client` : brancher notre propre LLM et nos outils (prise de notes, compte-rendu, recherche dans des docs).
- Fin d'échange jugée par le modèle lui-même (Decisions de l'API Live) plutôt que par des règles.
