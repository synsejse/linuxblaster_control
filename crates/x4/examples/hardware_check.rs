//! Explicitly changes supported settings, checks readback, and restores each item.
//! Run only with --exercise-and-restore; close other controllers first.
use soundblaster_x4::{Equalizer, Error, Feature, Output, Port, State, SxfiMode, X4, open};
use std::{error, io::Write};

/// The EQ slots as they were before the exercise.
type Profiles = Vec<(u8, Equalizer)>;

fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}

/// Select, edit and restore each slot, then walk the processing modes and mixer.
fn exercise(
    d: &mut X4<Port>,
    original: &State,
    profiles: &Profiles,
) -> soundblaster_x4::Result<()> {
    for (index, eq) in profiles {
        if *index == 3 {
            d.set_sxfi(SxfiMode::On)?;
        }
        d.set_eq_profile(*index)?;
        let mut changed = eq.clone();
        changed.preamp_db = (changed.preamp_db - 0.5).max(-12.0);
        d.apply_eq(original.output, &changed)?;
        let actual = d.stored_eq_profile(original.output, *index)?;
        if (actual.preamp_db - changed.preamp_db).abs() > 0.011 {
            return Err(invalid("Stored EQ does not match applied value"));
        }
        d.apply_eq(original.output, eq)?;
        println!("profile {index} select/edit/stored readback/restore passed");
    }
    d.set_sxfi(SxfiMode::Battle)?;
    println!("SXFI Battle passed");
    d.set_sxfi(SxfiMode::Off)?;
    println!("SXFI Off passed");
    d.set_sxfi(SxfiMode::On)?;
    d.set_feature(Feature::DirectMode, true)?;
    if !d.feature(Feature::DirectMode)? {
        return Err(invalid("Direct Mode did not enable"));
    }
    d.set_feature(Feature::DirectMode, false)?;
    d.set_sxfi(SxfiMode::Off)?;
    println!("Direct Mode interaction/restore passed");
    for c in &original.mixer {
        if let (Some(level), Some(range)) = (c.level_db, c.range.as_ref()) {
            d.set_mixer_level(c.index, (level - 1.0).max(range.min_db))?;
            d.set_mixer_level(c.index, level)?;
            println!("mixer {} level/restore passed", c.index);
        }
    }
    Ok(())
}

/// Put everything back, whether or not the exercise finished.
fn restore(
    d: &mut X4<Port>,
    original: &State,
    profiles: &Profiles,
    latent_mode: &[u8],
    mixer_channels: &[Vec<i16>],
) -> soundblaster_x4::Result<()> {
    d.set_feature(Feature::DirectMode, false)?;
    // Restore every saved slot even if the exercise failed between write and readback.
    for (index, eq) in profiles {
        d.set_sxfi(if *index == 3 {
            SxfiMode::On
        } else {
            SxfiMode::Off
        })?;
        d.set_eq_profile(*index)?;
        d.apply_eq(original.output, eq)?;
    }
    let mut restore_mode = vec![0x0c];
    restore_mode.extend_from_slice(&latent_mode[1..5]);
    d.session.set_pending(0x6f, &restore_mode, 0x0c)?;
    d.set_sxfi(SxfiMode::Off)?;
    let active = original
        .active_eq_profile
        .ok_or_else(|| invalid("No active EQ slot to restore"))?;
    d.set_eq_profile(active)?;
    if let Some(eq) = &original.eq {
        d.apply_eq(original.output, eq)?;
    }
    // Processing transitions can reset monitor mutes. Restore the mixer last.
    for c in &original.mixer {
        if let Some(level) = c.level_db {
            d.set_mixer_level(c.index, level)?;
        }
        if let Some(muted) = c.muted
            && c.index != 2
        {
            d.set_mixer_mute(c.index, muted)?;
        }
    }
    d.restore_mixer_channels(mixer_channels)
}

fn main() -> Result<(), Box<dyn error::Error>> {
    if std::env::args().nth(1).as_deref() != Some("--exercise-and-restore") {
        return Err(
            invalid("Requires --exercise-and-restore; this changes live audio settings").into(),
        );
    }
    let mut d = open(None)?;
    let original = d.state()?;
    let mixer_channels = d.capture_mixer_channels()?;
    let latent_mode = d
        .session
        .get(0x6f, &[0x0d], |f| f.payload.first() == Some(&0x0d))?;
    if original.output != Output::Headphone
        || original.feature(Feature::DirectMode)
        || original.sxfi != Some(SxfiMode::Off)
    {
        return Err(
            invalid("Hardware exercise expects headphones, Direct Mode off and SXFI off").into(),
        );
    }
    // Record all EQ slots before any changes.
    let mut profiles = Vec::new();
    for info in &original.eq_profiles {
        profiles.push((
            info.index,
            d.stored_eq_profile(original.output, info.index)?,
        ));
    }
    let backup = serde_json::json!({"state":original,"profiles":profiles,"latent_sxfi_mode":latent_mode,"mixer_channels":mixer_channels});
    let path = std::env::var("X4_CHECK_BACKUP")
        .map_err(|_| invalid("Set X4_CHECK_BACKUP to a new backup file path"))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(serde_json::to_string_pretty(&backup)?.as_bytes())?;
    file.sync_all()?;
    let result = exercise(&mut d, &original, &profiles);
    // Reopen on any uncertain transaction; do not retry the failed requested change.
    if !d.session.usable {
        drop(d);
        d = open(None)?;
    }
    restore(&mut d, &original, &profiles, &latent_mode, &mixer_channels)?;
    println!(
        "restored original active EQ: {}",
        serde_json::to_string(&d.eq()?)?
    );
    Ok(result?)
}
