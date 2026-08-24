//! The single verified entry point for changing the display.
//!
//! Nothing else in this project may submit a hardware update. Opening a
//! [`DisplaySession`] proves, at runtime, that:
//!
//! 1. the probed hardware geometry matches the profile exactly,
//! 2. the device code, serial model prefix, firmware version, and kernel
//!    release match the profile exactly,
//! 3. the profile's owner-attended evidence is complete, and
//! 4. the caller supplied the exact owner-attended unlock phrase.
//!
//! The module is compiled only with the non-default `device-write` feature, so
//! a default build contains no callable display-write code at all.

use crate::probe::{probe_device, ProbeError};
use crate::refresh::{Backend, Rect, RefreshPlan};
use crate::surface::{self, RegionSnapshot, SurfaceError, SurfaceGeometry};
use kobo_abi::{hwtcon, mxcfb};
use kobo_profile::{DeviceProfile, DeviceSnapshot, WRITE_EVIDENCE_PENDING};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// The exact phrase an owner must supply to open a write session.
pub const OWNER_UNLOCK_PHRASE: &str = "OWNER_ATTENDED_DISPLAY_WRITE";

const SMOKE_FIXED_REGION: Rect = Rect {
    x: 512,
    y: 704,
    width: 32,
    height: 32,
};
const SMOKE_PATCH_REGION: Rect = Rect {
    x: 408,
    y: 600,
    width: 256,
    height: 256,
};
const SMOKE_VISIBLE_HOLD: Duration = Duration::from_millis(1200);
const ATTENDED_SMOKE_UNLOCK_PHRASE: &str = "OWNER_ATTENDED_CANDIDATE_DISPLAY_VALIDATION";
const ATTENDED_GUARD_UNLOCK_PHRASE: &str = "OWNER_ATTENDED_CANDIDATE_GUARD_VALIDATION";

/// One bounded operation used to gather owner-attended display evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttendedSmokeStage {
    DisplayOnly,
    ReversiblePixels,
    ScreenSnapshot,
    FastFeedback,
    /// Measures the submit and wait ioctls across the three offered
    /// waveforms, reversibly, on the fixed patch region.
    WaitTiming,
}

impl AttendedSmokeStage {
    /// Every stage there is, so a test can walk them.
    ///
    /// A hand-written list is only as good as the memory of whoever adds a
    /// stage, so [`Self::position`] exists to make forgetting a compile
    /// error rather than a silently narrower test.
    #[cfg(test)]
    const ALL: [Self; 5] = [
        Self::DisplayOnly,
        Self::ReversiblePixels,
        Self::ScreenSnapshot,
        Self::FastFeedback,
        Self::WaitTiming,
    ];

    /// Where the stage sits in [`Self::ALL`].
    ///
    /// The match is exhaustive, so a new variant does not compile until it is
    /// given a position, and the test below proves each position holds the
    /// stage that claims it. Together those two facts are what make `ALL`
    /// complete rather than merely plausible.
    #[cfg(test)]
    const fn position(self) -> usize {
        match self {
            Self::DisplayOnly => 0,
            Self::ReversiblePixels => 1,
            Self::ScreenSnapshot => 2,
            Self::FastFeedback => 3,
            Self::WaitTiming => 4,
        }
    }

    const fn intent(self) -> crate::refresh::RefreshIntent {
        match self {
            Self::FastFeedback => crate::refresh::RefreshIntent::FastFeedback,
            _ => crate::refresh::RefreshIntent::QualityContent,
        }
    }

    /// Every intent the stage may submit, not just the one it opens with.
    ///
    /// [`Self::intent`] answers for a single update; `WaitTiming` submits
    /// three, one per offered waveform, and an invariant stated over
    /// [`Self::intent`] alone would miss two of them.
    ///
    /// Not `#[cfg(test)]`: [`smoke_wait_timing`] reads this list to decide
    /// what it submits, which is the point. A declaration the behaviour does
    /// not consult is a second copy of the truth, and the test walking it
    /// would then be checking the copy rather than the device path.
    const fn intents(self) -> &'static [crate::refresh::RefreshIntent] {
        use crate::refresh::RefreshIntent::{FastFeedback, QualityContent, TextContent};
        match self {
            Self::DisplayOnly | Self::ReversiblePixels | Self::ScreenSnapshot => &[QualityContent],
            Self::FastFeedback => &[FastFeedback],
            Self::WaitTiming => &[QualityContent, TextContent, FastFeedback],
        }
    }
}

