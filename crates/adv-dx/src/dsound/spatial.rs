//! DirectSound 3D: the listener, a source's 3D parameters, and the stereo
//! gains and playback rate they give the source.
//!
//! Defaults, parameter ranges, orientation normalization and deferred
//! settings are as measured on Windows 11. Distance rolloff, cones, panning
//! by the listener's orientation and Doppler shift follow the software 3D
//! model of Wine's dsound (sound3d.c).
use adv_cpu::backend::{Guest, GuestExt};
use std::f32::consts::{FRAC_PI_2, PI};

/// `DS3D_IMMEDIATE` and `DS3D_DEFERRED`.
pub(super) const IMMEDIATE: u32 = 0;
pub(super) const DEFERRED: u32 = 1;
/// `DS3DMODE_DISABLE`.
const DISABLE: u32 = 2;
/// `sizeof(DS3DLISTENER)` and `sizeof(DS3DBUFFER)`.
pub(super) const PARAMETERS: u32 = 64;
const SOUND_SPEED: f32 = 360.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Vector {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vector {
    const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }
    pub(super) fn from_words(words: [u32; 3]) -> Self {
        Self::new(
            f32::from_bits(words[0]),
            f32::from_bits(words[1]),
            f32::from_bits(words[2]),
        )
    }
    fn words(self) -> [u32; 3] {
        [self.x.to_bits(), self.y.to_bits(), self.z.to_bits()]
    }
    pub(super) fn read(cpu: &dyn Guest, at: u32) -> Result<Self, String> {
        let word = |i: u32| cpu.read_u32(at + i * 4).map_err(|e| e.to_string());
        Ok(Self::from_words([word(0)?, word(1)?, word(2)?]))
    }
    pub(super) fn write(self, cpu: &mut dyn Guest, at: u32) -> Result<(), String> {
        for (i, word) in self.words().into_iter().enumerate() {
            cpu.write_u32(at + i as u32 * 4, word)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn scaled(self, factor: f32) -> Self {
        Self::new(self.x * factor, self.y * factor, self.z * factor)
    }
    fn unit(self) -> Option<Self> {
        let length = self.length();
        (length > 0.0 && length.is_finite()).then(|| self.scaled(1.0 / length))
    }
    pub(super) fn is_zero(self) -> bool {
        self == Self::default()
    }
    fn finite_or_zero(self) -> Self {
        if self.x.is_nan() || self.y.is_nan() || self.z.is_nan() {
            Self::default()
        } else {
            self
        }
    }
    fn dot(self, other: Self) -> f32 {
        let product = self.x * other.x + self.y * other.y + self.z * other.z;
        if product.is_nan() { 0.0 } else { product }
    }
    fn cross(self, other: Self) -> Self {
        Self::new(
            self.y * other.z - self.z * other.y,
            self.z * other.x - self.x * other.z,
            self.x * other.y - self.y * other.x,
        )
        .finite_or_zero()
    }
    fn length(self) -> f32 {
        self.dot(self).sqrt()
    }
    /// The vector from `self` to `to`.
    fn to(self, to: Self) -> Self {
        Self::new(to.x - self.x, to.y - self.y, to.z - self.z).finite_or_zero()
    }
    fn angle(self, other: Self) -> f32 {
        let lengths = self.length() * other.length();
        if lengths == 0.0 {
            return 0.0;
        }
        (self.dot(other) / lengths).clamp(-1.0, 1.0).acos()
    }
}

/// `DS3DLISTENER` after its size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Listener {
    pub position: Vector,
    pub velocity: Vector,
    pub front: Vector,
    pub top: Vector,
    pub distance_factor: f32,
    pub rolloff_factor: f32,
    pub doppler_factor: f32,
}

impl Default for Listener {
    fn default() -> Self {
        Self {
            position: Vector::default(),
            velocity: Vector::default(),
            front: Vector::new(0.0, 0.0, 1.0),
            top: Vector::new(0.0, 1.0, 0.0),
            distance_factor: 1.0,
            rolloff_factor: 1.0,
            doppler_factor: 1.0,
        }
    }
}

impl Listener {
    pub(super) fn read(cpu: &dyn Guest, at: u32) -> Result<Self, String> {
        let float = |offset: u32| {
            cpu.read_u32(at + offset)
                .map(f32::from_bits)
                .map_err(|e| e.to_string())
        };
        Ok(Self {
            position: Vector::read(cpu, at + 4)?,
            velocity: Vector::read(cpu, at + 16)?,
            front: Vector::read(cpu, at + 28)?,
            top: Vector::read(cpu, at + 40)?,
            distance_factor: float(52)?,
            rolloff_factor: float(56)?,
            doppler_factor: float(60)?,
        })
    }
    pub(super) fn write(&self, cpu: &mut dyn Guest, at: u32) -> Result<(), String> {
        self.position.write(cpu, at + 4)?;
        self.velocity.write(cpu, at + 16)?;
        self.front.write(cpu, at + 28)?;
        self.top.write(cpu, at + 40)?;
        for (offset, value) in [
            (52, self.distance_factor),
            (56, self.rolloff_factor),
            (60, self.doppler_factor),
        ] {
            cpu.write_u32(at + offset, value.to_bits())
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    /// The listener as `SetAllParameters` stores it, or `None` outside the
    /// ranges it accepts.
    pub(super) fn checked(self) -> Option<Self> {
        let (front, top) = orientation(self.front, self.top)?;
        (self.distance_factor > 0.0
            && (0.0..=10.0).contains(&self.rolloff_factor)
            && (0.0..=10.0).contains(&self.doppler_factor))
        .then_some(Self { front, top, ..self })
    }
}

/// The orientation stored for `front` and `top`: a unit front and the unit
/// part of top perpendicular to it. Zero or parallel vectors are rejected.
pub(super) fn orientation(front: Vector, top: Vector) -> Option<(Vector, Vector)> {
    let front = front.unit()?;
    let along = top.dot(front);
    let top = Vector::new(
        top.x - front.x * along,
        top.y - front.y * along,
        top.z - front.z * along,
    )
    .unit()?;
    Some((front, top))
}

/// `DS3DBUFFER` after its size, with the frequency the application set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Source {
    pub position: Vector,
    pub velocity: Vector,
    pub inside_cone: u32,
    pub outside_cone: u32,
    pub cone: Vector,
    pub cone_outside_volume: i32,
    pub min_distance: f32,
    pub max_distance: f32,
    pub mode: u32,
    pub frequency: u32,
}

impl Source {
    pub(super) fn new(frequency: u32) -> Self {
        Self {
            position: Vector::default(),
            velocity: Vector::default(),
            inside_cone: 360,
            outside_cone: 360,
            cone: Vector::new(0.0, 0.0, 1.0),
            cone_outside_volume: 0,
            min_distance: 1.0,
            max_distance: 1_000_000_000.0,
            mode: 0,
            frequency,
        }
    }
    pub(super) fn read(cpu: &dyn Guest, at: u32, frequency: u32) -> Result<Self, String> {
        let word = |offset: u32| cpu.read_u32(at + offset).map_err(|e| e.to_string());
        Ok(Self {
            position: Vector::read(cpu, at + 4)?,
            velocity: Vector::read(cpu, at + 16)?,
            inside_cone: word(28)?,
            outside_cone: word(32)?,
            cone: Vector::read(cpu, at + 36)?,
            cone_outside_volume: word(48)? as i32,
            min_distance: f32::from_bits(word(52)?),
            max_distance: f32::from_bits(word(56)?),
            mode: word(60)?,
            frequency,
        })
    }
    pub(super) fn write(&self, cpu: &mut dyn Guest, at: u32) -> Result<(), String> {
        self.position.write(cpu, at + 4)?;
        self.velocity.write(cpu, at + 16)?;
        self.cone.write(cpu, at + 36)?;
        for (offset, value) in [
            (28, self.inside_cone),
            (32, self.outside_cone),
            (48, self.cone_outside_volume as u32),
            (52, self.min_distance.to_bits()),
            (56, self.max_distance.to_bits()),
            (60, self.mode),
        ] {
            cpu.write_u32(at + offset, value)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    /// The ranges `IDirectSound3DBuffer` accepts. The maximum distance may
    /// be below the minimum.
    pub(super) fn valid(&self) -> bool {
        self.inside_cone <= self.outside_cone
            && self.outside_cone <= 360
            && !self.cone.is_zero()
            && (-10000..=0).contains(&self.cone_outside_volume)
            && self.min_distance > 0.0
            && self.max_distance > 0.0
            && self.mode <= DISABLE
    }
}

/// Parameters whose deferred changes take effect, and become visible to
/// `Get` calls, when the listener commits them.
#[derive(Clone, Copy, Debug)]
pub(super) struct Deferred<T> {
    pub applied: T,
    pending: Option<T>,
}

impl<T: Copy> Deferred<T> {
    pub(super) fn new(applied: T) -> Self {
        Self {
            applied,
            pending: None,
        }
    }
    /// Changes the parameters now with `IMMEDIATE`, at the next commit with
    /// `DEFERRED`. Returns whether the applied parameters changed.
    pub(super) fn set(&mut self, apply: u32, change: impl Fn(&mut T)) -> bool {
        if apply == DEFERRED {
            change(self.pending.get_or_insert(self.applied));
            return false;
        }
        change(&mut self.applied);
        if let Some(pending) = self.pending.as_mut() {
            change(pending);
        }
        true
    }
    pub(super) fn commit(&mut self) -> bool {
        self.pending
            .take()
            .map(|pending| self.applied = pending)
            .is_some()
    }
}

/// What the mixer plays for a 3D source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Output {
    pub gains: [f32; 2],
    pub rate: u32,
}

/// The output of `source` with buffer volume `volume` (millibels), or `None`
/// when its 3D processing is disabled and it plays as a plain buffer.
pub(super) fn output(
    listener: &Listener,
    source: &Source,
    volume: i32,
    mute_at_max_distance: bool,
) -> Option<Output> {
    let (offset, relative_velocity) = match source.mode {
        DISABLE.. => return None,
        // DS3DMODE_HEADRELATIVE
        1 => (source.position, source.velocity),
        _ => (
            listener.position.to(source.position),
            listener.velocity.to(source.velocity),
        ),
    };
    let mut distance = offset.length();
    if distance > source.max_distance {
        if mute_at_max_distance {
            return Some(Output {
                gains: [0.0; 2],
                rate: source.frequency,
            });
        }
        distance = source.max_distance;
    }
    let distance = distance.max(source.min_distance);
    let distance = source.min_distance + (distance - source.min_distance) * listener.rolloff_factor;
    let ratio = distance / source.min_distance;
    let mut volume = volume as f32 - (ratio * ratio).log10() * 1000.0;

    if !source.cone.is_zero() && source.inside_cone != source.outside_cone {
        let inside = (source.inside_cone / 2) as f32;
        let outside = ((source.outside_cone / 2).max(source.inside_cone / 2 + 1)) as f32;
        let behind = Vector::new(-offset.x, -offset.y, -offset.z);
        let angle = source
            .cone
            .angle(behind)
            .to_degrees()
            .clamp(inside, outside);
        volume += (angle - inside) / (outside - inside) * source.cone_outside_volume as f32;
    }
    let gain = if volume <= -10000.0 {
        0.0
    } else {
        10f32.powf(volume / 2000.0)
    };

    // The source's bearing from the listener, -pi/2 to the left and pi/2 to
    // the right, mixed between speakers at those angles.
    let bearing = if offset.is_zero() {
        0.0
    } else {
        let left = listener.front.cross(listener.top);
        let mut bearing = left.angle(offset);
        if listener.front.angle(offset) > FRAC_PI_2 {
            bearing = -bearing;
        }
        bearing -= FRAC_PI_2;
        if bearing < -PI {
            bearing + 2.0 * PI
        } else {
            bearing
        }
    };
    let gains = if (-FRAC_PI_2..FRAC_PI_2).contains(&bearing) {
        let right = (bearing + FRAC_PI_2) / PI;
        [(1.0 - right).sqrt() * gain, right.sqrt() * gain]
    } else {
        let bearing = if bearing < -FRAC_PI_2 {
            bearing + 2.0 * PI
        } else {
            bearing
        };
        let left = (bearing - FRAC_PI_2) / PI;
        [left.sqrt() * gain, (1.0 - left).sqrt() * gain]
    };

    let mut rate = source.frequency;
    if !(source.velocity.is_zero() && listener.velocity.is_zero())
        && !relative_velocity.is_zero()
        && !offset.is_zero()
    {
        let length = offset.length();
        let toward = if length == 0.0 {
            0.0
        } else {
            relative_velocity.dot(offset) / length
        };
        let speed = (toward * listener.distance_factor * listener.doppler_factor)
            .clamp(-SOUND_SPEED / 2.0, SOUND_SPEED / 2.0);
        let shifted = source.frequency as f32 * (SOUND_SPEED / (SOUND_SPEED + speed));
        rate = (shifted as u32).clamp(100, 200_000);
    }
    Some(Output { gains, rate })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, y: f32, z: f32) -> Source {
        Source {
            position: Vector::new(x, y, z),
            ..Source::new(22050)
        }
    }
    fn gains(source: &Source) -> [f32; 2] {
        output(&Listener::default(), source, 0, false)
            .unwrap()
            .gains
    }
    fn close(actual: [f32; 2], expected: [f32; 2]) -> bool {
        // A bearing of exactly pi/2 is within float rounding of the speaker
        // angle, where the square-root pan law leaves a trace on the other side.
        (actual[0] - expected[0]).abs() < 1e-3 && (actual[1] - expected[1]).abs() < 1e-3
    }

    // spec: DEV-AUDIO-6
    #[test]
    fn sources_pan_by_their_bearing_and_fade_with_distance() {
        let half = 0.5f32.sqrt();
        assert!(close(gains(&at(0.0, 0.0, 0.0)), [half, half]));
        assert!(close(gains(&at(0.0, 0.0, 1.0)), [half, half]));
        assert!(close(gains(&at(0.0, 0.0, -1.0)), [half, half]));
        assert!(close(gains(&at(-1.0, 0.0, 0.0)), [1.0, 0.0]));
        assert!(close(gains(&at(1.0, 0.0, 0.0)), [0.0, 1.0]));
        // Twice the minimum distance is a quarter of the power: -6.02 dB.
        let far = gains(&at(-2.0, 0.0, 0.0));
        assert!((far[0] - 10f32.powf(-602.06 / 2000.0)).abs() < 1e-3);
        let rolloff = Listener {
            rolloff_factor: 0.0,
            ..Listener::default()
        };
        assert!(close(
            output(&rolloff, &at(-2.0, 0.0, 0.0), 0, false)
                .unwrap()
                .gains,
            [1.0, 0.0]
        ));
        let muted = Source {
            max_distance: 1.5,
            ..at(2.0, 0.0, 0.0)
        };
        assert_eq!(output(&rolloff, &muted, 0, true).unwrap().gains, [0.0; 2]);
        let disabled = Source {
            mode: DISABLE,
            ..at(1.0, 0.0, 0.0)
        };
        assert_eq!(output(&rolloff, &disabled, 0, false), None);
    }

    // spec: DEV-AUDIO-6
    #[test]
    fn orientations_normalize_and_deferred_changes_wait_for_commit() {
        let z = Vector::new(0.0, 0.0, 3.0);
        let y = Vector::new(0.0, 1.0, 1.0);
        assert_eq!(
            orientation(z, y),
            Some((Vector::new(0.0, 0.0, 1.0), Vector::new(0.0, 1.0, 0.0)))
        );
        assert_eq!(orientation(Vector::default(), y), None);
        assert_eq!(orientation(z, Vector::new(0.0, 0.0, 2.0)), None);
        let mut listener = Deferred::new(Listener::default());
        assert!(!listener.set(DEFERRED, |l| l.rolloff_factor = 2.0));
        assert!(listener.set(IMMEDIATE, |l| l.doppler_factor = 3.0));
        assert_eq!(listener.applied.rolloff_factor, 1.0);
        assert!(listener.commit());
        assert_eq!(
            (
                listener.applied.rolloff_factor,
                listener.applied.doppler_factor
            ),
            (2.0, 3.0)
        );
        assert!(!listener.commit());
    }

    // spec: DEV-AUDIO-6
    #[test]
    fn approaching_sources_play_higher_and_cones_attenuate_behind() {
        let approaching = Source {
            velocity: Vector::new(0.0, 0.0, -36.0),
            ..at(0.0, 0.0, 10.0)
        };
        let rate = output(&Listener::default(), &approaching, 0, false)
            .unwrap()
            .rate;
        assert_eq!(rate, (22050.0f32 * 360.0 / 324.0) as u32);
        let cone = Source {
            cone: Vector::new(0.0, 0.0, 1.0),
            inside_cone: 90,
            outside_cone: 180,
            cone_outside_volume: -2000,
            ..at(0.0, 0.0, 1.0)
        };
        let facing_away = gains(&cone)[0];
        let facing = gains(&Source {
            cone: Vector::new(0.0, 0.0, -1.0),
            ..cone
        })[0];
        assert!((facing / facing_away - 10.0).abs() < 1e-3);
    }
}
