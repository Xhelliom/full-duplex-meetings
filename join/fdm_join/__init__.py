"""fdm-join : fait entrer le bot dans une réunion Teams via un Chromium dédié (Playwright).

Ne gère PAS l'audio ni l'IA : c'est le rôle du binaire Rust `fdm`, qui crée les périphériques
PipeWire `fdm_meeting` (haut-parleur du bot) et `fdm_ai_mic` (micro du bot). Ce script se
contente d'ouvrir un navigateur branché dessus et de cliquer « Rejoindre ».
"""