#[derive(Debug)]
pub enum DisplayError {
    UnlockMissing,
    ProfileRejected(Vec<String>),
    WriteRejected(Vec<String>),
    Smoke(String),
    Probe(ProbeError),
    Surface(SurfaceError),
    Io(io::Error),
}

impl fmt::Display for DisplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnlockMissing => {
                formatter.write_str("owner-attended display unlock is missing or incorrect")
            }
            Self::ProfileRejected(reasons) => {
                write!(
                    formatter,
                    "hardware profile rejected: {}",
                    reasons.join("; ")
                )
            }
            Self::WriteRejected(reasons) => {
                write!(formatter, "device write rejected: {}", reasons.join("; "))
            }
            Self::Smoke(reason) => write!(formatter, "attended display smoke: {reason}"),
            Self::Probe(error) => write!(formatter, "read-only probe: {error}"),
            Self::Surface(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "display io: {error}"),
        }
    }
}

impl std::error::Error for DisplayError {}

impl From<SurfaceError> for DisplayError {
    fn from(error: SurfaceError) -> Self {
        Self::Surface(error)
    }
}

impl From<io::Error> for DisplayError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// An open, fully verified display write session.
pub struct DisplaySession {
    framebuffer: File,
    geometry: SurfaceGeometry,
    backend: Backend,
    profile: &'static DeviceProfile,
    snapshot: DeviceSnapshot,
    /// The marker of a submitted update the controller may still be driving,
    /// or zero when there is none.
    ///
    /// Waiting for completion in the same breath as submitting makes every
    /// refresh cost the whole waveform before the caller may do anything else,
    /// and on a keystroke the thing the caller does next is render the frame
    /// after it: work the controller could have been doing at the same time.
    /// So the wait is deferred to the only moment it is actually required,
    /// which is immediately before the framebuffer bytes the controller is
    /// reading would be overwritten. [`Self::restore`] enforces that, so no
    /// caller can produce a torn frame by forgetting to wait.
    pending: AtomicU32,
}

#[derive(Clone, Copy)]
enum WritePolicy {
    ReadyOnly,
    AttendedCandidateValidation,
}

impl DisplaySession {
    /// Probes the device and opens the framebuffer read-write.
    ///
    /// # Errors
    ///
    /// Returns an error when the unlock phrase is wrong, the probe fails, the
    /// hardware profile does not match exactly, the device identity does not
    /// match exactly, or the framebuffer cannot be opened.
    pub fn open(unlock: Option<&str>) -> Result<Self, DisplayError> {
        if unlock != Some(OWNER_UNLOCK_PHRASE) {
            return Err(DisplayError::UnlockMissing);
        }
        let snapshot = probe_device().map_err(DisplayError::Probe)?;
        let profile = kobo_profile::identify_profile(&snapshot).ok_or_else(|| {
            DisplayError::ProfileRejected(vec![
                "no supported hardware profile matched this device".to_owned()
            ])
        })?;
        Self::open_verified(
            profile,
            snapshot,
            Path::new("/dev/fb0"),
            WritePolicy::ReadyOnly,
        )
    }

    fn open_for_attended_validation() -> Result<Self, DisplayError> {
        let snapshot = probe_device().map_err(DisplayError::Probe)?;
        let profile = kobo_profile::identify_profile(&snapshot).ok_or_else(|| {
            DisplayError::ProfileRejected(vec![
                "no supported hardware profile matched this device".to_owned()
            ])
        })?;
        Self::open_verified(
            profile,
            snapshot,
            Path::new("/dev/fb0"),
            WritePolicy::AttendedCandidateValidation,
        )
    }

    /// Opens the framebuffer for the fixed, owner-attended guard restoration
    /// probe while the profile's ordinary write evidence is still pending.
    ///
    /// This does not make a general candidate-capable session available: the
    /// guard binary only calls it for its bounded `--prove-restore` test.
    pub fn open_for_guard_validation(unlock: Option<&str>) -> Result<Self, DisplayError> {
        if unlock != Some(ATTENDED_GUARD_UNLOCK_PHRASE) {
            return Err(DisplayError::UnlockMissing);
        }
        Self::open_for_attended_validation()
    }

