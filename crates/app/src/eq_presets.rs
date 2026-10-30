//! Named EQ settings to pick from in a channel's EQ: built-in ones grouped
//! by what they're for (gaming, chat, your mic, media, music), plus ones you
//! save yourself. Every channel can use any of them; its own group is
//! listed first.
//!
//! The built-ins are written as gains for five bands at 100 Hz, 350 Hz,
//! 1 kHz, 3.5 kHz and 10 kHz, and set as the ten bands that sound the same
//! (see [`common::bands_from_five`]). They are starting points in the spirit of common curves
//! (presence around 3.5 kHz for footsteps and speech, less low end for
//! boomy mics, fuller lows for explosions and hip-hop), kept to boosts of
//! 6 dB or less so loud audio doesn't clip.

use audio_engine::{EqBand, EQ_BANDS};
use common::NamedEq;

pub struct BuiltIn {
    pub name: &'static str,
    gains_db: [f32; 5],
}

impl BuiltIn {
    pub fn bands(&self) -> [EqBand; EQ_BANDS] {
        common::bands_from_five(self.gains_db)
    }
}

const fn preset(name: &'static str, gains_db: [f32; 5]) -> BuiltIn {
    BuiltIn { name, gains_db }
}

/// A group of presets shown under one heading.
pub struct Category {
    pub name: &'static str,
    pub presets: &'static [BuiltIn],
}

pub const FLAT: BuiltIn = preset("Flat", [0.0; 5]);

pub const GAMING: Category = Category {
    name: "Gaming",
    presets: &[
        preset("Footsteps", [-3.0, -1.0, 2.0, 5.0, 3.0]),
        preset("Tactical Shooter", [-4.0, -2.0, 1.5, 5.0, 2.5]),
        preset("Competitive FPS", [-2.0, -1.5, 1.0, 4.5, 3.0]),
        preset("Battle Royale", [-1.0, -1.0, 2.0, 4.0, 2.0]),
        preset("Immersive", [3.0, 0.0, -1.0, 2.0, 3.0]),
        preset("Explosions / Cinematic", [5.0, 1.0, 0.0, 1.0, 2.0]),
        preset("Horror", [4.0, -1.0, -1.0, 3.0, 2.0]),
        preset("RPG / Adventure", [2.0, 0.0, 1.0, 1.5, 2.0]),
        preset("MOBA / Strategy", [0.0, 0.0, 2.0, 2.5, 1.0]),
        preset("Racing", [4.0, 2.0, 0.0, 1.0, 1.0]),
        preset("Retro / Chiptune", [-2.0, 0.0, 2.0, 1.0, -2.0]),
    ],
};

/// For other people's voices (the Chat channel).
pub const CHAT: Category = Category {
    name: "Chat",
    presets: &[
        preset("Voice Clarity", [-2.0, -1.0, 2.0, 4.0, 2.0]),
        preset("Crisp Voices", [-3.0, -1.0, 1.0, 3.5, 3.0]),
        preset("Warm Voices", [2.0, 1.5, 0.0, 0.0, -1.0]),
        preset("Quiet Talkers", [-1.0, 0.0, 3.0, 4.0, 1.0]),
        preset("Fix Boomy Mics", [-5.0, -2.0, 0.0, 1.0, 0.0]),
        preset("Less Harsh", [0.0, 0.0, -1.0, -3.0, -2.0]),
        preset("Broadcast", [1.0, -1.0, 1.5, 2.5, 1.0]),
    ],
};

/// For your own voice (the Mic channel).
pub const MIC: Category = Category {
    name: "Mic",
    presets: &[
        preset("Radio Voice", [2.0, -1.0, 1.0, 3.0, 2.0]),
        preset("Clear & Bright", [-2.0, -1.0, 1.0, 3.0, 4.0]),
        preset("Warm & Full", [3.0, 1.0, 0.0, 1.0, 0.0]),
        preset("Deep Voice", [4.0, 2.0, 0.0, -1.0, -1.0]),
        preset("Headset Mic Boost", [-1.0, 0.0, 2.0, 4.0, 2.0]),
        preset("Less Boomy", [-4.0, -2.0, 0.0, 1.0, 1.0]),
        preset("Less Hiss / S Sounds", [0.0, 0.0, 0.0, -2.0, -4.0]),
    ],
};

/// For videos, films and streams (the Media channel).
pub const MEDIA: Category = Category {
    name: "Media",
    presets: &[
        preset("Movie / Cinematic", [4.0, 0.0, 0.0, 1.5, 2.0]),
        preset("Dialogue Boost", [-2.0, -1.0, 3.0, 3.0, 0.0]),
        preset("Videos / Podcasts", [-1.0, 0.0, 2.0, 2.5, 1.0]),
        preset("Streams", [1.0, 0.0, 1.0, 2.0, 1.5]),
        preset("Anime", [2.0, 0.0, 1.0, 2.5, 2.0]),
        preset("Night Mode", [-4.0, -1.0, 1.0, 1.0, -1.0]),
    ],
};

