# h120 — implémentation de référence du codec vidéo ITU-T H.120

H.120 (CCITT, 1984) est **le premier codec vidéo numérique standardisé** de
l'histoire. Conçu pour la visioconférence sur des liaisons à 2048 kbit/s, il
n'a jamais connu de déploiement commercial ni d'implémentation publique : il
n'existait jusqu'ici que sous la forme de sa spécification
([Rec. ITU-T H.120 (03/93)](https://www.itu.int/rec/T-REC-H.120)).

Ce projet en propose une implémentation de référence en Rust : encodeur,
décodeur, analyseur de flux et lecteur graphique. Il couvre la **clause 1**
de la Recommandation — le codec européen historique (COST 211) à
*conditional replenishment* : 625 lignes / 50 champs/s, luminance de
256 échantillons sur 143 lignes actives par champ, DPCM et codes à longueur
variable, canal de 2048 kbit/s.

> Voir [docs/FORMAT.md](docs/FORMAT.md) pour le fonctionnement du codec et
> [docs/DEVIATIONS.md](docs/DEVIATIONS.md) pour la liste précise des choix
> d'implémentation et des écarts par rapport à la spec.

## Compilation

Prérequis :

- **Rust** (édition 2024, testé avec rustc 1.95) ;
- **GTK 4 ≥ 4.14** et **libadwaita ≥ 1.5** (paquets de développement) pour le
  lecteur intégré — sous Debian/Ubuntu : `sudo apt install libgtk-4-dev libadwaita-1-dev` ;
- **ffmpeg** (binaire dans le PATH), facultatif mais recommandé : il sert à
  lire les formats autres que Y4M et à exploiter les fichiers décodés.

```bash
cargo build --release
```

Deux binaires sont produits, volontairement séparés pour la portabilité :

- `target/release/h120` — encodeur, décodeur et analyseur. **Aucune
  dépendance graphique** (seule la libc est liée) : il se copie tel quel sur
  un serveur ou une machine sans environnement de bureau ;
- `target/release/h120-play` — le lecteur graphique, seul à dépendre de
  GTK4/libadwaita.

Pour ne compiler que le CLI (sans même avoir GTK installé) :

```bash
cargo build --release --no-default-features
```

## Utilisation

### Encoder une vidéo

```bash
h120 encode entrée.mp4 sortie.h120
```

L'entrée peut être n'importe quel fichier vidéo lisible par ffmpeg (MP4, MKV,
WebM…) : elle est automatiquement convertie en 256×286 à 25 images/s, avec
letterbox pour préserver les proportions sur l'écran 4:3 du codec. Un fichier
`.y4m` est lu nativement, sans ffmpeg.

Options :

| Option | Effet |
|---|---|
| `--bitrate 1600k` | Débit vidéo simulé (défaut 1600k, max 2048k). Plus il est bas, plus le codec sous-échantillonne et saccade — authentiquement. |
| `--mono` | Encode en monochrome (chrominance neutre). |
| `--frames N` | N'encode que les N premières images. |

À la fin, l'encodeur affiche ses statistiques : débit réel, lignes PCM de
rafraîchissement, lignes sous-échantillonnées, champs omis, occupation
maximale du buffer de 96 kbit.

### Lire un flux dans une fenêtre

```bash
h120-play sortie.h120
```

Ouvre une fenêtre GTK4/libadwaita et joue le flux à 25 i/s, au rapport 4:3
d'origine (les pixels H.120 ne sont pas carrés). Barre d'en-tête : bouton
pause (ou touche espace), lecture en boucle, compteur d'images.

On y observe les comportements caractéristiques du codec : montée de l'image
en ~1 seconde au démarrage (rafraîchissement PCM progressif), perte de
définition horizontale dans les zones en mouvement, saccades quand le canal
sature.

### Décoder vers un fichier vidéo standard

```bash
h120 decode sortie.h120 sortie.y4m          # Y4M 4:4:4, 256×288, 25 i/s
h120 decode sortie.h120 sortie.y4m --scale 2 # agrandi 2× (512×576)
```

Le fichier Y4M se lit avec mpv ou VLC, et se convertit avec ffmpeg — en
corrigeant l'aspect (le Y4M transporte déjà le bon rapport de pixel, mpv et
VLC le respectent ; pour un MP4 à pixels carrés, on ré-échantillonne) :

```bash
mpv sortie.y4m                                # lecture directe
ffmpeg -i sortie.y4m -vf "scale=768:576:flags=lanczos" -pix_fmt yuv420p sortie.mp4
```

### Analyser un flux

```bash
h120 info sortie.h120
```

Affiche taille, durée, débit, et la ventilation des lignes (PCM, mobiles,
sous-échantillonnées, vides), clusters, éléments extra et champs omis.

## Exemple complet

```bash
h120 encode film.mp4 film.h120 --bitrate 1600k
h120 info film.h120
h120-play film.h120
h120 decode film.h120 film.y4m --scale 2
ffmpeg -i film.y4m -pix_fmt yuv420p film_h120.mp4
```

## Ce qui est implémenté

- Codec de la **clause 1** complet : conditional replenishment, DPCM
  (prédiction (A+D)/2 en luminance, A en chrominance), quantification et
  codes à longueur variable des Tables 1 et 2, adressage des clusters,
  échappement couleur, lignes PCM de rafraîchissement, sous-échantillonnage
  horizontal en quinconce avec éléments « extra », omission/interpolation de
  champs, codes LST/FST avec bit A, contrôle de débit par buffer de 96 kbit.
- Le fichier `.h120` est le **multiplex vidéo brut de la spec**, bit à bit —
  pas de conteneur propriétaire.
- L'encodeur et le décodeur maintiennent des mémoires d'image strictement
  identiques (boucle fermée) : c'est vérifié bit à bit par les tests
  d'intégration (`cargo test`).

## Ce qui ne l'est pas

- Les clauses 2 (variante 525 lignes/1544 kbit/s) et 3 (codec à compensation
  de mouvement de 1988) ;
- la couche transmission (trame G.704/H.130, audio A-law, FEC BCH,
  signalisation codec-à-codec) : le flux produit est le multiplex vidéo seul,
  la contrainte de débit du canal restant simulée — voir
  [docs/DEVIATIONS.md](docs/DEVIATIONS.md) ;
- les options des annexes (mode graphique, chiffrement, multipoint).

## Licence et statut

Projet à but historique et pédagogique. La Recommandation ITU-T H.120 reste
la référence normative ; en cas de divergence non documentée dans
docs/DEVIATIONS.md, c'est un bug — les rapports sont bienvenus.
