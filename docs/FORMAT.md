# The H.120 codec (clause 1) and the stream format

This document summarizes how the codec works and the exact structure of the
bitstream produced/consumed by this implementation. The "§x.y" references point
to Rec. ITU-T H.120 (03/93).

## 1. Principle: conditional replenishment

H.120 only transmits what changes. The encoder and the decoder each maintain a
**frame memory** (two fields of 143 lines × 256 samples); a motion detector
compares the incoming image to the memory, and only the groups of samples
deemed moving — the **clusters** — are transmitted, coded in DPCM with
variable-length codes. The rest of the image is slowly "replenished" by full
PCM lines.

Since bit production is irregular, a **96 kbit buffer** (§1.5.1) smooths the
rate towards the channel. Its fill level drives the progressive degradation:

1. raised motion-detector thresholds;
2. **horizontal subsampling** in a quincunx pattern: every other sample (even
   ones on even lines, odd ones on odd lines), the missing ones being
   interpolated at decoding (§1.4.1.4.1);
3. **field omission**: a whole field skipped, reconstructed by
   spatio-temporal interpolation (§1.4.1.4.2);
4. as a last resort, lines left empty (the image freezes).

Conversely, when the buffer drains, uncompressed **PCM lines** refresh the
image in rotation (§1.5.5) — this is also what builds up the initial image at
startup.

## 2. Image format

| | Luminance | Chrominance |
|---|---|---|
| Samples per active line | 256 (element 255 forced to 128) | 52 (addresses 4 to 55) |
| Active lines per field | 143 | 143 (one component per line) |
| Levels | black 16, white 239 | zero 128, range 17–239 |

The (B′−Y′) and (R′−Y′) components alternate line by line: the first line of
field 1 carries (B′−Y′), the first line of field 2 carries (R′−Y′) (§1.4.2.1).
The component missing from a line is interpolated at display time.

The lines are numbered 0–142 (field 1) and 144–286 (field 2); 143 and 287 are
uncoded synchronization lines (§1.5.2.1, Figure 3).

## 3. Bitstream structure

Everything is serialized MSB first (§1.6.1). No byte alignment is guaranteed.
Since legal PCM values are confined to 16–239, the synchronization words (≥ 12
zeros) and the special codes (0xFF, 0x09) cannot be imitated by data.

### Line start code — LST (20 bits, §1.5.2.1)

```
0000 0000 0000 1000   S   LLL
└── 16-bit sync ────┘  │   └─ 3 low bits of the line number
                       └─ 1 if the line that follows is subsampled
```

### Field start code — FST (48 bits, Figure 4)

```
0000 0000 0000 1 AAA  F 111   0000 F11F   0000 0000 0000 1000  S 000
└─ LST of line 143/287 ─┘     └─ byte ──┘ └─ LST of line 0/144 ─────┘
```

- F = 1: FST‑1 (field 1 follows); F = 0: FST‑2 (field 2 follows);
- AAA = 111 if the transmitter buffer holds less than 6 kbit (A bit);
- S = subsampling of the first line of the field.

**Two consecutive FSTs with the same number** signal that the intermediate
field was omitted and must be interpolated (§1.5.2.2).

### Content of a line (after its LST)

Three cases, discriminated by the next 8 bits:

- `1111 1111` → **PCM line** (Figure 6):
  `0xFF, 0xFF, 256 luminance bytes (the last one is 128), 52 chrominance
  bytes`. Never subsampled, non-moving for field interpolation.
- `0000 1001` → **color escape**: the line has no luma cluster, the chroma
  clusters follow directly.
- ≥ 12 zeros → **empty line** (the next LST begins).
- otherwise → **luminance clusters**:

```
PCM(8 bits)  address(8 bits)  VLC…  EOC  PCM  address  VLC…  [EOC  0000 1001  chroma clusters…]
```

Each cluster starts with the PCM value of its first element then its address
(§1.5.3). The EOC (`1001`) separates clusters; it is **omitted after the last
cluster of the line** (the following synchronization word serves instead). If
color data follows, the last luma cluster keeps its EOC, then comes the
`0000 1001` escape and the chroma clusters (addresses 4–55, same structure,
§1.5.4).

Addressing constraints: no cluster starting at address 255 (luma) or 0x37
(chroma), a minimum gap of 4 elements between clusters, a minimum length of 1
(§1.5.3, §1.5.4).

## 4. DPCM and variable-length codes

Prediction (§1.4.1.3.1, Figure 1):

- luminance: X = (A + D)/2, truncated division — A = previous element on the
  same line, D = the upper-right element on the previous line of the same
  field; blanking is 128;
- chrominance: X = A (§1.4.2.3.1).

The prediction error (−255 to +255) is quantized into at most 16 levels. Every
code of Tables 1 and 2 has one of the two forms `0…01` (positive levels) or
`10…01` (negative levels), the EOC `1001` occupying one of the slots — the set
is therefore prefix-free and decodes by counting zeros.

**Table 1** (normal lines): 16 levels, from −141 to +140.

**Table 2** (subsampled lines): 8 levels for the elements normally transmitted
+ 8 "**extra**" codes that allow transmitting a normally omitted element when
its interpolation would be too inaccurate (§1.4.1.4.1). A cluster can end on a
normal or an extra element.

Under subsampling, the prediction substitutions are: A → AS (the element even
further back) if A was not transmitted; D → C (the element directly above) if
D belonged to an untransmitted subsampled moving area of the previous line.

## 5. Interpolation of omitted fields (§1.4.1.4.2)

For an element x of the omitted field, bracketed by the lines of the previous
transmitted field (a above, b below) and the following one (c, d):

- x is moving if a, b, c **or** d is moving (OR function); only moving elements
  are interpolated, the rest keep their value;
- luminance: x = ((a+b)/2 + (c+d)/2)/2;
- chrominance: x = (a+c)/2 in field 1, (b+d)/2 in field 2.

The encoder applies exactly the same interpolation to its own memory to stay in
sync with the decoder.

## 6. The `.h120` file

The file is the raw concatenation of the FSTs, LSTs and line data described
above — exactly the "video multiplex" of the spec, with no header or container.
Since the format is entirely fixed by the Recommendation (625/50, 256×286,
25 fps), the stream is self-describing; the decoder synchronizes on the first
FST it finds.
