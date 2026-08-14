//! Wraps a rodio `Player` connected to the default output device, kept in
//! sync with the master [`crate::timeline::Timeline`] cursor.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use rodio::{MixerDeviceSink, Player};

pub struct AudioPlayback {
    /// Must be kept alive for as long as we want to hear anything.
    _device: MixerDeviceSink,
    player: Player,
    last_playing: bool,
    last_synced_at: f64,
}

impl AudioPlayback {
    pub fn new(path: &Path) -> Result<Self> {
        let device = rodio::DeviceSinkBuilder::open_default_sink()
            .map_err(|e| anyhow::anyhow!("no audio output device available: {e}"))?;
        let player = Player::connect_new(&device.mixer());

        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let byte_len = file.metadata().map(|m| m.len()).ok();
        let hint = path.extension().and_then(|e| e.to_str()).map(str::to_owned);

        let mut builder = rodio::Decoder::builder().with_data(BufReader::new(file));
        if let Some(len) = byte_len {
            builder = builder.with_byte_len(len);
        }
        if let Some(hint) = &hint {
            builder = builder.with_hint(hint);
        }
        let source = builder.build().map_err(|e| anyhow::anyhow!("decoding audio: {e}"))?;
        player.append(source);
        player.pause();

        Ok(Self {
            _device: device,
            player,
            last_playing: false,
            last_synced_at: 0.0,
        })
    }

    /// Called once per frame with the desired playback position (this
    /// source's local time, i.e. master cursor minus its offset) and whether
    /// the master timeline is currently playing.
    pub fn sync(&mut self, target_time: f64, playing: bool) {
        let target_time = target_time.max(0.0);
        if playing != self.last_playing {
            self.seek(target_time);
            if playing {
                self.player.play();
            } else {
                self.player.pause();
            }
            self.last_playing = playing;
        } else if playing {
            let actual = self.player.get_pos().as_secs_f64();
            if (actual - target_time).abs() > 0.3 {
                self.seek(target_time);
            }
        } else if (target_time - self.last_synced_at).abs() > 0.05 {
            // Paused, but the user scrubbed: keep the decoder positioned so
            // playback resumes from the right spot.
            self.seek(target_time);
        }
    }

    fn seek(&mut self, t: f64) {
        let _ = self.player.try_seek(Duration::from_secs_f64(t));
        self.last_synced_at = t;
    }
}