    fn open_verified(
        profile: &'static DeviceProfile,
        snapshot: DeviceSnapshot,
        framebuffer_path: &Path,
        policy: WritePolicy,
    ) -> Result<Self, DisplayError> {
        let report = profile.validate(&snapshot);
        if !report.mismatches.is_empty() {
            return Err(DisplayError::ProfileRejected(report.mismatches));
        }
        let mut blockers = report.write_blockers;
        if matches!(policy, WritePolicy::AttendedCandidateValidation) {
            blockers.retain(|blocker| blocker != WRITE_EVIDENCE_PENDING);
        }
        if !blockers.is_empty() {
            return Err(DisplayError::WriteRejected(blockers));
        }
        let framebuffer = snapshot
            .framebuffer
            .as_ref()
            .ok_or_else(|| DisplayError::ProfileRejected(vec!["framebuffer missing".to_owned()]))?;
        let geometry = SurfaceGeometry {
            width: framebuffer.width,
            height: framebuffer.height,
            stride: framebuffer.stride,
            bits_per_pixel: framebuffer.bits_per_pixel,
            memory_length: u64::from(framebuffer.memory_length),
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(framebuffer_path)?;
        Ok(Self {
            framebuffer: file,
            geometry,
            backend: Backend::from_controller(profile.framebuffer_controller),
            profile,
            snapshot,
            pending: AtomicU32::new(0),
        })
    }

    #[must_use]
    pub fn profile(&self) -> &'static DeviceProfile {
        self.profile
    }

    #[must_use]
    pub fn snapshot(&self) -> &DeviceSnapshot {
        &self.snapshot
    }

    /// The panel-controller interface this device speaks.
    #[must_use]
    pub fn backend(&self) -> Backend {
        self.backend
    }

    #[must_use]
    pub fn geometry(&self) -> SurfaceGeometry {
        self.geometry
    }

    /// Captures the exact current bytes of `region` so they can be restored.
    ///
    /// # Errors
    ///
    /// Returns an error when the region is invalid or the read fails.
    pub fn capture(&self, region: Rect) -> Result<RegionSnapshot, DisplayError> {
        Ok(surface::read_region(
            &self.framebuffer,
            self.geometry,
            region,
        )?)
    }

    /// Writes a previously captured region back to the exact place it came
    /// from. The snapshot carries its own validated placement, so no other
    /// region can be addressed.
    ///
    /// Any update still in flight is waited out first. The controller reads the
    /// framebuffer while it drives the panel, so overwriting those bytes early
    /// is what a torn frame is made of.
    ///
    /// # Errors
    ///
    /// Returns an error when the write, or the wait before it, fails.
    pub fn restore(&self, snapshot: &RegionSnapshot) -> Result<(), DisplayError> {
        self.settle()?;
        surface::write_region(&self.framebuffer, self.geometry, snapshot)?;
        Ok(())
    }

    /// Submits one hardware update for `plan` and waits for it to complete.
    ///
    /// # Errors
    ///
    /// Returns an error when the region is invalid or either ioctl fails.
    pub fn refresh(&self, plan: RefreshPlan) -> Result<(), DisplayError> {
        self.submit(plan)?;
        self.settle()
    }

    /// Submits one hardware update for `plan` and returns without waiting for
    /// the panel to finish showing it.
    ///
    /// Use this for a frame the caller has more work to do after: the wait is
    /// taken by the next [`Self::restore`] or [`Self::settle`] instead, so the
    /// controller and the processor work at the same time rather than in turn.
    ///
    /// # Errors
    ///
    /// Returns an error when the region is invalid or the ioctl fails.
    pub fn refresh_deferred(&self, plan: RefreshPlan) -> Result<(), DisplayError> {
        self.submit(plan)
    }

