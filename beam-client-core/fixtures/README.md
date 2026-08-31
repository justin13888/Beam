# Committed Matroska fixtures for the demuxer

Tiny (64x64, ~0.6s) real containers used by `crate::demux`'s tests. Real bytes,
not a hand-built element tree: a demuxer tested against a synthetic structure
only proves that the test author and the parser agree, which is the failure
mode that lets a real file crash it. It is the same discipline `src/tls.rs`
applies by generating actual certificates with `rcgen`.

## Why these are duplicated from `beam-index/tests/fixtures`

`beam-client-core` deliberately depends on none of the server crates
([ADR-0012](../../docs/architecture/decisions/ADR-0012-native-client-rust-core.md)),
so it cannot reach across to `beam-index`'s copies, and a `../..` path in an
`include_bytes!` would be that dependency in everything but name. They are a
few KB each, and the recipe below is the one
`beam-index/tests/fixtures/README.md` already records -- so the two sets are
derived from one recipe rather than maintained as two.

## Fixtures

| File             | Video | Audio | Container | What it covers |
|------------------|-------|-------|-----------|----------------|
| `h264.mkv`       | H.264 | --    | Matroska  | the common case; single-track parsing |
| `hevc.mkv`       | HEVC  | --    | Matroska  | a second `CodecID` and a different `CodecPrivate` shape |
| `h264_aac.mkv`   | H.264 | AAC   | Matroska  | two tracks, track selection, audio track metadata |
| `vp9_opus.webm`  | VP9   | Opus  | WebM      | WebM read through the same path; a codec Apple cannot render |

`vp9_opus.webm` earns its place twice: it proves the extractor opens WebM
through the same code, and its Opus track is one the Apple sample-buffer engine
has no decoder for -- which the capability matcher has to report as a reason
rather than as an unknown codec.

## Regeneration

All inputs are synthetic (`testsrc2` video, `sine` audio), so no third-party
media is embedded. `h264.mkv`, `hevc.mkv` and `vp9_opus.webm` are copies of the
`beam-index` fixtures of the same name; `h264_aac.mkv` is generated here.

```sh
V="testsrc2=size=64x64:rate=15:duration=0.6"
A="sine=frequency=440:duration=0.6:sample_rate=44100"
FF="ffmpeg -hide_banner -loglevel error -y"

$FF -f lavfi -i "$V" -f lavfi -i "$A" -c:v libx264 -preset ultrafast -crf 40 \
    -pix_fmt yuv420p -c:a aac -b:a 24k h264_aac.mkv
```

This needs a system FFmpeg built with libx264; the vendored LGPL build
(ADR-0007) ships no H.264 encoder. That is why the files are committed rather
than synthesized at test time -- and it is why the suite still satisfies
NFR-201, since *reading* them needs no FFmpeg at all.
