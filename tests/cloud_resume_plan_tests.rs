use peakbot::cloud::{ForegroundSignal, ResumeStep, resume_steps};

#[test]
fn resumed_event_plans_foreground_then_nudge() {
    let steps = resume_steps(ForegroundSignal::Resumed);
    assert_eq!(
        steps,
        &[ResumeStep::Foreground, ResumeStep::Nudge],
        "Resumed must resume the cloud link (foreground) then nudge the SPA (nudge)"
    );
}

#[test]
fn focused_event_plans_only_foreground() {
    let steps = resume_steps(ForegroundSignal::Focused);
    assert_eq!(
        steps,
        &[ResumeStep::Foreground],
        "Focused must only resume the cloud link (foreground), not nudge"
    );
}