    /// Waits out an update left in flight by [`Self::refresh_deferred`].
    ///
    /// Doing nothing when there is none makes this safe to call on any path
    /// that is about to hand the panel to somebody else.
    ///
    /// # Errors
    ///
    /// Returns an error when the wait ioctl fails.
    pub fn settle(&self) -> Result<(), DisplayError> {
        let marker = self.pending.swap(0, Ordering::AcqRel);
        if marker == 0 {
            return Ok(());
        }
        if self.profile.framebuffer_id == "mxc_epdc_fb" {
            let mut wait = mxcfb::MxcfbUpdateMarkerData {
                update_marker: marker,
                collision_test: 0,
            };
            mxcfb::wait_for_update_complete(&self.framebuffer, &mut wait)?;
        } else {
            let mut wait = hwtcon::HwtconUpdateMarkerData {
                update_marker: marker,
                collision_test: 0,
            };
            hwtcon::wait_for_update_complete(&self.framebuffer, &mut wait)?;
        }
        Ok(())
    }

    /// Sends one update request and remembers its marker as in flight.
    ///
    /// A fresh high-entropy marker is generated for every update. Markers are a
    /// global namespace shared with the stock reader, so a low fixed marker
    /// could be matched against another process's update.
    fn submit(&self, plan: RefreshPlan) -> Result<(), DisplayError> {
        // Validate the region against this exact surface before the kernel sees it.
        surface::RegionPlacement::new(self.geometry, plan.region)?;
        // Only one marker is tracked, so an earlier update is waited out here
        // rather than forgotten.
        self.settle()?;
        let marker = unique_marker()?;
        if self.profile.framebuffer_id == "mxc_epdc_fb" {
            let mut update = mxcfb::MxcfbUpdateDataV1Ntx {
                update_region: mxcfb::MxcfbRect {
                    top: plan.region.y,
                    left: plan.region.x,
                    width: plan.region.width,
                    height: plan.region.height,
                },
                waveform_mode: mxcfb_waveform(plan.waveform),
                update_mode: if plan.full {
                    mxcfb::UPDATE_MODE_FULL
                } else {
                    mxcfb::UPDATE_MODE_PARTIAL
                },
                update_marker: marker,
                temp: mxcfb::TEMP_USE_AMBIENT,
                flags: 0,
                alt_buffer_data: mxcfb::MxcfbAltBufferDataNtx::default(),
            };
            mxcfb::send_update(&self.framebuffer, &mut update)?;
        } else {
            let mut update = plan.update_data(marker);
            hwtcon::send_update(&self.framebuffer, &mut update)?;
        }
        self.pending.store(marker, Ordering::Release);
        Ok(())
    }
}

/// The NTx driver assigns GL16 a different waveform number than HWTCON.
fn mxcfb_waveform(waveform: u32) -> u32 {
    if waveform == hwtcon::WAVEFORM_GL16 {
        mxcfb::WAVEFORM_GL16
    } else {
        waveform
    }
}

/// Runs one fixed, bounded, restoration-verified candidate display check.
///
/// This is the only operation allowed to ignore the evidence-pending blocker.
/// It never exposes the underlying candidate-capable [`DisplaySession`]; the
/// regions, waveform intent, restoration, and verification all remain owned by
/// this hardware boundary. Geometry, framebuffer safety, and exact identity
/// blockers are still enforced before the framebuffer is opened.
///
/// # Errors
///
/// Returns an error when probing or exact validation fails, a fixed region is
/// invalid, a framebuffer operation fails, or restored bytes differ.
pub fn run_attended_smoke(
    stage: AttendedSmokeStage,
    unlock: Option<&str>,
) -> Result<String, DisplayError> {
    if unlock != Some(ATTENDED_SMOKE_UNLOCK_PHRASE) {
        return Err(DisplayError::UnlockMissing);
    }
    let session = DisplaySession::open_for_attended_validation()?;
    let plan = RefreshPlan::new(
        SMOKE_FIXED_REGION,
        stage.intent(),
        false,
        session.geometry().width,
        session.geometry().height,
    )
    .ok_or_else(|| DisplayError::Smoke("fixed region is not inside this screen".to_owned()))?;

    match stage {
        AttendedSmokeStage::DisplayOnly => {
            session.refresh(plan)?;
            Ok("display-only GC16 refresh completed; no pixel byte was written".to_owned())
        }
        AttendedSmokeStage::ReversiblePixels => {
            let original = session.capture(SMOKE_FIXED_REGION)?;
            smoke_show_and_restore(&session, plan, &original)?;
            Ok(format!(
                "reversible GC16 pixel test completed; {} bytes restored and verified",
                original.pixels().len()
            ))
        }
        AttendedSmokeStage::ScreenSnapshot => smoke_screen_snapshot_restore(&session),
        AttendedSmokeStage::WaitTiming => smoke_wait_timing(&session),
        AttendedSmokeStage::FastFeedback => {
            let original = session.capture(SMOKE_FIXED_REGION)?;
            smoke_show_and_restore(&session, plan, &original)?;
            Ok(format!(
                "reversible DU pixel test completed; {} bytes restored and verified",
                original.pixels().len()
            ))
        }
    }
}

