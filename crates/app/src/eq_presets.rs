//! Named EQ settings to pick from in a channel's EQ: built-in ones for
//! music genres and common uses, plus ones you save yourself.
//!
//! Gains are for the five bands at 100 Hz, 350 Hz, 1 kHz, 3.5 kHz and
//! 10 kHz. The built-ins are starting points in the spirit of common genre
//! curves (fuller lows for hip-hop and EDM, presence and air for vocals and
//! jazz, scooped mids for rock and metal), kept to modest boosts so loud
//! tracks don't clip.

use audio_engine::EQ_BAND_COUNT;
use common::NamedEq;

pub struct BuiltIn {
    pub name: &'static str,
    pub gains_db: [f32; EQ_BAND_COUNT],
}

const fn preset(name: &'static str, gains_db: [f32; EQ_BAND_COUNT]) -> BuiltIn {
    BuiltIn { name, gains_db }
}

pub const FLAT: BuiltIn = preset("Flat", [0.0; EQ_BAND_COUNT]);

pub const MUSIC: &[BuiltIn] = &[
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
];

pub const GENERAL: &[BuiltIn] = &[
    preset("Bass Boost", [6.0, 2.0, 0.0, 0.0, 0.0]),
    preset("Treble Boost", [0.0, 0.0, 0.0, 3.0, 6.0]),
    preset("Vocal Clarity", [-2.0, -1.0, 2.0, 4.0, 2.0]),
    preset("Footsteps", [-3.0, -1.0, 2.0, 5.0, 3.0]),
    preset("Late Night", [-4.0, -1.0, 0.0, 1.0, 0.0]),
];

fn same(a: &[f32; EQ_BAND_COUNT], b: &[f32; EQ_BAND_COUNT]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.05)
}

/// The name of the preset `gains_db` matches (yours first), if any.
pub fn matching<'a>(gains_db: &[f32; EQ_BAND_COUNT], custom: &'a [NamedEq]) -> Option<&'a str> {
    if let Some(preset) = custom.iter().find(|p| same(&p.gains_db, gains_db)) {
        return Some(&preset.name);
    }
    std::iter::once(&FLAT)
        .chain(MUSIC)
        .chain(GENERAL)
        .find(|p| same(&p.gains_db, gains_db))
        .map(|p| p.name)
}

#[cfg(test)]
mod tests {
    use audio_engine::EQ_GAIN_RANGE_DB;

    use super::*;

    #[test]
    fn built_ins_fit_the_eq_and_have_unique_names() {
        let all: Vec<&BuiltIn> = std::iter::once(&FLAT).chain(MUSIC).chain(GENERAL).collect();
        for preset in &all {
            assert!(preset.gains_db.iter().all(|g| g.abs() <= EQ_GAIN_RANGE_DB), "{}", preset.name);
        }
        let mut names: Vec<&str> = all.iter().map(|p| p.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), all.len());
    }

    #[test]
    fn your_presets_win_over_built_ins_with_the_same_curve() {
        let custom = vec![NamedEq { name: "Mine".into(), gains_db: MUSIC[0].gains_db }];
        assert_eq!(matching(&MUSIC[0].gains_db, &custom), Some("Mine"));
        assert_eq!(matching(&MUSIC[0].gains_db, &[]), Some("Pop"));
        assert_eq!(matching(&[0.3, 0.0, 0.0, 0.0, 0.0], &[]), None);
    }
}
