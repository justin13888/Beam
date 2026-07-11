# Committed real-media probe fixtures

Tiny (64x64, ~0.6s) real containers used by the probe test suite to exercise
[`VideoFileMetadata::from_path`](../../src/probe/metadata.rs) against actual
demuxed streams. See GitHub issue #92.

## Why these are committed (and not synthesized at test time)

The probe tests synthesize their own MP4/MKV fixtures at runtime using the
**linked** FFmpeg (`ffmpeg-next`). That vendored LGPL build (ADR-0007), and the
GPL system FFmpeg CI uses, only ship a limited set of *encoders* (mpeg4,
mpeg2video, mjpeg, ffv1, native aac, flac, pcm). Neither build can *encode*
H.264, HEVC, AV1, or VP9 — and **no** common build ships an AV1 encoder that CI
can rely on — yet all of these have native *decoders*, which is all
`from_path` needs (it opens decoder contexts; it never decodes frames).

So the modern-codec containers are generated **once** here with a
full-featured system FFmpeg and committed. They are a few KB each
(28.5 KB total), well under the suite's size budget.

## Fixtures

| File            | Video | Audio | Container |
|-----------------|-------|-------|-----------|
| `h264_aac.mp4`  | H.264 | AAC   | MP4 (faststart) |
| `h264.mkv`      | H.264 | —     | Matroska  |
| `hevc.mp4`      | HEVC  | —     | MP4 (faststart, `hvc1` tag) |
| `hevc.mkv`      | HEVC  | —     | Matroska  |
| `av1.mp4`       | AV1   | —     | MP4 (faststart) |
| `av1.mkv`       | AV1   | —     | Matroska  |
| `av1.webm`      | AV1   | —     | WebM      |
| `vp9_opus.webm` | VP9   | Opus  | WebM      |

Audio streams are included in `h264_aac.mp4` (AAC-in-MP4) and `vp9_opus.webm`
(Opus-in-WebM) so audio probing is covered on real streams in both major
container families.

## Regeneration

Generated with a system FFmpeg built with libx264/libx265/libaom-av1/libvpx-vp9
(the repo was created with FFmpeg 8.1.1). All inputs are synthetic
(`testsrc2` video, `sine` audio) so no third-party media is embedded.

```sh
V="testsrc2=size=64x64:rate=15:duration=0.6"
A="sine=frequency=440:duration=0.6:sample_rate=44100"
FF="ffmpeg -hide_banner -loglevel error -y"

$FF -f lavfi -i "$V" -f lavfi -i "$A" -c:v libx264 -preset ultrafast -crf 40 \
    -pix_fmt yuv420p -c:a aac -b:a 24k -movflags +faststart h264_aac.mp4
$FF -f lavfi -i "$V" -c:v libx264 -preset ultrafast -crf 40 -pix_fmt yuv420p h264.mkv
$FF -f lavfi -i "$V" -c:v libx265 -preset ultrafast -crf 40 -pix_fmt yuv420p \
    -tag:v hvc1 -movflags +faststart hevc.mp4
$FF -f lavfi -i "$V" -c:v libx265 -preset ultrafast -crf 40 -pix_fmt yuv420p hevc.mkv
$FF -f lavfi -i "$V" -c:v libaom-av1 -cpu-used 8 -crf 50 -b:v 0 -pix_fmt yuv420p \
    -movflags +faststart av1.mp4
$FF -f lavfi -i "$V" -c:v libaom-av1 -cpu-used 8 -crf 50 -b:v 0 -pix_fmt yuv420p av1.mkv
$FF -f lavfi -i "$V" -c:v libaom-av1 -cpu-used 8 -crf 50 -b:v 0 -pix_fmt yuv420p av1.webm
$FF -f lavfi -i "$V" -f lavfi -i "$A" -c:v libvpx-vp9 -crf 50 -b:v 0 -pix_fmt yuv420p \
    -c:a libopus -b:a 24k vp9_opus.webm
```