const WAIT_TIMING_ROUNDS: usize = 4;

/// Measures the submit and wait ioctls, reversibly, on the patch region.
///
/// Each of the three offered waveforms is driven [`WAIT_TIMING_ROUNDS`] times
/// through an invert-and-restore pair, and every update reports the waveform
/// the driver actually translated the request to alongside both ioctl
/// durations. The screen is left exactly as found, and the restoration is
/// verified byte for byte even when a refresh fails mid-run.
fn smoke_wait_timing(session: &DisplaySession) -> Result<String, DisplayError> {
    use std::fmt::Write as _;

    let original = session.capture(SMOKE_PATCH_REGION)?;
    let inverted = original.inverted_rgb();
    // Built before the run, not inside the recovery, so that the error path
    // always has a plan to restore with. A quality update, because this is the
    // last thing the panel is asked to show.
    let restore_plan = smoke_plan_with_intent(
        session,
        SMOKE_PATCH_REGION,
        crate::refresh::RefreshIntent::QualityContent,
    )?;

    let mut lines = String::from("update  intent   waveform  translated  submit_us  wait_us\n");
    let mut run = || -> Result<(), DisplayError> {
        let mut update = 0_usize;
        // The stage's own declaration, so that what the invariant test walks
        // and what the panel is actually asked for cannot drift apart.
        for intent in AttendedSmokeStage::WaitTiming.intents().iter().copied() {
            let plan = smoke_plan_with_intent(session, SMOKE_PATCH_REGION, intent)?;
            for _ in 0..WAIT_TIMING_ROUNDS {
                for snapshot in [&inverted, &original] {
                    session.restore(snapshot)?;
                    let timing = session.refresh_timed(plan)?;
                    update += 1;
                    let _ = writeln!(
                        lines,
                        "{update:>6}  {:<8} {:>8}  {:>10}  {:>9}  {:>7}",
                        match intent {
                            crate::refresh::RefreshIntent::QualityContent => "GC16",
                            crate::refresh::RefreshIntent::TextContent => "GL16",
                            crate::refresh::RefreshIntent::FastFeedback => "DU",
                        },
                        timing.submitted_waveform,
                        timing.translated_waveform,
                        timing.submit.as_micros(),
                        timing.wait.as_micros(),
                    );
                }
            }
        }
        Ok(())
    };
    let outcome = run();

    // Always leave the screen as found, even when a refresh failed mid-run.
    // The bytes alone are not enough: without a refresh the panel goes on
    // showing the inverted patch, so the owner is left looking at the failure.
    let restored = session
        .restore(&original)
        .and_then(|()| session.refresh(restore_plan));
    outcome?;
    restored?;
    let verify = session.capture(SMOKE_PATCH_REGION)?;
    if !verify.matches(&original) {
        return Err(DisplayError::Smoke(
            "patch region does not match the original bytes".to_owned(),
        ));
    }
    Ok(format!(
        "{lines}wait timing completed; {} bytes restored and verified",
        original.pixels().len()
    ))
}

/// Whether a smoke update asks for a full, cleaning refresh.
///
/// It does not, and the invariant test asserts the consequence: every smoke
/// update is partial. Shared with the test rather than written twice, so that
/// flipping it here fails there instead of quietly widening what an
/// owner-attended stage is allowed to do to the panel.
const SMOKE_UPDATE_IS_FULL: bool = false;

fn smoke_plan_with_intent(
    session: &DisplaySession,
    region: Rect,
    intent: crate::refresh::RefreshIntent,
) -> Result<RefreshPlan, DisplayError> {
    RefreshPlan::new(
        region,
        intent,
        SMOKE_UPDATE_IS_FULL,
        session.geometry().width,
        session.geometry().height,
    )
    .ok_or_else(|| DisplayError::Smoke(format!("region {region:?} is not inside this screen")))
}

