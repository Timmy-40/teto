# Teto – compagnon vocal de bureau (Rust)

Application Windows native : Teto est dessinée à l'écran, répond via l'API OpenAI et parle à voix haute.

## Compiler

1. Installe Rust : https://rustup.rs (accepte les options par défaut ; sous Windows il demande aussi les « Build Tools » de Visual Studio).
2. Dans ce dossier :

```
cargo run --release
```

L'exécutable se trouve ensuite dans `target\release\teto.exe` (tu peux le copier où tu veux).

## Premier lancement

- La fenêtre Réglages s'ouvre : colle ta clé API OpenAI (platform.openai.com), puis clique sur **Enregistrer**.
- Les réglages sont écrits dans `%APPDATA%\teto\config.json`. Le message vert sous le bouton confirme l'enregistrement (ou affiche l'erreur).
- Fermer la fenêtre Réglages avec la croix enregistre aussi.

## Voix

- **Voix Windows** (par défaut) : voix SAPI installée sur ton PC, en français ou en japonais selon la langue choisie.
- **Commande externe** : pour utiliser une vraie voix de Teto, indique une ligne de commande qui crée un fichier WAV.
  `{text}` est remplacé par le texte à dire, `{out}` par le chemin du WAV à créer. Exemple (à adapter au nom exact de ta voix) :

```
voicepeak -s "{text}" -n "NOM DE LA VOIX" -o "{out}"
```

Les voix officielles de Teto sont japonaises : active « Japonais » dans les Réglages pour qu'elle réponde en japonais
(la traduction française s'affiche dans la fenêtre, seule la partie japonaise est dite à voix haute).

## Micro

Bouton **Micro** : utilise la reconnaissance vocale intégrée à Windows (français), 8 secondes d'écoute. C'est expérimental.
