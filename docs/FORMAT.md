# Le codec H.120 (clause 1) et le format de flux

Ce document résume le fonctionnement du codec et la structure exacte du
bitstream produit/consommé par cette implémentation. Les références « §x.y »
renvoient à la Rec. ITU-T H.120 (03/93).

## 1. Principe : conditional replenishment

H.120 ne transmet que ce qui change. L'encodeur et le décodeur entretiennent
chacun une **mémoire d'image** (deux champs de 143 lignes × 256 échantillons) ;
un détecteur de mouvement compare l'image entrante à la mémoire, et seuls les
groupes d'échantillons jugés mobiles — les **clusters** — sont transmis,
codés en DPCM avec des codes à longueur variable. Le reste de l'image est
« réapprovisionné » lentement par des lignes PCM complètes.

La production de bits étant irrégulière, un **buffer de 96 kbit** (§1.5.1)
lisse le débit vers le canal. Son remplissage pilote la dégradation
progressive :

1. seuils du détecteur de mouvement relevés ;
2. **sous-échantillonnage horizontal** en quinconce : un échantillon sur deux
   (pairs sur lignes paires, impairs sur lignes impaires), les manquants
   étant interpolés au décodage (§1.4.1.4.1) ;
3. **omission de champ** : un champ entier sauté, reconstruit par
   interpolation spatio-temporelle (§1.4.1.4.2) ;
4. en dernier recours, lignes laissées vides (l'image se fige).

À l'inverse, quand le buffer se vide, des **lignes PCM** non compressées
rafraîchissent l'image en rotation (§1.5.5) — c'est aussi ce qui construit
l'image initiale au démarrage.

## 2. Format d'image

| | Luminance | Chrominance |
|---|---|---|
| Échantillons par ligne active | 256 (élément 255 forcé à 128) | 52 (adresses 4 à 55) |
| Lignes actives par champ | 143 | 143 (une composante par ligne) |
| Niveaux | noir 16, blanc 239 | zéro 128, plage 17–239 |

Les composantes (B′−Y′) et (R′−Y′) alternent ligne à ligne : la première
ligne du champ 1 porte (B′−Y′), la première du champ 2 porte (R′−Y′)
(§1.4.2.1). La composante absente d'une ligne est interpolée à l'affichage.

Les lignes sont numérotées 0–142 (champ 1) et 144–286 (champ 2) ; 143 et 287
sont des lignes de synchronisation non codées (§1.5.2.1, Figure 3).

## 3. Structure du bitstream

Tout est sérialisé MSB en premier (§1.6.1). Aucun alignement octet n'est
garanti. Les valeurs PCM légales étant confinées à 16–239, les mots de
synchronisation (≥ 12 zéros) et les codes spéciaux (0xFF, 0x09) ne peuvent
pas être imités par des données.

### Code de début de ligne — LST (20 bits, §1.5.2.1)

```
0000 0000 0000 1000   S   LLL
└── synchro 16 bits ┘  │   └─ 3 bits de poids faible du n° de ligne
                       └─ 1 si la ligne qui suit est sous-échantillonnée
```

### Code de début de champ — FST (48 bits, Figure 4)

```
0000 0000 0000 1 AAA  F 111   0000 F11F   0000 0000 0000 1000  S 000
└─ LST de la ligne 143/287 ┘  └─ octet ─┘ └─ LST de la ligne 0/144 ──┘
```

- F = 1 : FST‑1 (le champ 1 suit) ; F = 0 : FST‑2 (le champ 2 suit) ;
- AAA = 111 si le buffer de l'émetteur contient moins de 6 kbit (bit A) ;
- S = sous-échantillonnage de la première ligne du champ.

**Deux FST consécutifs de même numéro** signalent que le champ intermédiaire
a été omis et doit être interpolé (§1.5.2.2).

### Contenu d'une ligne (après son LST)

Trois cas, discriminés par les 8 bits suivants :

- `1111 1111` → **ligne PCM** (Figure 6) :
  `0xFF, 0xFF, 256 octets de luminance (le dernier vaut 128), 52 octets de
  chrominance`. Jamais sous-échantillonnée, non mobile pour l'interpolation
  de champ.
- `0000 1001` → **échappement couleur** : la ligne n'a pas de cluster luma,
  les clusters chroma suivent directement.
- ≥ 12 zéros → **ligne vide** (le LST suivant commence).
- sinon → **clusters de luminance** :

```
PCM(8 bits)  adresse(8 bits)  VLC…  EOC  PCM  adresse  VLC…  [EOC  0000 1001  clusters chroma…]
```

Chaque cluster commence par la valeur PCM de son premier élément puis son
adresse (§1.5.3). L'EOC (`1001`) sépare les clusters ; il est **omis après le
dernier cluster de la ligne** (le mot de synchronisation suivant en tient
lieu). Si des données couleur suivent, le dernier cluster luma garde son EOC,
puis vient l'échappement `0000 1001` et les clusters chroma (adresses 4–55,
même structure, §1.5.4).

Contraintes d'adressage : pas de cluster commençant à l'adresse 255 (luma) ou
0x37 (chroma), écart minimal de 4 éléments entre clusters, longueur minimale
1 (§1.5.3, §1.5.4).

## 4. DPCM et codes à longueur variable

Prédiction (§1.4.1.3.1, Figure 1) :

- luminance : X = (A + D)/2, division tronquée — A = élément précédent sur la
  même ligne, D = élément au-dessus à droite sur la ligne précédente du même
  champ ; le blanking vaut 128 ;
- chrominance : X = A (§1.4.2.3.1).

L'erreur de prédiction (−255 à +255) est quantifiée sur 16 niveaux au plus.
Tous les codes des Tables 1 et 2 ont l'une des deux formes `0…01` (niveaux
positifs) ou `10…01` (niveaux négatifs), l'EOC `1001` occupant l'un des
créneaux — l'ensemble est donc préfixe-libre et se décode en comptant les
zéros.

**Table 1** (lignes normales) : 16 niveaux, de −141 à +140.

**Table 2** (lignes sous-échantillonnées) : 8 niveaux pour les éléments
normalement transmis + 8 codes « **extra** » qui permettent de transmettre un
élément normalement omis quand son interpolation serait trop fausse
(§1.4.1.4.1). Un cluster peut se terminer sur un élément normal ou extra.

En sous-échantillonnage, les substitutions de prédiction sont : A → AS
(l'élément encore avant) si A n'a pas été transmis ; D → C (l'élément
directement au-dessus) si D appartenait à une zone mobile sous-échantillonnée
non transmise de la ligne précédente.

## 5. Interpolation des champs omis (§1.4.1.4.2)

Pour un élément x du champ omis, encadré par les lignes des champs transmis
précédent (a au-dessus, b en dessous) et suivant (c, d) :

- x est mobile si a, b, c **ou** d est mobile (fonction OR) ; seuls les
  éléments mobiles sont interpolés, le reste garde sa valeur ;
- luminance : x = ((a+b)/2 + (c+d)/2)/2 ;
- chrominance : x = (a+c)/2 dans le champ 1, (b+d)/2 dans le champ 2.

L'encodeur applique exactement la même interpolation à sa propre mémoire pour
rester synchrone du décodeur.

## 6. Le fichier `.h120`

Le fichier est la concaténation brute des FST, LST et données de ligne décrits
ci-dessus — exactement le « multiplex vidéo » de la spec, sans en-tête ni
conteneur. Le format étant entièrement fixé par la Recommandation (625/50,
256×286, 25 images/s), le flux est auto-descriptif ; le décodeur se
synchronise sur le premier FST trouvé.