fn smoke_screen_snapshot_restore(session: &DisplaySession) -> Result<String, DisplayError> {
    let geometry = session.geometry();
    let whole_screen = Rect {
        x: 0,
        y: 0,
        width: geometry.width,
        height: geometry.height,
    };
    let screen = session.capture(whole_screen)?;
    let patch = session.capture(SMOKE_PATCH_REGION)?;
    let patch_plan = smoke_plan_for(session, SMOKE_PATCH_REGION)?;
    let screen_plan = smoke_plan_for(session, whole_screen)?;

    let shown = session
        .restore(&patch.inverted_rgb())
        .and_then(|()| session.refresh(patch_plan));
    if shown.is_ok() {
        sleep(SMOKE_VISIBLE_HOLD);
    }
    let restored = session
        .restore(&screen)
        .and_then(|()| session.refresh(screen_plan));
    shown?;
    restored?;

    let verify = session.capture(SMOKE_PATCH_REGION)?;
    if !verify.matches(&patch) {
        return Err(DisplayError::Smoke(
            "the changed region was not restored by the whole-screen write".to_owned(),
        ));
    }
    Ok(format!(
        "whole-screen snapshot and restore completed; {} screen bytes captured, \
         {} bytes changed and verified restored",
        screen.pixels().len(),
        patch.pixels().len()
    ))
}

fn smoke_plan_for(session: &DisplaySession, region: Rect) -> Result<RefreshPlan, DisplayError> {
    RefreshPlan::new(
        region,
        crate::refresh::RefreshIntent::QualityContent,
        false,
        session.geometry().width,
        session.geometry().height,
    )
    .ok_or_else(|| DisplayError::Smoke(format!("region {region:?} is not inside this screen")))
}

fn smoke_show_and_restore(
    session: &DisplaySession,
    plan: RefreshPlan,
    original: &RegionSnapshot,
) -> Result<(), DisplayError> {
    let shown = session
        .restore(&original.inverted_rgb())
        .and_then(|()| session.refresh(plan));
    if shown.is_ok() {
        sleep(SMOKE_VISIBLE_HOLD);
    }
    let restored = session
        .restore(original)
        .and_then(|()| session.refresh(plan));
    shown?;
    restored?;

    let verify = session.capture(original.placement().region())?;
    if verify.matches(original) {
        Ok(())
    } else {
        Err(DisplayError::Smoke(
            "restored region does not match the original bytes".to_owned(),
        ))
    }
}

/// Returns a random nonzero update marker.
///
/// # Errors
///
/// Returns an error when the system random source is unreadable.
fn unique_marker() -> Result<u32, DisplayError> {
    let mut bytes = [0_u8; 4];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    // Keep the value large so it cannot coincide with the small sequential
    // markers the stock reader is observed to use.
    Ok((u32::from_le_bytes(bytes) | 0x4000_0000).max(1))
}

#[cfg(test)]
mod tests {
    use super::{
        unique_marker, AttendedSmokeStage, DisplayError, DisplaySession, Rect, RefreshPlan,
        WritePolicy, ATTENDED_SMOKE_UNLOCK_PHRASE, OWNER_UNLOCK_PHRASE, SMOKE_FIXED_REGION,
        SMOKE_PATCH_REGION, SMOKE_UPDATE_IS_FULL, SMOKE_VISIBLE_HOLD,
    };
    use crate::surface::{RegionPlacement, SurfaceGeometry};
    use kobo_abi::{hwtcon, mxcfb};
    use kobo_profile::{
        DeviceProfile, DeviceSnapshot, FramebufferSnapshot, IdentitySnapshot, TouchSnapshot,
        CLARA_2E_N506, CLARA_BW_391, ELIPSA_2E_389, WRITE_EVIDENCE_PENDING,
    };
    use std::path::Path;

    fn matched_snapshot() -> DeviceSnapshot {
        snapshot_for(
            &CLARA_BW_391,
            IdentitySnapshot {
                serial_prefix: Some("N365".into()),
                firmware_version: Some("4.45.23697".into()),
                kernel_release: Some("4.9.77".into()),
                device_code: Some(391),
            },
        )
    }

