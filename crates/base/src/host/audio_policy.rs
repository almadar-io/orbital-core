//! Shared interpretation of authored audio frames. Hosts execute the actions with their own audio API.

#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::string::String;
use std::vec::Vec;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AudioPaths {
    One(String),
    Many(Vec<String>),
}

impl AudioPaths {
    pub fn prefixed(&self, prefix: &str) -> Vec<String> {
        match self {
            Self::One(path) => vec![format!("{prefix}{path}")],
            Self::Many(paths) => paths.iter().map(|path| format!("{prefix}{path}")).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioEntry {
    #[serde(default)]
    pub path: Option<AudioPaths>,
    #[serde(default = "one_f32")]
    pub volume: f32,
    #[serde(default, rename = "loop")]
    pub looping: bool,
    #[serde(default = "one_usize")]
    pub pool_size: usize,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default = "default_crossfade")]
    pub crossfade_duration_ms: u64,
}

fn one_f32() -> f32 {
    1.0
}
fn one_usize() -> usize {
    1
}
fn default_crossfade() -> u64 {
    1500
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioFrame {
    #[serde(default)]
    pub cue: String,
    #[serde(default)]
    pub cue_seq: Option<f64>,
    #[serde(default)]
    pub music: String,
    #[serde(default)]
    pub muted: bool,
    #[serde(default = "one_f32")]
    pub volume: f32,
    #[serde(default)]
    pub manifest: BTreeMap<String, AudioEntry>,
    #[serde(default)]
    pub base_url: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AudioAction {
    PlayCue {
        key: String,
        sources: Vec<String>,
        volume: f32,
        looping: bool,
        pool_size: usize,
    },
    ChangeMusic {
        key: String,
        sources: Vec<String>,
        volume: f32,
        crossfade_ms: u64,
    },
    StopMusic,
    SetMuted {
        muted: bool,
    },
    SetVolume {
        volume: f32,
    },
}

fn cue(frame: &AudioFrame, key: &str) -> Option<AudioAction> {
    if frame.muted {
        return None;
    }
    let entry = frame.manifest.get(key)?;
    let path = entry.path.as_ref()?;
    Some(AudioAction::PlayCue {
        key: key.into(),
        sources: path.prefixed(&frame.base_url),
        volume: entry.volume,
        looping: entry.looping,
        pool_size: entry.pool_size.max(1),
    })
}

pub fn plan_audio(previous: Option<&AudioFrame>, next: &AudioFrame) -> Vec<AudioAction> {
    let mut actions = Vec::new();
    if previous.map(|prior| prior.muted) != Some(next.muted) {
        actions.push(AudioAction::SetMuted { muted: next.muted });
    }
    if previous.map(|prior| prior.volume) != Some(next.volume) {
        actions.push(AudioAction::SetVolume {
            volume: next.volume.clamp(0.0, 1.0),
        });
    }
    if previous.map(|prior| prior.music.as_str()) != Some(next.music.as_str()) {
        if next.music.is_empty() {
            actions.push(AudioAction::StopMusic);
        } else if let Some(entry) = next
            .manifest
            .get(&next.music)
            .filter(|entry| entry.path.is_some())
        {
            actions.push(AudioAction::ChangeMusic {
                key: next.music.clone(),
                sources: entry.path.as_ref().unwrap().prefixed(&next.base_url),
                volume: entry.volume,
                crossfade_ms: entry.crossfade_duration_ms,
            });
        }
    }
    if previous.and_then(|prior| prior.cue_seq).is_some()
        && next.cue_seq.is_some()
        && previous.and_then(|prior| prior.cue_seq) != next.cue_seq
        && !next.cue.is_empty()
    {
        if let Some(action) = cue(next, &next.cue) {
            actions.push(action);
        }
    }
    actions
}

pub fn plan_audio_unlock(frame: &AudioFrame) -> Vec<AudioAction> {
    frame
        .manifest
        .iter()
        .filter(|(_, entry)| entry.autostart)
        .filter_map(|(key, _)| cue(frame, key))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seq: f64) -> AudioFrame {
        AudioFrame {
            cue: "beep".into(),
            cue_seq: Some(seq),
            music: String::new(),
            muted: false,
            volume: 1.0,
            base_url: "/assets/".into(),
            manifest: BTreeMap::from([(
                "beep".into(),
                AudioEntry {
                    path: Some(AudioPaths::One("beep.wav".into())),
                    volume: 0.5,
                    looping: false,
                    pool_size: 2,
                    autostart: true,
                    crossfade_duration_ms: 1500,
                },
            )]),
        }
    }

    #[test]
    fn an_authored_cue_sequence_retriggers_only_after_mount_and_uses_the_manifest() {
        let initial = frame(0.0);
        assert!(!plan_audio(None, &initial)
            .iter()
            .any(|action| matches!(action, AudioAction::PlayCue { .. })));
        assert_eq!(
            plan_audio(Some(&initial), &frame(1.0)).last(),
            Some(&AudioAction::PlayCue {
                key: "beep".into(),
                sources: vec!["/assets/beep.wav".into()],
                volume: 0.5,
                looping: false,
                pool_size: 2,
            })
        );
        assert!(plan_audio(Some(&initial), &initial).is_empty());
    }

    #[test]
    fn unlock_plays_only_authored_autostart_entries_when_unmuted() {
        let initial = frame(0.0);
        assert_eq!(plan_audio_unlock(&initial).len(), 1);
        let mut muted = initial;
        muted.muted = true;
        assert!(plan_audio_unlock(&muted).is_empty());
    }

    #[test]
    fn action_wire_names_match_both_mobile_adapters() {
        let action = plan_audio_unlock(&frame(0.0)).remove(0);
        let wire = serde_json::to_value(action).unwrap();
        assert_eq!(wire["kind"], "playCue");
        assert_eq!(wire["poolSize"], 2);
        assert!(wire.get("pool_size").is_none());
    }
}