pub const MUSIC: Category = Category {
    name: "Music",
    presets: &[
        preset("Pop", [1.5, -0.5, 1.0, 2.5, 2.0]),
        preset("Rap / Hip-Hop", [5.0, 1.0, -1.0, 1.5, 2.0]),
        preset("R&B", [4.0, 1.5, -1.0, 1.5, 2.5]),
        preset("Jazz", [2.5, 1.0, -1.0, 1.5, 3.0]),
        preset("Rock", [4.0, -1.5, -1.0, 2.5, 3.5]),
        preset("Metal", [4.0, -2.0, 0.5, 3.0, 3.0]),
        preset("Electronic / EDM", [5.5, 0.5, -1.5, 1.5, 4.0]),
        preset("Classical", [3.0, 0.0, -1.0, 1.0, 3.5]),
        preset("Acoustic", [2.0, 1.5, 0.5, 2.0, 2.5]),
        preset("Country", [1.5, 0.5, 1.0, 3.0, 2.0]),
        preset("Latin / Reggaeton", [4.5, 0.5, -0.5, 1.5, 2.5]),
        preset("Lo-fi", [3.0, 2.0, 0.0, -1.5, -3.0]),
    ],
};

pub const GENERAL: Category = Category {
    name: "General",
    presets: &[
        preset("Bass Boost", [6.0, 2.0, 0.0, 0.0, 0.0]),
        preset("Treble Boost", [0.0, 0.0, 0.0, 3.0, 6.0]),
        preset("Late Night", [-4.0, -1.0, 0.0, 1.0, 0.0]),
    ],
};

const ALL: [&Category; 6] = [&GAMING, &CHAT, &MIC, &MEDIA, &MUSIC, &GENERAL];

/// Every group, the one for `bus` first (music right after media, since
/// music often plays on the Media channel).
pub fn categories_for(bus: &str) -> Vec<&'static Category> {
    let first: &[&Category] = match bus {
        "Game" => &[&GAMING],
        "Chat" => &[&CHAT],
        "Mic" => &[&MIC],
        "Media" => &[&MEDIA, &MUSIC],
        _ => &[&MUSIC],
    };
    first.iter().copied().chain(ALL.into_iter().filter(|c| !first.iter().any(|f| f.name == c.name))).collect()
}

/// Whether two EQs sound the same: every band that shapes the sound is in
/// both, alike.
fn same(a: &[EqBand; EQ_BANDS], b: &[EqBand; EQ_BANDS]) -> bool {
    let shaping = |bands: &[EqBand; EQ_BANDS]| bands.iter().filter(|band| band.shapes()).copied().collect::<Vec<_>>();
    let (a, b) = (shaping(a), shaping(b));
    a.len() == b.len()
        && a.iter().all(|x| {
            b.iter().any(|y| {
                x.kind == y.kind
                    && (x.freq / y.freq - 1.0).abs() < 0.005
                    && (x.gain_db - y.gain_db).abs() < 0.05
                    && (x.q - y.q).abs() < 0.01
            })
        })
}

/// The name of the preset `bands` matches (yours first), if any.
pub fn matching<'a>(bands: &[EqBand; EQ_BANDS], custom: &'a [NamedEq]) -> Option<&'a str> {
    if let Some(preset) = custom.iter().find(|p| same(&p.bands, bands)) {
        return Some(&preset.name);
    }
    std::iter::once(&FLAT).chain(ALL.iter().flat_map(|c| c.presets)).find(|p| same(&p.bands(), bands)).map(|p| p.name)
}

#[cfg(test)]
mod tests {
    use audio_engine::EQ_GAIN_RANGE_DB;

    use super::*;

    fn all() -> Vec<&'static BuiltIn> {
        std::iter::once(&FLAT).chain(ALL.iter().flat_map(|c| c.presets)).collect()
    }

    #[test]
    fn built_ins_fit_the_eq_and_dont_clip() {
        for preset in all() {
            assert!(preset.gains_db.iter().all(|g| g.abs() <= EQ_GAIN_RANGE_DB), "{}", preset.name);
            assert!(preset.gains_db.iter().all(|&g| g <= 6.0), "{} boosts more than 6 dB", preset.name);
        }
    }

    #[test]
    fn names_and_curves_are_unique_so_the_menu_shows_the_right_one() {
        let all = all();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a.name, b.name);
                assert!(!same(&a.bands(), &b.bands()), "{} and {} have the same curve", a.name, b.name);
            }
        }
    }

    #[test]
    fn each_channel_lists_its_own_presets_first_and_every_group_once() {
        assert_eq!(categories_for("Game")[0].name, "Gaming");
        assert_eq!(categories_for("Mic")[0].name, "Mic");
        let media: Vec<&str> = categories_for("Media").iter().map(|c| c.name).collect();
        assert_eq!(media[..2], ["Media", "Music"]);
        for bus in ["Game", "Chat", "Mic", "Media", "Aux"] {
            assert_eq!(categories_for(bus).len(), ALL.len(), "{bus}");
        }
    }

    #[test]
    fn your_presets_win_over_built_ins_with_the_same_curve() {
        let jazz = MUSIC.presets.iter().find(|p| p.name == "Jazz").unwrap();
        let custom = vec![NamedEq { name: "Mine".into(), bands: jazz.bands() }];
        assert_eq!(matching(&jazz.bands(), &custom), Some("Mine"));
        assert_eq!(matching(&jazz.bands(), &[]), Some("Jazz"));
        let mut moved = jazz.bands();
        moved[0].gain_db += 0.5;
        assert_eq!(matching(&moved, &[]), None);
        assert_eq!(matching(&audio_engine::DEFAULT_EQ_BANDS, &[]), Some("Flat"), "flat however its bands sit");
    }
}