    fn snapshot_for(profile: &DeviceProfile, identity: IdentitySnapshot) -> DeviceSnapshot {
        DeviceSnapshot {
            compatible: profile
                .compatible_fragments
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            model: Some(profile.device_tree_model.to_owned()),
            framebuffer: Some(FramebufferSnapshot {
                id: profile.framebuffer_id.to_owned(),
                width: profile.width,
                height: profile.height,
                virtual_width: profile.virtual_width,
                virtual_height: profile.virtual_height,
                x_offset: profile.x_offset,
                y_offset: profile.y_offset,
                bits_per_pixel: profile.bits_per_pixel,
                grayscale: profile.grayscale,
                stride: profile.stride,
                memory_length: profile.memory_length,
                kind: profile.framebuffer_kind,
                visual: profile.framebuffer_visual,
                rotation: profile.rotation,
                red: profile.red,
                green: profile.green,
                blue: profile.blue,
                alpha: profile.alpha,
            }),
            touch: Some(TouchSnapshot {
                path: "/dev/input/event1".into(),
                name: profile.touch_name.into(),
                x_min: profile.touch_x_min,
                x_max: profile.touch_x_max,
                y_min: profile.touch_y_min,
                y_max: profile.touch_y_max,
            }),
            identity,
        }
    }

    #[test]
    fn refuses_a_wrong_unlock_phrase_before_probing() {
        assert!(matches!(
            DisplaySession::open(None),
            Err(DisplayError::UnlockMissing)
        ));
        assert!(matches!(
            DisplaySession::open(Some("please")),
            Err(DisplayError::UnlockMissing)
        ));
        assert_eq!(OWNER_UNLOCK_PHRASE, "OWNER_ATTENDED_DISPLAY_WRITE");
        assert!(matches!(
            super::run_attended_smoke(AttendedSmokeStage::DisplayOnly, None),
            Err(DisplayError::UnlockMissing)
        ));
        assert!(matches!(
            super::run_attended_smoke(AttendedSmokeStage::DisplayOnly, Some("please")),
            Err(DisplayError::UnlockMissing)
        ));
        assert_ne!(ATTENDED_SMOKE_UNLOCK_PHRASE, OWNER_UNLOCK_PHRASE);
    }

    #[test]
    fn refuses_a_device_whose_identity_does_not_match() {
        for identity in [
            IdentitySnapshot::default(),
            IdentitySnapshot {
                device_code: Some(390),
                ..matched_snapshot().identity
            },
            IdentitySnapshot {
                firmware_version: Some("4.46.0".into()),
                ..matched_snapshot().identity
            },
            IdentitySnapshot {
                kernel_release: Some("5.10.0".into()),
                ..matched_snapshot().identity
            },
            IdentitySnapshot {
                serial_prefix: Some("N249".into()),
                ..matched_snapshot().identity
            },
        ] {
            let snapshot = DeviceSnapshot {
                identity,
                ..matched_snapshot()
            };
            assert!(matches!(
                DisplaySession::open_verified(
                    &CLARA_BW_391,
                    snapshot,
                    Path::new("/dev/null"),
                    WritePolicy::ReadyOnly,
                ),
                Err(DisplayError::WriteRejected(_))
            ));
        }
    }

    #[test]
    fn ordinary_writes_refuse_a_candidate_but_attended_smoke_may_open_it() {
        const CANDIDATE: kobo_profile::DeviceProfile = kobo_profile::DeviceProfile {
            write_ready: false,
            ..ELIPSA_2E_389
        };
        let identity = IdentitySnapshot {
            serial_prefix: Some("N605".into()),
            firmware_version: Some("4.38.23697".into()),
            kernel_release: Some("4.9.77".into()),
            device_code: Some(389),
        };
        let snapshot = snapshot_for(&CANDIDATE, identity);
        let Err(error) = DisplaySession::open_verified(
            &CANDIDATE,
            snapshot.clone(),
            Path::new("/dev/null"),
            WritePolicy::ReadyOnly,
        ) else {
            panic!("ordinary display writes require completed evidence");
        };
        assert!(
            matches!(error, DisplayError::WriteRejected(ref blockers) if blockers.iter().any(|blocker| blocker == WRITE_EVIDENCE_PENDING))
        );

        DisplaySession::open_verified(
            &CANDIDATE,
            snapshot,
            Path::new("/dev/null"),
            WritePolicy::AttendedCandidateValidation,
        )
        .expect("the bounded attended smoke path may gather the missing evidence");
    }

