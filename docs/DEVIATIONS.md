# Choix d'implémentation et écarts par rapport à la Recommandation

La Rec. H.120 spécifie strictement le bitstream et les reconstructions, mais
laisse délibérément plusieurs blocs « au choix de l'implémenteur » (ils
n'affectent pas l'interopérabilité). Ce document liste, exhaustivement, ce
que cette implémentation a choisi — et les quelques points où elle s'écarte
sciemment du document.

## 1. Périmètre

| Élément de la spec | Statut |
|---|---|
| Clause 1 (625/50, 2048 kbit/s, conditional replenishment) | **Implémentée** |
| Clause 2 (variante 525/60, 1544 kbit/s) | Non implémentée |
| Clause 3 (codec à compensation de mouvement, 1988) | Non implémentée |
| §1.6 Transmission (trame G.704/H.130, audio G.711, signalisation TS2) | Non implémentée (voir §2) |
| §1.7 Correction d'erreurs BCH (4095, 4035) | Non implémentée |
| Annexes A–D (mode graphique, chiffrement) | Non implémentées |

## 2. Couche transmission absente

Le fichier `.h120` contient le **multiplex vidéo seul** (sortie du « video
multiplex coder », §1.5), pas la trame 2048 kbit/s complète. Raisons :

- la structure de trame est définie dans la Rec. H.130, document séparé ;
  l'implémenter fidèlement sans cette référence serait de l'extrapolation ;
- l'audio, la justification d'horloge et la signalisation codec-à-codec
  n'ont pas de sens pour un codec travaillant sur fichiers.

La contrainte de débit du canal reste simulée : le buffer de 96 kbit (§1.5.1)
se vide au débit `--bitrate` (défaut 1600 kbit/s, approximation de la part
vidéo d'un canal 2048 kbit/s après audio 64k, signalisation et trame). Les
mécanismes pilotés par le buffer (bit A, sous-échantillonnages, lignes PCM)
sont tous actifs.

Conséquence : le bit 1 (justification d'horloge) et le bit 2 (état du buffer
sur 8 bits multitrame) de TS2, transportés par la trame H.130, n'existent pas
ici. L'état « buffer < 6 kbit » reste signalé par le bit A des FST, comme
dans la spec.

## 3. Blocs laissés libres par la spec — choix faits

### Détecteur de mouvement (§1.4.1.3 : « It is not necessary to specify… »)

Seuil sur |entrée − mémoire| par échantillon, durci linéairement avec
l'occupation du buffer (de 4 à 18 niveaux). Les segments mobiles d'une ligne
séparés de ≤ 6 échantillons sont fusionnés en un seul cluster (la spec impose
de toute façon un écart minimal de 4 entre clusters).

### Pré/post-filtres (§1.4.1.2 : caractéristiques non imposées)

Redimensionnement bilinéaire : l'image d'entrée est ramenée à 256×286
(luminance) et 52×286 (chrominance), champ 1 = lignes paires, champ 2 =
lignes impaires. Au décodage : tissage des deux champs, interpolation de la
composante chroma absente de chaque ligne à partir des lignes voisines du
même champ, sur-échantillonnage chroma 52 → 256. Pas de filtre temporel.

### Rafraîchissement PCM (§1.5.5 : « systematic or forced updating »)

- au démarrage (mémoires à 128) : autant de lignes PCM par champ que le
  buffer le permet (remplissage jusqu'à 70 %), d'où une image complète en
  une seconde environ — la montée progressive de l'image est authentique ;
- en régime établi : une ligne PCM par champ, en rotation, si l'occupation
  est sous 45 % — l'image entière est rafraîchie en ~2,9 s.

### Contrôle de débit (Appendice I : principes seulement)

Seuils d'occupation du buffer : sous-échantillonnage horizontal au-delà de
55 %, omission du champ 2 au-delà de 72 %, lignes vides (« panique »)
au-delà de 97 %. Les éléments extra sont émis sous 65 % d'occupation quand
l'erreur d'interpolation atteint 12 niveaux. Seul le champ 2 est omis (la
spec autorise l'un ou l'autre) ; le décodeur, lui, gère l'omission de
n'importe quel champ.

## 4. Écarts et interprétations assumés

1. **Erratum Table 2** : le document imprime « 0 to 22 » pour le niveau +15,
   ce qui chevauche la plage « 0 to +9 » du niveau +4. Lecture retenue :
   « **10** to 22 ». De même « 1–5 » et « 1+4 » se lisent « −5 » et « +4 ».

2. **Reconstructions DPCM bornées** : la spec interdit les mots PCM hors
   16–239 mais ne dit pas comment borner `prédiction + niveau` ; les
   reconstructions sont écrêtées à [16, 239] (luminance) et [17, 239]
   (chrominance), identiquement à l'encodage et au décodage.

3. **Flux toujours « couleur »** : en mode `--mono`, la chrominance est
   neutralisée (128) mais les lignes PCM transportent toujours leurs
   52 octets de chrominance. Un vrai flux monochrome (sans ces octets) ne
   serait pas distinguable sans la signalisation hors-bande de H.130 ; le
   décodeur suppose donc le format couleur.

4. **Élément 255 jamais transmis en cluster** : la spec le force à 128 des
   deux côtés (§1.4.1.1) ; l'encodeur termine ses clusters à l'élément 254
   au plus, le décodeur force 128 après chaque ligne.

5. **Ordre extra/normal** : la spec ne précise pas explicitement la position
   du code « extra » dans le flux ; il est émis ici en ordre spatial, entre
   le code normal de l'élément à sa gauche et celui de l'élément à sa
   droite, ce qui rend le décodage déterministe sans information annexe.

6. **Numéro de ligne dans le quinconce** : « éléments pairs sur lignes
   paires » est interprété avec le numéro de ligne de la spec (0–142 /
   144–286) et, pour la chrominance, la parité de l'échantillon (égale à
   celle de son adresse).

7. **Fin de flux** : un fichier se termine sans marqueur ; le décodeur traite
   l'épuisement du flux comme une fin propre (la dernière image peut être
   perdue si son champ 2 a été omis, l'interpolation nécessitant le champ
   suivant).

8. **Délai décodeur** : la spec prévoit ~130 ms de latence (buffer canal) ;
   sur fichier, cette latence n'existe pas, le lecteur joue dès que possible.

## 5. Vérification

`cargo test` exécute notamment :

- la conformité bit à bit des codes des Tables 1 et 2 aux chaînes imprimées
  dans la spec, et la liberté de préfixe de l'ensemble codes + EOC ;
- le **synchronisme bit à bit** des mémoires d'image encodeur/décodeur après
  chaque image, en mode normal comme en mode sous-échantillonné (boucle
  fermée — c'est la propriété fondamentale d'un codec DPCM) ;
- l'exactitude du chemin PCM (une scène statique devient identique à
  l'entrée après amorçage) ;
- la tenue du contrôle de débit (le buffer de 96 kbit ne déborde jamais) et
  la robustesse aux flux tronqués.
