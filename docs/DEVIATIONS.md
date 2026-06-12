# Implementation choices and deviations from the Recommendation

Rec. H.120 strictly specifies the bitstream and the reconstructions, but
deliberately leaves several blocks "to the implementer's choice" (they do not
affect interoperability). This document lists, exhaustively, what this
implementation has chosen — and the few points where it knowingly departs from
the document.

## 1. Scope

| Spec element | Status |
|---|---|
| Clause 1 (625/50, 2048 kbit/s, conditional replenishment) | **Implemented** |
| Clause 2 (525/60 variant, 1544 kbit/s) | Not implemented |
| Clause 3 (motion-compensated codec, 1988) | Not implemented |
| §1.6 Transmission (G.704/H.130 frame, G.711 audio, TS2 signalling) | Not implemented (see §2) |
| §1.7 BCH error correction (4095, 4035) | Not implemented |
| Annexes A–D (graphics mode, encryption) | Not implemented |

## 2. Transmission layer absent

The `.h120` file contains the **video multiplex alone** (output of the "video
multiplex coder", §1.5), not the full 2048 kbit/s frame. Reasons:

- the frame structure is defined in Rec. H.130, a separate document;
  implementing it faithfully without that reference would be extrapolation;
- audio, clock justification and codec-to-codec signalling make no sense for a
  codec working on files.

The channel rate constraint remains simulated: the 96 kbit buffer (§1.5.1)
drains at the `--bitrate` rate (default 1600 kbit/s, an approximation of the
video share of a 2048 kbit/s channel after 64k audio, signalling and framing).
The buffer-driven mechanisms (A bit, subsampling, PCM lines) are all active.

Consequence: TS2 bit 1 (clock justification) and bit 2 (8-bit multiframe buffer
state), carried by the H.130 frame, do not exist here. The "buffer < 6 kbit"
state remains signalled by the A bit of the FSTs, as in the spec.

## 3. Blocks left free by the spec — choices made

### Motion detector (§1.4.1.3: "It is not necessary to specify…")

Threshold on |input − memory| per sample, hardened linearly with the buffer
occupancy (from 4 to 18 levels). Moving segments of a line separated by
≤ 6 samples are merged into a single cluster (the spec imposes a minimum gap of
4 between clusters anyway).

### Pre/post filters (§1.4.1.2: characteristics not imposed)

Bilinear resizing: the input image is reduced to 256×286 (luminance) and
52×286 (chrominance), field 1 = even lines, field 2 = odd lines. At decoding:
weaving of the two fields, interpolation of the chroma component missing from
each line from the neighbouring lines of the same field, chroma upsampling
52 → 256. No temporal filter.

### PCM refresh (§1.5.5: "systematic or forced updating")

- at startup (memories at 128): as many PCM lines per field as the buffer
  allows (filling up to 70 %), giving a full image in about one second — the
  progressive build-up of the image is authentic;
- in steady state: one PCM line per field, in rotation, if occupancy is below
  45 % — the whole image is refreshed in ~2.9 s.

### Rate control (Appendix I: principles only)

Buffer-occupancy thresholds: horizontal subsampling beyond 55 %, omission of
field 2 beyond 72 %, empty ("panic") lines beyond 97 %. Extra elements are
emitted below 65 % occupancy when the interpolation error reaches 12 levels.
Only field 2 is omitted (the spec allows either); the decoder, on the other
hand, handles the omission of any field.

## 4. Assumed deviations and interpretations

1. **Table 2 erratum**: the document prints "0 to 22" for level +15, which
   overlaps the "0 to +9" range of level +4. Reading retained: "**10** to 22".
   Likewise "1–5" and "1+4" read as "−5" and "+4".

2. **Bounded DPCM reconstructions**: the spec forbids PCM words outside
   16–239 but does not say how to bound `prediction + level`; reconstructions
   are clipped to [16, 239] (luminance) and [17, 239] (chrominance),
   identically at encoding and decoding.

3. **Stream always "color"**: in `--mono` mode, chrominance is neutralized
   (128) but PCM lines still carry their 52 chrominance bytes. A true
   monochrome stream (without those bytes) would not be distinguishable
   without the out-of-band H.130 signalling; the decoder therefore assumes the
   color format.

4. **Element 255 never transmitted in a cluster**: the spec forces it to 128 on
   both sides (§1.4.1.1); the encoder ends its clusters at element 254 at the
   latest, the decoder forces 128 after each line.

5. **Extra/normal order**: the spec does not explicitly specify the position of
   the "extra" code in the stream; it is emitted here in spatial order, between
   the normal code of the element to its left and that of the element to its
   right, which makes decoding deterministic without side information.

6. **Line number in the quincunx**: "even elements on even lines" is
   interpreted with the spec line number (0–142 / 144–286) and, for
   chrominance, the parity of the sample (equal to that of its address).

7. **End of stream**: a file ends without a marker; the decoder treats the
   exhaustion of the stream as a clean end (the last frame may be lost if its
   field 2 was omitted, the interpolation requiring the next field).

8. **Decoder delay**: the spec provides for ~130 ms of latency (channel
   buffer); on a file, that latency does not exist, the player plays as soon as
   possible.

## 5. Verification

`cargo test` runs in particular:

- the bit-for-bit conformance of the codes of Tables 1 and 2 to the strings
  printed in the spec, and the prefix-freeness of the set of codes + EOC;
- the **bit-for-bit lockstep** of the encoder/decoder frame memories after each
  frame, in normal mode as well as subsampled mode (closed loop — the
  fundamental property of a DPCM codec);
- the correctness of the PCM path (a static scene becomes identical to the
  input after bootstrap);
- that rate control holds (the 96 kbit buffer never overflows) and robustness
  to truncated streams.