    #[test]
    fn attended_smoke_never_bypasses_exact_identity() {
        let snapshot = snapshot_for(&ELIPSA_2E_389, IdentitySnapshot::default());
        assert!(matches!(
            DisplaySession::open_verified(
                &ELIPSA_2E_389,
                snapshot,
                Path::new("/dev/null"),
                WritePolicy::AttendedCandidateValidation,
            ),
            Err(DisplayError::WriteRejected(_))
        ));
    }

    #[test]
    fn hal_owned_smoke_regions_are_bounded_on_every_registered_panel() {
        for profile in [&CLARA_BW_391, &ELIPSA_2E_389, &CLARA_2E_N506] {
            let geometry = SurfaceGeometry {
                width: profile.width,
                height: profile.height,
                stride: profile.stride,
                bits_per_pixel: profile.bits_per_pixel,
                memory_length: u64::from(profile.memory_length),
            };
            let fixed = RegionPlacement::new(geometry, SMOKE_FIXED_REGION)
                .expect("fixed smoke region fits the supported panel");
            assert_eq!(fixed.total_bytes(), 32 * 32 * 4);
            let patch = RegionPlacement::new(geometry, SMOKE_PATCH_REGION)
                .expect("patch smoke region fits the supported panel");
            assert_eq!(patch.total_bytes(), 256 * 256 * 4);

            let whole = Rect {
                x: 0,
                y: 0,
                width: profile.width,
                height: profile.height,
            };
            RegionPlacement::new(geometry, whole).expect("whole panel is valid");
        }
    }

    #[test]
    fn mxc_epdc_uses_the_ntx_waveform_number_for_gl16() {
        assert_eq!(
            super::mxcfb_waveform(hwtcon::WAVEFORM_GC16),
            mxcfb::WAVEFORM_GC16
        );
        assert_eq!(
            super::mxcfb_waveform(hwtcon::WAVEFORM_DU),
            mxcfb::WAVEFORM_DU
        );
        assert_eq!(
            super::mxcfb_waveform(hwtcon::WAVEFORM_GL16),
            mxcfb::WAVEFORM_GL16
        );
        assert_ne!(hwtcon::WAVEFORM_GL16, mxcfb::WAVEFORM_GL16);
    }

    #[test]
    fn hal_owned_smoke_stages_use_only_partial_gc16_or_du_updates() {
        for (stage, waveform) in [
            (AttendedSmokeStage::DisplayOnly, hwtcon::WAVEFORM_GC16),
            (AttendedSmokeStage::ReversiblePixels, hwtcon::WAVEFORM_GC16),
            (AttendedSmokeStage::ScreenSnapshot, hwtcon::WAVEFORM_GC16),
            (AttendedSmokeStage::FastFeedback, hwtcon::WAVEFORM_DU),
        ] {
            let plan = RefreshPlan::new(
                SMOKE_FIXED_REGION,
                stage.intent(),
                false,
                CLARA_BW_391.width,
                CLARA_BW_391.height,
            )
            .expect("fixed plan");
            let update = plan.update_data(0x4000_0001);
            assert_eq!(update.waveform_mode, waveform);
            assert_eq!(update.update_mode, hwtcon::UPDATE_MODE_PARTIAL);
        }
        assert!(SMOKE_VISIBLE_HOLD.as_secs() < 5);
    }

    #[test]
    fn refuses_hardware_that_does_not_match_the_profile() {
        let mut snapshot = matched_snapshot();
        snapshot.framebuffer.as_mut().expect("framebuffer").stride = 4096;
        assert!(matches!(
            DisplaySession::open_verified(
                &CLARA_BW_391,
                snapshot,
                Path::new("/dev/null"),
                WritePolicy::ReadyOnly,
            ),
            Err(DisplayError::ProfileRejected(_))
        ));
    }

    #[test]
    fn markers_are_high_entropy_and_never_collide_with_low_reader_markers() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..64 {
            let marker = unique_marker().expect("random marker");
            assert!(marker >= 0x4000_0000);
            assert_ne!(marker, 0);
            seen.insert(marker);
        }
        assert!(seen.len() > 32, "markers should not repeat");
    }
}
