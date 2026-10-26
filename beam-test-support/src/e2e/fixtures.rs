//! Fixture media, generated per run by the FFmpeg CLI.
//!
//! Real containers with real streams, so the server probes, classifies and
//! serves what a library actually holds. Nothing here is committed: every
//! input is synthetic (`testsrc2` video, a `sine` tone), and a video's tone
//! is what makes its bytes its own. Encoding is single-threaded and
//! bit-exact, so two videos that differ only in a tag of the same length are
//! the same size -- which is what a same-size swap needs.
//!
//! The committed probe fixtures in `beam-index/tests/fixtures` (HEVC, AV1 and
//! the rest no CI FFmpeg can encode) are reused as they are, through
//! [`probe_fixture`].

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The container a [`Video`] is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// H.264 and AAC in an MP4 with its index up front: what a browser
    /// direct-plays (ADR-0004).
    Mp4,
    /// H.264 and AAC in Matroska.
    Matroska,
    /// MPEG-2 video and MPEG audio in an MPEG program stream: a DVD's VOB.
    Vob,
}

/// One synthetic video, described before it is rendered.
#[derive(Debug, Clone)]
pub struct Video {
    seconds: u32,
    width: u32,
    height: u32,
    tone_hz: u32,
    container: Container,
    tags: Vec<(String, String)>,
}

impl Video {
    /// Six seconds of 160x90 H.264/AAC in an MP4, its audio a `tone_hz`
    /// tone. Give each video in a test a tone of its own and their bytes
    /// differ.
    pub fn new(tone_hz: u32) -> Self {
        Self {
            seconds: 6,
            width: 160,
            height: 90,
            tone_hz,
            container: Container::Mp4,
            tags: Vec::new(),
        }
    }

    pub fn seconds(mut self, seconds: u32) -> Self {
        self.seconds = seconds;
        self
    }

    pub fn size(mut self, width: u32, height: u32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    pub fn container(mut self, container: Container) -> Self {
        self.container = container;
        self
    }

    /// A container-level tag (`title`, `show`, `season_number`, ...), as
    /// FFmpeg's `-metadata` writes it.
    pub fn tag(mut self, key: &str, value: &str) -> Self {
        self.tags.push((key.to_owned(), value.to_owned()));
        self
    }

    /// How long the video is, in seconds.
    pub fn duration_secs(&self) -> f64 {
        f64::from(self.seconds)
    }

    /// Render the video to `dest`.
    ///
    /// # Panics
    ///
    /// When no FFmpeg CLI with libx264 is on `PATH`, saying so, or when the
    /// encode fails.
    pub fn render(&self, dest: &Path) {
        require_ffmpeg();
        let Self {
            seconds,
            width,
            height,
            tone_hz,
            container,
            tags,
        } = self;
        let mut command = Command::new("ffmpeg");
        command
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i"])
            .arg(format!(
                "testsrc2=size={width}x{height}:rate=24:duration={seconds}"
            ))
            .args(["-f", "lavfi", "-i"])
            .arg(format!(
                "sine=frequency={tone_hz}:sample_rate=48000:duration={seconds}"
            ))
            .args(["-threads", "1", "-shortest", "-map_metadata", "-1"])
            .args([
                "-fflags",
                "+bitexact",
                "-flags:v",
                "+bitexact",
                "-flags:a",
                "+bitexact",
            ]);
        match container {
            Container::Mp4 | Container::Matroska => {
                command.args([
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "48k",
                ]);
            }
            Container::Vob => {
                command.args(["-c:v", "mpeg2video", "-c:a", "mp2", "-b:a", "64k"]);
            }
        }
        for (key, value) in tags {
            command.arg("-metadata").arg(format!("{key}={value}"));
        }
        match container {
            Container::Mp4 => command.args(["-movflags", "+faststart", "-f", "mp4"]),
            Container::Matroska => command.args(["-f", "matroska"]),
            Container::Vob => command.args(["-f", "vob"]),
        };
        let output = command.arg(dest).output().expect("run ffmpeg");
        assert!(
            output.status.success(),
            "ffmpeg could not render {}: {}",
            dest.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Fail once, clearly, when the FFmpeg CLI this tier needs is missing.
fn require_ffmpeg() {
    static CHECKED: OnceLock<Result<(), String>> = OnceLock::new();
    let outcome = CHECKED.get_or_init(|| {
        let output = Command::new("ffmpeg")
            .args(["-hide_banner", "-encoders"])
            .output()
            .map_err(|e| format!("no ffmpeg on PATH ({e})"))?;
        let encoders = String::from_utf8_lossy(&output.stdout);
        ["libx264", "aac", "mpeg2video", "mp2"]
            .into_iter()
            .find(|encoder| !encoders.split_whitespace().any(|word| word == *encoder))
            .map_or(Ok(()), |missing| {
                Err(format!("the ffmpeg on PATH has no {missing} encoder"))
            })
    });
    if let Err(message) = outcome {
        panic!(
            "the e2e-server tier generates its fixture media with the FFmpeg CLI, and {message}. \
             Install an FFmpeg built with libx264."
        );
    }
}

/// One of the committed probe fixtures in `beam-index/tests/fixtures`, by
/// file name, found from `workspace_root`.
pub fn probe_fixture(workspace_root: &Path, name: &str) -> PathBuf {
    let path = workspace_root.join("beam-index/tests/fixtures").join(name);
    assert!(path.is_file(), "no probe fixture {}", path.display());
    path
}
